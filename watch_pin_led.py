#!/usr/bin/env python3
"""Poll a GPIO input and mirror its level on the on-board LED.

Configures the pin as an input, then reads it in a loop: when the pin is high
(1) the LED lights, when it's low (0) the LED is off. Runs until Ctrl-C and
always leaves the LED off on exit.

    python3 watch_pin_led.py --port /dev/cu.usbmodem101 --pin 45

Reuses the JSON-RPC transport/client from host_client.py.
"""

from __future__ import annotations

import argparse
import sys
import time

from host_client import Client, SerialTransport, Timeout


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--port", metavar="DEV", required=True, help="serial port of the board")
    ap.add_argument("--baud", type=int, default=115200)
    ap.add_argument("--timeout", type=float, default=2.0)
    ap.add_argument("--pin", type=int, default=45, help="input pin to watch (default 45)")
    ap.add_argument(
        "--color",
        metavar="R,G,B",
        default="0,16,0",
        help="LED color when the pin is high (default 0,16,0 = green)",
    )
    ap.add_argument("--interval", type=float, default=0.001, help="seconds between reads (default 0.1)")
    args = ap.parse_args()

    r, g, b = (int(x) for x in args.color.split(","))

    transport = SerialTransport(args.port, args.baud, args.timeout)
    client = Client(transport, timeout=args.timeout)
    try:
        client.gpio_config(args.pin, "input")
        print(f"watching pin {args.pin}; LED ({r},{g},{b}) when high. Ctrl-C to stop.")

        last = None  # only send led_set when the state changes
        while True:
            level = client.gpio_read(args.pin)
            if level != last:
                if level == 1:
                    client.led_set(r, g, b)
                else:
                    client.led_set(0, 0, 0)
                print(f"gpio_read(pin={args.pin}) -> {level}  LED {'ON' if level == 1 else 'off'}")
                last = level
            time.sleep(args.interval)
    except KeyboardInterrupt:
        print("\nstopping")
        return 0
    except (RuntimeError, Timeout, ValueError) as e:
        print(f"FAIL: {e}", file=sys.stderr)
        return 1
    finally:
        client.led_set(0, 0, 0)  # leave the LED off
        transport.close()


if __name__ == "__main__":
    sys.exit(main())
