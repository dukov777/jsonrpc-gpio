#!/usr/bin/env python3
"""JSON-RPC 2.0 GPIO validation client (NDJSON over a byte stream).

Each request gets an incrementing integer ``id``; the response ``id`` is
asserted to match so a swapped/stale response is caught. Every read has a
timeout, so a dropped or late response surfaces as an error instead of hanging.

Two transports:

  PTY (host build, no extra deps)::

      python3 host_client.py --spawn ./target/aarch64-apple-darwin/debug/jsonrpc-gpio

    Spawns the host binary attached to a pseudo-terminal in raw mode and talks
    to it over the PTY master — a real serial-like path, validated on the host.

  Serial (real ESP32-S3 hardware, stdlib only)::

      python3 host_client.py --port /dev/cu.usbmodemXXXX --baud 115200

Exit code is 0 on success, 1 on any mismatch/timeout — so CI can gate on it.
"""

from __future__ import annotations

import argparse
import json
import os
import pty
import select
import subprocess
import sys
import termios
import time
import tty


class Timeout(Exception):
    """A response did not arrive within the deadline."""


class PtyTransport:
    """Run a host binary on a raw PTY and exchange bytes over the master end."""

    def __init__(self, argv: list[str]):
        self._master, slave = pty.openpty()
        # Raw mode: no echo, no CR/LF translation, so the byte stream the binary
        # sees (and we read back) is exactly the NDJSON we exchange.
        tty.setraw(slave)
        termios.tcsetattr(self._master, termios.TCSANOW, termios.tcgetattr(self._master))
        self._proc = subprocess.Popen(
            argv, stdin=slave, stdout=slave, stderr=subprocess.DEVNULL, close_fds=True
        )
        os.close(slave)  # parent keeps only the master end
        self._buf = bytearray()

    def write_line(self, data: bytes) -> None:
        os.write(self._master, data)

    def read_line(self, timeout: float) -> bytes:
        while b"\n" not in self._buf:
            ready, _, _ = select.select([self._master], [], [], timeout)
            if not ready:
                raise Timeout("no response within %.1fs" % timeout)
            try:
                chunk = os.read(self._master, 4096)
            except OSError:
                chunk = b""
            if not chunk:
                raise Timeout("transport closed before a full line")
            self._buf.extend(chunk)
        line, _, rest = self._buf.partition(b"\n")
        self._buf = bytearray(rest)
        return bytes(line)

    def close(self) -> None:
        try:
            os.close(self._master)
        except OSError:
            pass
        if self._proc.poll() is None:
            self._proc.terminate()
        self._proc.wait(timeout=2)


class SerialTransport:
    """Raw serial transport for real hardware (stdlib only, no pyserial).

    Opens the tty in raw mode. Setting the baud via termios is a no-op for a
    USB-CDC device (e.g. the S3 USB Serial/JTAG, where baud is nominal) and
    takes effect for a real UART bridge.
    """

    def __init__(self, port: str, baud: int, timeout: float):
        self._fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        tty.setraw(self._fd)
        baud_const = getattr(termios, f"B{baud}", None)
        if baud_const is not None:
            attrs = termios.tcgetattr(self._fd)
            attrs[4] = baud_const  # ispeed
            attrs[5] = baud_const  # ospeed
            termios.tcsetattr(self._fd, termios.TCSANOW, attrs)
        self._buf = bytearray()

    def write_line(self, data: bytes) -> None:
        os.write(self._fd, data)

    def read_line(self, timeout: float) -> bytes:
        while b"\n" not in self._buf:
            ready, _, _ = select.select([self._fd], [], [], timeout)
            if not ready:
                raise Timeout("no response within %.1fs" % timeout)
            try:
                chunk = os.read(self._fd, 4096)
            except OSError:
                chunk = b""
            if not chunk:
                raise Timeout("serial port closed")
            self._buf.extend(chunk)
        line, _, rest = self._buf.partition(b"\n")
        self._buf = bytearray(rest)
        return bytes(line)

    def close(self) -> None:
        os.close(self._fd)


class Client:
    def __init__(self, transport, timeout: float = 2.0):
        self._t = transport
        self._timeout = timeout
        self._next_id = 0

    def call(self, method: str, params: dict):
        self._next_id += 1
        req_id = self._next_id
        request = {"jsonrpc": "2.0", "id": req_id, "method": method, "params": params}
        self._t.write_line((json.dumps(request) + "\n").encode())

        # On real hardware the device's log/boot output shares the same serial
        # endpoint as the JSON responses, so skip any line that isn't a JSON
        # object echoing our id. Bounded by an overall deadline.
        deadline = time.monotonic() + self._timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise Timeout(f"no matching response (id={req_id}) within {self._timeout:.1f}s")
            raw = self._t.read_line(remaining)
            try:
                resp = json.loads(raw)
            except (ValueError, UnicodeDecodeError):
                continue  # log/boot noise, not a JSON-RPC response
            if not isinstance(resp, dict) or resp.get("id") != req_id:
                continue  # a response to some other request, or a notification
            if "error" in resp:
                raise RuntimeError(f"rpc error: {resp['error']}")
            return resp["result"]

    # --- typed convenience wrappers ---

    def gpio_config(self, pin: int, mode: str):
        return self.call("gpio_config", {"pin": pin, "mode": mode})

    def gpio_write(self, pin: int, level: int):
        return self.call("gpio_write", {"pin": pin, "level": level})

    def gpio_read(self, pin: int) -> int:
        return self.call("gpio_read", {"pin": pin})["level"]


def run_validation(client: Client) -> None:
    """Exercise the full RPC surface and assert expected behavior."""
    pin = 2

    assert client.gpio_config(pin, "output") == {"ok": True}
    print(f"  gpio_config(pin={pin}, output) -> ok")

    assert client.gpio_write(pin, 1) == {"ok": True}
    print(f"  gpio_write(pin={pin}, level=1) -> ok")

    level = client.gpio_read(pin)
    assert level == 1, f"expected level 1, got {level}"
    print(f"  gpio_read(pin={pin}) -> {level}")

    # A fresh pin reads 0 by default.
    other = client.gpio_read(7)
    assert other == 0, f"expected default 0, got {other}"
    print(f"  gpio_read(pin=7) -> {other} (default)")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--spawn", metavar="BIN", help="spawn host binary on a PTY")
    src.add_argument("--port", metavar="DEV", help="serial port of real hardware")
    ap.add_argument("--baud", type=int, default=115200)
    ap.add_argument("--timeout", type=float, default=2.0)
    args = ap.parse_args()

    if args.spawn:
        transport = PtyTransport([args.spawn])
    else:
        transport = SerialTransport(args.port, args.baud, args.timeout)

    client = Client(transport, timeout=args.timeout)
    try:
        print("running GPIO RPC validation...")
        run_validation(client)
        print("PASS")
        return 0
    except (AssertionError, RuntimeError, Timeout) as e:
        print(f"FAIL: {e}", file=sys.stderr)
        return 1
    finally:
        transport.close()


if __name__ == "__main__":
    sys.exit(main())
