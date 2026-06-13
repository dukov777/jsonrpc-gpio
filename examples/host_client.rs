//! Rust port of `host_client.py`: a JSON-RPC 2.0 GPIO validation/control client
//! (NDJSON over a serial byte stream).
//!
//! Each request gets an incrementing integer `id`; the response `id` must match,
//! so a swapped/stale response is caught. Every read is bounded by a deadline, so
//! a dropped or late response surfaces as an error instead of hanging. Device
//! log/boot lines share the serial endpoint, so any line that isn't a JSON object
//! echoing our `id` is skipped (and counted for benchmark integrity).
//!
//! Serial only (the Python `--spawn` PTY transport is intentionally omitted).
//!
//! Run (host target):
//!   cargo run --release --example host_client --target aarch64-apple-darwin -- \
//!       --port /dev/cu.usbmodem101 --read 2
//!
//! Exit code is 0 on success, 1 on any mismatch/timeout — so CI can gate on it.

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use serde_json::{json, Value};
use serialport::SerialPort;

const WARMUP: usize = 10;

/// Raw serial transport: one `\n`-terminated line at a time, buffering partial reads.
struct SerialTransport {
    port: Box<dyn SerialPort>,
    buf: Vec<u8>,
}

impl SerialTransport {
    fn open(port: &str, baud: u32, timeout: Duration) -> Result<Self> {
        let port = serialport::new(port, baud)
            .timeout(timeout)
            .open()
            .with_context(|| format!("opening {port}"))?;
        Ok(Self {
            port,
            buf: Vec::new(),
        })
    }

    fn write_line(&mut self, data: &[u8]) -> Result<()> {
        self.port.write_all(data)?;
        self.port.flush()?;
        Ok(())
    }

    /// Pull one `\n`-terminated line, bounded by `deadline`. The `\n` is dropped.
    fn read_line(&mut self, deadline: Instant) -> Result<Vec<u8>> {
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..pos).collect();
                self.buf.drain(..1); // drop the '\n'
                return Ok(line);
            }
            if Instant::now() >= deadline {
                bail!("no response within deadline");
            }
            let mut tmp = [0u8; 256];
            match self.port.read(&mut tmp) {
                Ok(0) => {}
                Ok(k) => self.buf.extend_from_slice(&tmp[..k]),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}

struct Client {
    transport: SerialTransport,
    timeout: Duration,
    next_id: i64,
    /// Non-matching/noise lines skipped (benchmark integrity).
    skipped: usize,
}

impl Client {
    fn new(transport: SerialTransport, timeout: Duration) -> Self {
        Self {
            transport,
            timeout,
            next_id: 0,
            skipped: 0,
        }
    }

    /// Send a request and return its `result`, skipping noise/other-id lines.
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let req_id = self.next_id;
        let request = json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_vec(&request)?;
        line.push(b'\n');
        self.transport.write_line(&line)?;

