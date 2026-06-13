//! Rust port of `host_client.py --bench`: measure end-to-end JSON-RPC roundtrip
//! latency against a real device over serial.
//!
//! Sends N `gpio_read` requests one at a time, timing each from just-before the
//! write to just-after the matching response is parsed — the same client-
//! perceived path the Python `--bench` measures. Prints min/p50/mean/p95/p99/max
//! + req/s + a compact histogram, so the two implementations are directly
//! comparable.
//!
//! Run (host target):
//!   cargo run --release --example serial_bench --target aarch64-apple-darwin -- \
//!       /dev/cu.usbmodem101 1000

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serialport::SerialPort;

const WARMUP: usize = 10;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let port_path = args
        .next()
        .unwrap_or_else(|| "/dev/cu.usbmodem101".to_string());
    let n: usize = args.next().map(|s| s.parse()).transpose()?.unwrap_or(1000);

    let mut port = serialport::new(&port_path, 115_200)
        .timeout(Duration::from_secs(8))
        .open()
        .with_context(|| format!("opening {port_path}"))?;

    println!("benchmarking {n}x gpio_read (10 warmup) on {port_path}...");

    let mut buf: Vec<u8> = Vec::new();
    let mut id: i64 = 0;
    let mut skipped = 0usize;

    for _ in 0..WARMUP {
        id += 1;
        call(port.as_mut(), &mut buf, id, &mut skipped)?;
    }

    skipped = 0;
    let mut samples_ms: Vec<f64> = Vec::with_capacity(n);
    for _ in 0..n {
        id += 1;
        let t0 = Instant::now();
        call(port.as_mut(), &mut buf, id, &mut skipped)?;
        samples_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }

    print_stats(&mut samples_ms, n, skipped);
    Ok(())
}

/// Send one `gpio_read(2)` request and wait for the response echoing `id`,
/// skipping any noise lines (device logs share the serial endpoint).
fn call(port: &mut dyn SerialPort, buf: &mut Vec<u8>, id: i64, skipped: &mut usize) -> Result<()> {
    let req = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"gpio_read\",\"params\":{{\"pin\":2}}}}\n"
    );
    port.write_all(req.as_bytes())?;
    port.flush()?;

    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if Instant::now() > deadline {
            bail!("no response for id {id} within 8s");
        }
        let line = read_line(port, buf)?;
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<serde_json::Value>(&line) {
            Ok(v) if v.get("id").and_then(|x| x.as_i64()) == Some(id) => {
                if let Some(err) = v.get("error") {
                    bail!("rpc error: {err}");
                }
                return Ok(());
            }
            _ => *skipped += 1, // noise / other id
        }
    }
}

/// Pull one `\n`-terminated line from the port, buffering partial reads.
fn read_line(port: &mut dyn SerialPort, buf: &mut Vec<u8>) -> Result<Vec<u8>> {
    loop {
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..pos).collect();
            buf.drain(..1); // drop the '\n'
            return Ok(line);
        }
        let mut tmp = [0u8; 256];
        match port.read(&mut tmp) {
            Ok(0) => {}
            Ok(k) => buf.extend_from_slice(&tmp[..k]),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let k = (sorted.len() - 1) as f64 * p / 100.0;
    let lo = k.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    sorted[lo] + (sorted[hi] - sorted[lo]) * (k - lo as f64)
}

fn print_stats(samples: &mut [f64], n: usize, skipped: usize) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let var = samples.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / samples.len() as f64;
    let (min, max) = (samples[0], samples[samples.len() - 1]);
    println!(
        "roundtrip: {n}x gpio_read  ({} samples, {skipped} noise lines skipped)",
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
