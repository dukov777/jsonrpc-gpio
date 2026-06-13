//! Poll a GPIO input and mirror its level on the on-board LED.
//!
//! Configures the pin as an input, then reads it in a loop: when the pin is high
//! (1) the LED lights, when it's low (0) the LED is off. Runs until Ctrl-C and
//! always leaves the LED off on exit.
//!
//! Rust port of `watch_pin_led.py`. Self-contained (a minimal JSON-RPC client
//! over serial), matching the repo's example conventions.
//!
//! Run (host target):
//!   cargo run --release --example watch_pin_led --target aarch64-apple-darwin -- \
//!       --port /dev/cu.usbmodem101 --pin 45

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use serde_json::{json, Value};
use serialport::SerialPort;

/// Minimal JSON-RPC 2.0 client over a `\n`-framed serial byte stream.
struct Client {
    port: Box<dyn SerialPort>,
    buf: Vec<u8>,
    timeout: Duration,
    next_id: i64,
}

impl Client {
    fn open(port: &str, baud: u32, timeout: Duration) -> Result<Self> {
        let port = serialport::new(port, baud)
            .timeout(timeout)
            .open()
            .with_context(|| format!("opening {port}"))?;
        Ok(Self {
            port,
            buf: Vec::new(),
            timeout,
            next_id: 0,
        })
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
        self.port.write_all(&line)?;
        self.port.flush()?;

        let deadline = Instant::now() + self.timeout;
        loop {
            if Instant::now() >= deadline {
                bail!(
                    "no matching response (id={req_id}) within {:.1}s",
                    self.timeout.as_secs_f64()
                );
            }
            let raw = self.read_line(deadline)?;
            let resp: Value = match serde_json::from_slice(&raw) {
                Ok(v) => v,
                Err(_) => continue, // log/boot noise, not a JSON-RPC response
            };
            if !resp.is_object() || resp.get("id").and_then(Value::as_i64) != Some(req_id) {
                continue; // a response to some other request, or a notification
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

    fn gpio_config(&mut self, pin: i64, mode: &str) -> Result<Value> {
        self.call("gpio_config", json!({ "pin": pin, "mode": mode }))
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
#[command(about = "Poll a GPIO input and mirror its level on the on-board LED (Rust port of watch_pin_led.py)")]
struct Cli {
    /// Serial port of the board, e.g. /dev/cu.usbmodem101
    #[arg(long)]
    port: String,

    #[arg(long, default_value_t = 115200)]
    baud: u32,

    #[arg(long, default_value_t = 2.0)]
    timeout: f64,

    /// Input pin to watch
    #[arg(long, default_value_t = 45)]
    pin: i64,

    /// LED color when the pin is high (default 0,16,0 = green)
    #[arg(long, value_name = "R,G,B", default_value = "0,16,0")]
    color: String,

    /// Seconds between reads
    #[arg(long, default_value_t = 0.001)]
    interval: f64,
}

fn run(cli: &Cli, client: &mut Client, stop: &AtomicBool) -> Result<()> {
    let (r, g, b) = parse_rgb(&cli.color)?;
    let interval = Duration::from_secs_f64(cli.interval);

    client.gpio_config(cli.pin, "input")?;
    println!(
        "watching pin {}; LED ({r},{g},{b}) when high. Ctrl-C to stop.",
        cli.pin
    );

    let mut last: Option<i64> = None; // only send led_set when the state changes
    while !stop.load(Ordering::Relaxed) {
        let level = client.gpio_read(cli.pin)?;
        if Some(level) != last {
            if level == 1 {
                client.led_set(r, g, b)?;
            } else {
                client.led_set(0, 0, 0)?;
            }
            println!(
                "gpio_read(pin={}) -> {level}  LED {}",
                cli.pin,
                if level == 1 { "ON" } else { "off" }
            );
            last = Some(level);
        }
        // Sleep `interval`, waking early if Ctrl-C arrives.
        let deadline = Instant::now() + interval;
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(remaining.min(Duration::from_millis(20)));
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        let _ = ctrlc::set_handler(move || {
            if stop.swap(true, Ordering::Relaxed) {
                std::process::exit(130); // second Ctrl-C: hard exit
            }
            eprintln!("\nstopping");
        });
    }

    let mut client = match Client::open(
        &cli.port,
        cli.baud,
        Duration::from_secs_f64(cli.timeout),
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: {e}");
            return ExitCode::from(1);
        }
    };

    let result = run(&cli, &mut client, &stop);
    let _ = client.led_set(0, 0, 0); // leave the LED off
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("FAIL: {e}");
            ExitCode::from(1)
        }
    }
}