        let deadline = Instant::now() + self.timeout;
        loop {
            if Instant::now() >= deadline {
                bail!(
                    "no matching response (id={req_id}) within {:.1}s",
                    self.timeout.as_secs_f64()
                );
            }
            let raw = self.transport.read_line(deadline)?;
            let resp: Value = match serde_json::from_slice(&raw) {
                Ok(v) => v,
                Err(_) => {
                    self.skipped += 1; // log/boot noise, not a JSON-RPC response
                    continue;
                }
            };
            if !resp.is_object() || resp.get("id").and_then(Value::as_i64) != Some(req_id) {
                self.skipped += 1; // a response to some other request, or a notification
                continue;
            }
            if let Some(err) = resp.get("error") {
                bail!("rpc error: {err}");
            }
            return resp
                .get("result")
                .cloned()
                .ok_or_else(|| anyhow!("response missing result"));
        }
    }

    // --- typed convenience wrappers ---

    fn gpio_config(&mut self, pin: i64, mode: &str) -> Result<Value> {
        self.call("gpio_config", json!({ "pin": pin, "mode": mode }))
    }

    fn gpio_write(&mut self, pin: i64, level: i64) -> Result<Value> {
        self.call("gpio_write", json!({ "pin": pin, "level": level }))
    }

    fn gpio_read(&mut self, pin: i64) -> Result<i64> {
        let result = self.call("gpio_read", json!({ "pin": pin }))?;
        result
            .get("level")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("gpio_read result missing level: {result}"))
    }

    fn led_set(&mut self, r: i64, g: i64, b: i64) -> Result<Value> {
        self.call("led_set", json!({ "r": r, "g": g, "b": b }))
    }

    /// Blink the LED on/off by toggling led_set. `count=None` blinks until
    /// interrupted; each on and off phase lasts `period`.
    fn blink(
        &mut self,
        r: i64,
        g: i64,
        b: i64,
        period: Duration,
        count: Option<u64>,
        stop: &AtomicBool,
    ) -> Result<()> {
        let mut n = 0u64;
        while count.map_or(true, |c| n < c) && !stop.load(Ordering::Relaxed) {
            self.led_set(r, g, b)?;
            sleep_interruptible(period, stop);
            if stop.load(Ordering::Relaxed) {
                break;
            }
            self.led_set(0, 0, 0)?;
            sleep_interruptible(period, stop);
            n += 1;
        }
        Ok(())
    }

    /// Sweep the LED smoothly through the full hue wheel. Each cycle takes
    /// `duration` (driven by wall-clock, so timing holds regardless of serial
    /// round-trip latency). `cycles=None` runs until interrupted.
    fn rainbow(
        &mut self,
        duration: Duration,
        brightness: i64,
        cycles: Option<u64>,
        stop: &AtomicBool,
    ) -> Result<()> {
        let value = brightness.clamp(0, 255) as f64 / 255.0;
        let dur = duration.as_secs_f64();
        let mut n = 0u64;
        while cycles.map_or(true, |c| n < c) && !stop.load(Ordering::Relaxed) {
            let start = Instant::now();
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let elapsed = start.elapsed().as_secs_f64();
                if elapsed >= dur {
                    break;
                }
                let (r, g, b) = hsv_to_rgb(elapsed / dur, 1.0, value);
                self.led_set(
                    (r * 255.0) as i64,
                    (g * 255.0) as i64,
                    (b * 255.0) as i64,
                )?;
            }
            n += 1;
        }
        Ok(())
    }

    /// Time `n` roundtrips of `method` with `Instant`; return ms samples.
    /// Discards `WARMUP` initial calls (port/USB/first-call jitter).
    fn bench(&mut self, n: usize, method: &str) -> Result<Vec<f64>> {
        let one = |c: &mut Client| -> Result<()> {
            match method {
                "gpio_read" => {
                    c.gpio_read(2)?;
                }
                "led_set" => {
                    c.led_set(0, 0, 0)?;
                }
                _ => {
                    c.call(method, json!({}))?;
                }
            }
            Ok(())
        };
        for _ in 0..WARMUP {
            one(self)?;
        }
        self.skipped = 0;
        let mut samples = Vec::with_capacity(n);
        for _ in 0..n {
            let t0 = Instant::now();
            one(self)?;
            samples.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        Ok(samples)
    }
}

/// Sleep `dur`, waking early in small slices if `stop` is set (so Ctrl-C is responsive).
fn sleep_interruptible(dur: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(Duration::from_millis(20)));
    }
}

/// HSV -> RGB with each component in [0, 1] (mirrors Python `colorsys.hsv_to_rgb`).
fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    if s == 0.0 {
        return (v, v, v);
    }
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match (i as i64).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// Linear-interpolated percentile of an already-sorted slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let k = (sorted.len() - 1) as f64 * p / 100.0;
    let lo = k.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    sorted[lo] + (sorted[hi] - sorted[lo]) * (k - lo as f64)
}

