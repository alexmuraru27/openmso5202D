#!/usr/bin/env python3
"""Run every decoder's hardware-free self-test (synthesises the 0x00..0xFF ramp in each protocol and
asserts it round-trips, incl. gapless-continuous and mid-stream-triggered UART).

    python3 -m decoding.selftest        # from scripts/
"""
from decoding import uart, spi, i2c


def main():
    ok = True
    ok &= uart.selftest()
    ok &= spi.selftest()
    ok &= i2c.selftest()
    print("\n" + ("ALL PASS" if ok else "FAILURES ABOVE"))
    return 0 if ok else 1


if __name__ == '__main__':
    import sys
    sys.exit(main())
