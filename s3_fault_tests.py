#!/usr/bin/env python3
"""S3 USB Serial/JTAG fault-injection tests (hardware-in-the-loop).

Exercises the §7 failure modes the firmware's `S3Transport` is designed to
survive, on a real connected ESP32-S3. These cannot run in host CI — they need
a board and manipulate the serial port / reset line.

  1. Boot with no host attached, then attach -> device boots headless without
     wedging (no blocking console/response write) and serves once attached.
  2. Host disconnects mid-session under load -> the device's writes to the
     closed port time out and drop (is_connected guard + finite write timeout);
     the loop never wedges, and the device keeps serving after reconnect.
  3. Task watchdog stays fed -> a long idle hold produces no reset / watchdog
     trigger, and the device is still responsive afterward.

Run:  python3 s3_fault_tests.py --port /dev/cu.usbmodem101 [--elf ELF] [--idle 20]

Exit code 0 iff all scenarios pass.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import time

from host_client import Client, SerialTransport, Timeout

# Strings that indicate the device reset, panicked, or the watchdog fired.
RESET_MARKERS = (
    "rst:",
    "boot:",
    "ESP-ROM",
    "task_wdt",
    "Guru Meditation",
    "Backtrace",
    "abort()",
    "panic",
)


def reset_device(port: str) -> None:
    """Reset the chip via espflash, then release the port."""
    subprocess.run(
        ["espflash", "reset", "--port", port],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def flash_device(port: str, elf: str) -> None:
    subprocess.run(
        ["espflash", "flash", "--port", port, elf],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def serves(port: str, n: int = 5, timeout: float = 6.0) -> bool:
    """Open the port and confirm the device answers `n` requests."""
    t = SerialTransport(port, 115200, timeout)
    c = Client(t, timeout=timeout)
    try:
        for _ in range(n):
            c.gpio_read(2)  # any deterministic response
        return True
    except (Timeout, RuntimeError):
        return False
    finally:
        t.close()


def monitor(port: str, secs: float) -> list:
    """Collect any lines the device emits over `secs` (port held open)."""
    t = SerialTransport(port, 115200, 0.5)
    lines = []
    end = time.monotonic() + secs
    try:
        while time.monotonic() < end:
            try:
                line = t.read_line(min(0.5, max(0.01, end - time.monotonic())))
                if line:
                    lines.append(line.decode("utf-8", "replace"))
            except Timeout:
                pass
    finally:
        t.close()
    return lines


# --- scenarios -------------------------------------------------------------


def scenario_boot_without_host(port: str) -> bool:
    print("1) boot with no host attached, then attach")
    reset_device(port)  # espflash resets and releases the port
    print("   reset; idling 6s with NO host attached (device boots headless)...")
    time.sleep(6.0)  # device runs with is_connected() == false the whole time
    ok = serves(port)
    print(f"   attached and queried -> {'serves' if ok else 'NO RESPONSE (wedged?)'}")
    return ok


def scenario_disconnect_under_load(port: str, cycles: int = 3) -> bool:
    print("2) host disconnects mid-session under load")
    # Baseline.
    if not serves(port):
        print("   baseline failed")
        return False
    for i in range(cycles):
        # Fire requests write-only, then yank the port so responses are in
        # flight when the host vanishes -> device must time out the write.
        t = SerialTransport(port, 115200, 6.0)
        c = Client(t, timeout=6.0)
        for _ in range(10):
            req = (
                '{"jsonrpc":"2.0","id":1,"method":"gpio_read","params":{"pin":2}}\n'
            )
            t.write_line(req.encode())
        t.close()  # abrupt disconnect mid-write
        time.sleep(1.5)  # device must not block forever here
        ok = serves(port)
        print(f"   cycle {i + 1}/{cycles}: reconnect -> {'serves' if ok else 'WEDGED'}")
        if not ok:
            return False
    return True


def scenario_watchdog_idle(port: str, idle: float) -> bool:
    print(f"3) task watchdog fed under {idle:.0f}s idle")
    noise = monitor(port, idle)
    hits = [ln for ln in noise if any(m in ln for m in RESET_MARKERS)]
    if hits:
        print(f"   RESET/WDT markers seen: {hits[:3]}")
        return False
    print(f"   {idle:.0f}s idle, no reset/watchdog markers ({len(noise)} stray lines)")
    ok = serves(port)
    print(f"   still responsive -> {'yes' if ok else 'NO'}")
    return ok


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", default="/dev/cu.usbmodem101")
    ap.add_argument("--elf", help="reflash this ELF for a known-good baseline before testing")
    ap.add_argument("--idle", type=float, default=20.0, help="watchdog idle hold seconds")
    args = ap.parse_args()

    if args.elf:
        print(f"flashing {args.elf} ...")
        flash_device(args.port, args.elf)
        time.sleep(2.0)

    results = {
        "boot-without-host": scenario_boot_without_host(args.port),
        "disconnect-under-load": scenario_disconnect_under_load(args.port),
        "watchdog-idle": scenario_watchdog_idle(args.port, args.idle),
    }
    print("\n=== results ===")
    for name, ok in results.items():
        print(f"  {'PASS' if ok else 'FAIL'}  {name}")
    allok = all(results.values())
    print("OVERALL:", "PASS" if allok else "FAIL")
    return 0 if allok else 1


if __name__ == "__main__":
    sys.exit(main())