/// Print min/p50/mean/p95/p99/max + req/s and a compact histogram (ms).
fn print_latency_stats(samples: &mut [f64], label: &str, skipped: usize) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let var = samples.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / samples.len() as f64;
    let (min, max) = (samples[0], samples[samples.len() - 1]);
    println!(
        "roundtrip: {label}  ({} samples, {skipped} noise lines skipped)",
        samples.len()
    );
    println!(
        "  min {min:6.2}  p50 {:6.2}  mean {mean:6.2}  p95 {:6.2}  p99 {:6.2}  max {max:6.2}   (ms)   stdev {:.2}",
        percentile(samples, 50.0),
        percentile(samples, 95.0),
        percentile(samples, 99.0),
        var.sqrt(),
    );
    println!("  throughput ~{:.0} req/s", 1000.0 / mean);

    // compact histogram over [min, p99] so a single outlier doesn't flatten it
    let hi = percentile(samples, 99.0);
    let bins = 12usize;
    let width = ((hi - min) / bins as f64).max(1e-9);
    let mut counts = vec![0usize; bins];
    for &v in samples.iter() {
        let idx = (((v - min) / width) as usize).min(bins - 1);
        counts[idx] += 1;
    }
    let peak = *counts.iter().max().unwrap_or(&1).max(&1);
    for (i, &c) in counts.iter().enumerate() {
        let edge = min + i as f64 * width;
        let bar = "#".repeat(40 * c / peak);
        println!("  {edge:6.2} ms |{bar:<40} {c}");
    }
}

/// Exercise the full RPC surface and assert expected behavior.
fn run_validation(client: &mut Client) -> Result<()> {
    let pin = 2;
    let ok = json!({ "ok": true });

    if client.gpio_config(pin, "output")? != ok {
        bail!("gpio_config did not return ok");
    }
    println!("  gpio_config(pin={pin}, output) -> ok");

    if client.gpio_write(pin, 1)? != ok {
        bail!("gpio_write did not return ok");
    }
    println!("  gpio_write(pin={pin}, level=1) -> ok");

    let level = client.gpio_read(pin)?;
    if level != 1 {
        bail!("expected level 1, got {level}");
    }
    println!("  gpio_read(pin={pin}) -> {level}");

    // A fresh pin reads 0 by default.
    let other = client.gpio_read(7)?;
    if other != 0 {
        bail!("expected default 0, got {other}");
    }
    println!("  gpio_read(pin=7) -> {other} (default)");

    Ok(())
}

/// Parse a comma-separated `R,G,B` triple of integers.
fn parse_rgb(s: &str) -> Result<(i64, i64, i64)> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 3 {
        bail!("expected R,G,B, got {s:?}");
    }
    let v: Vec<i64> = parts
        .iter()
        .map(|p| p.trim().parse::<i64>())
        .collect::<Result<_, _>>()
        .with_context(|| format!("invalid R,G,B: {s:?}"))?;
    Ok((v[0], v[1], v[2]))
}

#[derive(Parser)]
#[command(about = "JSON-RPC 2.0 GPIO validation client over serial (Rust port of host_client.py)")]
struct Cli {
    /// Serial port of real hardware, e.g. /dev/cu.usbmodem101
    #[arg(long)]
    port: String,

    #[arg(long, default_value_t = 115200)]
    baud: u32,

    #[arg(long, default_value_t = 2.0)]
    timeout: f64,

    /// Set the on-board LED to this color (0-255 each) and exit; e.g. --led 0,16,0
    #[arg(long, value_name = "R,G,B")]
    led: Option<String>,

    /// Blink the LED this color until Ctrl-C (or --count times); e.g. --blink 0,0,32
    #[arg(long, value_name = "R,G,B")]
    blink: Option<String>,

    /// Blink half-period in seconds
    #[arg(long, default_value_t = 0.3)]
    period: f64,

    /// Sweep the LED through all colors; --duration sets seconds/cycle, --count sets cycles
    #[arg(long)]
    rainbow: bool,

    /// Seconds per full color wheel for --rainbow
    #[arg(long, default_value_t = 5.0)]
    duration: f64,

    /// Max channel brightness 0-255 for --rainbow
    #[arg(long, default_value_t = 40)]
    brightness: i64,

    /// Number of blinks/rainbow cycles (default: until Ctrl-C)
    #[arg(long)]
    count: Option<u64>,

    /// Configure a GPIO pin and exit; MODE = input|output|input_pullup, e.g. --config 45,input_pullup
    #[arg(long, value_name = "PIN,MODE")]
    config: Option<String>,

    /// Write a GPIO pin level (0/1) and exit, e.g. --write 45,1
    #[arg(long, value_name = "PIN,LEVEL")]
    write: Option<String>,

