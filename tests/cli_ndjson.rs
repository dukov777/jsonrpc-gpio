//! End-to-end host test: pipe NDJSON requests through the built binary over
//! stdin/stdout and assert the NDJSON responses. Exercises the full stack —
//! HostTransport + Framer + dispatch + MockGpio — as a real process.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run the server binary with `input` on stdin, return everything it wrote to
/// stdout. Dropping the stdin handle signals EOF so the server exits.
fn run_server(input: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_jsonrpc-gpio"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server binary");

    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(input.as_bytes())
        .expect("write request to stdin");
    // stdin handle dropped here -> server sees EOF and exits.

    let output = child.wait_with_output().expect("server exits");
    assert!(
        output.status.success(),
        "server exited non-zero: {:?}",
        output.status
    );
    String::from_utf8(output.stdout).expect("stdout is utf8")
}

#[test]
fn write_then_read_roundtrips_over_stdio() {
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"gpio_write","params":{"pin":2,"level":1}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"gpio_read","params":{"pin":2}}"#,
        "\n",
    );
    let out = run_server(input);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "one response per request, got: {out:?}");

    let read_resp: serde_json::Value = serde_json::from_str(lines[1]).expect("valid JSON response");
    assert_eq!(read_resp["id"], 2);
    assert_eq!(read_resp["result"]["level"], 1);
}

#[test]
fn malformed_line_yields_parse_error_null_id() {
    let out = run_server("this is not json\n");
    let resp: serde_json::Value =
        serde_json::from_str(out.lines().next().expect("a response line")).expect("valid JSON");
    assert_eq!(resp["error"]["code"], -32700);
    assert_eq!(resp["id"], serde_json::Value::Null);
}

#[test]
fn two_requests_coalesced_in_one_write_get_two_responses() {
    // Both requests arrive together; the framer must split them.
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":"a","method":"gpio_config","params":{"pin":4,"mode":"output"}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":"b","method":"gpio_read","params":{"pin":4}}"#,
        "\n",
    );
    let out = run_server(input);
    assert_eq!(out.lines().count(), 2);
}
