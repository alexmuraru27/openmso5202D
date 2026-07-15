"""Serial-protocol decoders (UART / SPI / I²C) for MSO5202D captures — pure logic, no hardware.

Given digital (0/1) traces recovered from the scope's analog channels, reconstruct the bytes on the
wire. Split by concern:
  - `common` : analog→logic thresholding (local-envelope Schmitt), edges, the least-squares bit grid,
               and `both_ways()` (forward+backward decode) — shared by all three decoders;
  - `uart`   : asynchronous, start/stop framed, bit-grid framing robust to gapless streams;
  - `spi`    : SCLK + one data line, gap/CS framing;
  - `i2c`    : SCL + SDA, START/STOP self-framing.

The public API is re-exported here so callers can `from decoding import decode_uart, threshold_volts, …`.
Self-test: `python3 -m decoding.selftest` (or `-m decoding.uart` etc. for one protocol)."""
from decoding.common import (threshold, threshold_volts, schmitt_local, edges, min_pulse,
                             refine_period, sample_grid, both_ways, ramp_ratio)
from decoding.uart import decode_uart
from decoding.spi import decode_spi
from decoding.i2c import decode_i2c

__all__ = ['threshold', 'threshold_volts', 'schmitt_local', 'edges', 'min_pulse',
           'refine_period', 'sample_grid', 'both_ways', 'ramp_ratio',
           'decode_uart', 'decode_spi', 'decode_i2c']