    /// Read a GPIO pin's level and exit
    #[arg(long, value_name = "PIN")]
    read: Option<i64>,

    /// Measure end-to-end RPC roundtrip latency over N requests and exit
    #[arg(long, value_name = "N")]
    bench: Option<usize>,

    /// RPC method to time for --bench (gpio_read = lightest device work)
    #[arg(long, default_value = "gpio_read", value_parser = ["gpio_read", "led_set"])]
    bench_method: String,
}

fn run(cli: &Cli, client: &mut Client, stop: &AtomicBool) -> Result<()> {
    if let Some(n) = cli.bench {
        println!("benchmarking {n}x {} (10 warmup)...", cli.bench_method);
        let mut samples = client.bench(n, &cli.bench_method)?;
        let skipped = client.skipped;
        print_latency_stats(&mut samples, &format!("{n}x {}", cli.bench_method), skipped);
        return Ok(());
    }
    if cli.rainbow {
        let how_many = cli
            .count
            .map_or_else(|| "until Ctrl-C".to_string(), |c| c.to_string());
        println!("rainbow: {}s/cycle, {how_many}", cli.duration);
        client.rainbow(
            Duration::from_secs_f64(cli.duration),
            cli.brightness,
            cli.count,
            stop,
        )?;
        client.led_set(0, 0, 0)?; // leave the LED off
        return Ok(());
    }
    if let Some(spec) = &cli.blink {
        let (r, g, b) = parse_rgb(spec)?;
        let how_many = cli
            .count
            .map_or_else(|| "until Ctrl-C".to_string(), |c| c.to_string());
        println!(
            "blinking ({r},{g},{b}) period={}s, {how_many}",
            cli.period
        );
        client.blink(
            r,
            g,
            b,
            Duration::from_secs_f64(cli.period),
            cli.count,
            stop,
        )?;
        client.led_set(0, 0, 0)?; // leave the LED off
        return Ok(());
    }
    if let Some(spec) = &cli.led {
        let (r, g, b) = parse_rgb(spec)?;
        client.led_set(r, g, b)?;
        println!("led_set({r}, {g}, {b}) -> ok");
        return Ok(());
    }
    if let Some(spec) = &cli.config {
        let (pin_s, mode) = spec
            .split_once(',')
            .ok_or_else(|| anyhow!("expected PIN,MODE, got {spec:?}"))?;
        let pin: i64 = pin_s.trim().parse().context("invalid PIN")?;
        let mode = mode.trim();
        client.gpio_config(pin, mode)?;
        println!("gpio_config(pin={pin}, mode={mode}) -> ok");
        return Ok(());
    }
    if let Some(spec) = &cli.write {
        let (pin_s, level_s) = spec
            .split_once(',')
            .ok_or_else(|| anyhow!("expected PIN,LEVEL, got {spec:?}"))?;
        let pin: i64 = pin_s.trim().parse().context("invalid PIN")?;
        let level: i64 = level_s.trim().parse().context("invalid LEVEL")?;
        client.gpio_write(pin, level)?;
        println!("gpio_write(pin={pin}, level={level}) -> ok");
        return Ok(());
    }
    if let Some(pin) = cli.read {
        let level = client.gpio_read(pin)?;
        println!("gpio_read(pin={pin}) -> {level}");
        return Ok(());
    }
    println!("running GPIO RPC validation...");
    run_validation(client)?;
    println!("PASS");
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        // Best-effort Ctrl-C handler: signal blink/rainbow loops to stop cleanly.
        let _ = ctrlc::set_handler(move || {
            if stop.swap(true, Ordering::Relaxed) {
                std::process::exit(130); // second Ctrl-C: hard exit
            }
            eprintln!("\nstopping");
        });
    }

    let transport = match SerialTransport::open(
        &cli.port,
        cli.baud,
        Duration::from_secs_f64(cli.timeout),
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("FAIL: {e}");
            return ExitCode::from(1);
        }
    };
    let mut client = Client::new(transport, Duration::from_secs_f64(cli.timeout));

    match run(&cli, &mut client, &stop) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("FAIL: {e}");
            ExitCode::from(1)
        }
    }
}
