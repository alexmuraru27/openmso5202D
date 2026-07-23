# Serial-protocol decoders

Reference for `backend/src/decoder/` — the UART, SPI and I²C decoders, the shared analog
front end they run on, and how a capture reaches the byte list in the UI.

## Overview

The decoders take **volts** — one `f64` per sample, as parsed from a capture — and produce a
stream of **`Event`s**: decoded bytes and bus markers, each carrying the sample range it
spans.

Everything under `backend/src/decoder/` is pure logic. Nothing there talks to USB, opens a
device or reads a file; the input is a `&[f64]` (or an already-thresholded `&[bool]`), so a
decode runs identically on a live capture, on a CSV loaded from the SD card, and on a
synthesised trace in a test — with no instrument attached.

| Protocol | Module | Channels | Framing comes from |
|---|---|---|---|
| UART | `decoder::uart` | one line | a least-squares bit grid + frame validation |
| SPI | `decoder::spi` | clock + data (+ optional chip select) | chip select, else idle-clock gaps |
| I²C | `decoder::i2c` | SCL + SDA | START/STOP conditions — self-framing |

## The pipeline

```
volts (Vec<f64>)
  └─ decoder::common::threshold_volts        → logic trace (Vec<bool>)
       └─ uart::decode / spi::decode / i2c::decode  → Vec<Event>
            └─ api::decode                   → Vec<DecodedItem>
                 ├─ ByteList                 (byte list)
                 └─ WaveformView drawDecode / drawByteSlices  (pill overlay + time slices)
```

Real functions, in order:

1. **`waveform::parse_csv`** (`backend/src/waveform.rs`) produces `volts` and `dt_s` (the
   median timestamp step) for each channel.
2. **`decoder::common::threshold_volts(&[f64]) -> Vec<bool>`** digitises a channel.
3. One of **`uart::decode`**, **`spi::decode`**, **`i2c::decode`** returns `Vec<Event>`.
4. **`api::decode`** (`frontend/src-tauri/src/api.rs`) picks the decoder from
   `CaptureConfig.protocol`, thresholds the needed channels through the same
   `decoder::common::threshold_volts`, and maps each `Event` to a `DecodedItem`:
   `start_s = e.start * dt_s`, `end_s = e.end * dt_s`, `text = e.text()`,
   `kind = kind_name(e.kind)`, `value`, and `channel` = the data channel (UART line, SPI MOSI,
   I²C SDA) — annotations are therefore always drawn on the data lane, never the clock lane.
5. The React side renders `DecodedItem`s in **`ByteList`** and, on the canvas, in
   **`drawDecode`** (per-lane pills) and **`drawByteSlices`** (time slices across all lanes),
   both in `frontend/src/components/WaveformView.tsx`.

`api::decode` channel selection and per-protocol arguments:

| `protocol` | Channels | Call |
|---|---|---|
| `"uart"` | `data_channel` (default 1) | `uart::decode(&trace, UartOptions { sample_interval_ns: Some(dt_s*1e9), baud: Some(max_freq_hz), ..Default::default() })` |
| `"spi"` | `clock_channel` (default 1), `data_channel` (default 2) | `spi::decode(&clk, &data, None, Some(clock volts), SpiOptions::default())` |
| `"i2c"` | `clock_channel` = SCL (default 1), `data_channel` = SDA (default 2) | `i2c::decode(&scl, &sda, i2c::Anchor::Auto)` |
| anything else | — | empty vec |

Only `protocol`, the channel assignments and `max_freq_hz` reach the decoders from the UI —
`CaptureConfig` has no protocol-option fields, so SPI always runs `SpiOptions::default()`
(mode 0, MSB-first, `Anchor::Auto`, no chip select), I²C always `Anchor::Auto`, and UART the
defaults. The remaining options are for library callers and tests.

`kind_name` maps `Kind` to the strings `"byte"`, `"address"`, `"start"`,
`"repeated-start"`, `"stop"`.

## Shared types

`backend/src/decoder/mod.rs`.

### `Kind`

| Variant | Meaning | `carries_value()` |
|---|---|---|
| `Byte` | a data byte | `true` |
| `Address` | an I²C address byte | `true` |
| `Start` | I²C START condition | `false` |
| `RepeatedStart` | I²C repeated START | `false` |
| `Stop` | I²C STOP condition | `false` |

### `Event`

| Field | Type | Meaning |
|---|---|---|
| `start` | `usize` | index of the first sample the event spans |
| `end` | `usize` | index of the last sample the event spans |
| `value` | `Option<u8>` | the decoded byte; `None` for a bus marker |
| `ok` | `bool` | UART: parity (a framing violation rejects the frame outright, so no event is emitted); I²C: ACK; SPI: always `true` |
| `kind` | `Kind` | what the event is |

### Helpers

| Item | Behaviour |
|---|---|
| `values(&[Event]) -> Vec<u8>` | the byte values, ignoring bus markers — keeps `Kind::Byte` **and** `Kind::Address` |
| `Event::text() -> String` | `Start → "S"`, `RepeatedStart → "Sr"`, `Stop → "P"`, a value → `"AB"` (or `"AB!"` when `ok` is false), a valueless event of any other kind → `""` |

## Thresholding (`common.rs`)

`threshold_volts(volts)` is the single entry point; it is a thin wrapper over
`threshold_local`. `threshold_local` falls back to `threshold_global` when it cannot gauge a
timescale.

### `threshold_global(sig)`

One fixed threshold for the whole record.

- Rails are percentiles, not min/max, so a single glitch cannot set the scale:
  `lo = percentile(sig, 0.1)`, `hi = percentile(sig, 99.9)`.
- `span = hi - lo`; `span < 1e-12` → all-`false` output of the same length.
- `mid = lo + span*0.5`, `band = span * 0.3 / 2.0` (±0.15·span).
- Schmitt trigger with thresholds `mid ± band`, seeded `sig[0] > mid`.

### `threshold_local(sig)`

Digitises against the signal's **local envelope**, so a line whose low droops during bursts
(AC coupling, limited bandwidth) keeps its edges.

Constants: `HYSTERESIS_FRACTION = 0.2`, `FLOOR_FRACTION = 0.12`, `COARSE_BAND = 0.05`.

1. `lo = percentile(sig, 0.1)`, `hi = percentile(sig, 99.9)`, `span = hi - lo`;
   `span < 1e-12` → all-`false`.
2. **Coarse global pass**, purely to estimate a timescale: Schmitt with thresholds
   `lo + span*(0.5 ∓ COARSE_BAND)`, seed `sig[0] > lo + span*0.5`. The band is deliberately
   narrow (±0.05·span): a bandwidth-limited fast line never crosses a ±0.15 band, which would
   make the period come out as the whole record.
3. `transitions = edges(&coarse)`; **fewer than 4 transitions → `threshold_global(sig)`**.
4. Consecutive transition gaps, sorted; `period = gaps[len/2] * 2.0` (median gap × 2 = one
   full bit period); `window = (period * 1.5).round().max(3.0)`.
5. `sliding_extreme(sig, window, …)` (an O(n) monotonic-deque sliding max/min with edge
   clamping) gives `local_hi` and `local_lo`.
6. Per sample: `mid[i] = (local_hi[i] + local_lo[i]) * 0.5`;
   `band[i] = ((local_hi[i] - local_lo[i]) * 0.2).max(span * 0.12)`. The floor stops idle
   noise — where the local envelope collapses — from chattering.
7. Schmitt with `mid[i] ± band[i]`, seed `sig[0] > lo + span*0.5`. The seed is judged against
   the **global** midpoint: a collapsed local band forces nothing during a long idle, so the
   seed would otherwise decide the level for the whole quiet stretch.

The Schmitt trigger itself (`schmitt`, private): above `hi(i)` → high, below `lo(i)` → low,
in between the state holds; samples before the first forcing one take `initial`.

### Edge and period helpers

| Function | Returns |
|---|---|
| `edges(&[bool]) -> Vec<usize>` | indices `i` in `1..len` where `trace[i] != trace[i-1]` — an edge index is the sample **at the new level** |
| `min_pulse(&[bool]) -> usize` | shortest constant run in samples (min difference between consecutive edges); `0` with fewer than 2 edges — a rough one-bit estimate |
| `idle_level(&[bool]) -> bool` | the level held during the longest constant run (`true` for a trace shorter than 2 samples) — distinguishes an idle-high line from an inverted one |
| `percentile(&[f64], q)` | NumPy-default linear-interpolated percentile; `0.0` on empty input |
| `round_half_even(f64)` | banker's rounding, matching NumPy/Python 3 (Rust's `f64::round` rounds halves away from zero, shifting a grid index by one) |
| `ramp_ratio(&[u8]) -> f64` | fraction of adjacent byte pairs stepping by +1 mod 256; `0.0` for fewer than 2 values |

## Bit-grid recovery (`common.rs`)

This is the heart of UART decoding. Every edge sits on a bit boundary — an integer number of
bit periods from every other edge — so the true period is the one that puts all of them on
grid lines at once. Recovering it from the record matters because the nominal period is
usually not the real one: a transmitter's divider rarely hits the requested rate exactly, and
an error of a tenth of a percent walks a decode a whole bit out of step across a deep record.

### `refine_period(edge_indices, initial_spb) -> (samples_per_bit, phase)`

Constants: `FIRST_SPAN = 128` (edges in the first stage), `REACH = 0.02` (±2 % search reach
around nominal), `DRIFT_PER_STEP = 0.25` (step size = a quarter-bit of accumulated drift
across the span).

Fallback — returned immediately when `edge_indices.len() < 3 || initial_spb < 1.0` — is
`(initial_spb, first edge as f64, else 0.0)`.

**Stage 1: hierarchical direct search.** A short prefix cannot resolve the period finely but
brackets it cheaply; each doubling of the span sharpens the resolution and narrows the
bracket.

```
spb = initial_spb; (low, high) = (-REACH, REACH); span = FIRST_SPAN
loop {
    take   = min(span, edges.len());  window = &edges[..take]
    cells  = (window[take-1] - window[0]) / spb
    step   = (DRIFT_PER_STEP / cells.max(1.0)).min(REACH / 4.0)   // ≤ 0.005
    for offset in low..=high step step:
        score = grid_concentration(window, spb * (1 + offset))    // keep the argmax
    spb = best candidate
    if take == edges.len() { break }
    low = -2*step; high = 2*step; span *= 2
}
```

`grid_concentration(edges, spb)` is the magnitude of the edge train's Fourier component at
period `spb`, in `[0,1]`: each edge contributes a unit vector at angle `TAU * (e / spb)`,
and the sum is normalised by the edge count. Because phase is measured circularly, an edge
that has drifted past a cell boundary counts as slightly early rather than as a whole period
of error — which is what keeps the score meaningful far from the starting guess.

**Stage 2: polish by regression, guarded.**

```
tolerance = spb * DRIFT_PER_STEP / ((last_edge - first_edge)/spb).max(1.0)
match fit_grid(edges, spb) {
    Some((refined, phase)) if (refined - spb).abs() <= tolerance => (refined, phase),
    _ => (spb, edges[0] as f64),
}
```

The search runs first because snap-and-regress alone is only valid once the period is already
close; the regression is used only to polish the winner, and only if it stays inside the
basin the search picked.

`fit_grid(edges, spb)` is one least-squares pass: `first = edges[0]`,
`grid[k] = round_half_even((e - first)/spb)`, then
`refined = (n·Σke − Σk·Σe)/(n·Σk² − (Σk)²)` and `phase = (Σe − refined·Σk)/n`. It returns
`None` if there are fewer than 3 edges, if `grid.last() == grid[0]`, if the denominator's
magnitude is below `1e-12`, or if `refined <= 1.0` or is non-finite.

### `sample_grid(trace, spb, phase, vote) -> (Vec<bool>, Vec<usize>)`

Samples every cell of the grid `phase + k·spb`, returning one bit per cell plus each cell's
centre sample index.

- Guard: `n == 0 || spb < 1.0` → two empty vectors.
- The phase is centred into `(-spb/2, spb/2]` via
  `phase = (phase + 0.5*spb).rem_euclid(spb) - 0.5*spb`. Plain modulo could yield ≈`spb`
  instead of 0 for a near-exact multiple and start the grid one cell late.
- `k0 = ((0.0 - phase)/spb).floor()`; `k1 = ((n-1) - phase)/spb` truncated toward zero.
  `k1 <= k0` → empty.
- For `k in k0..k1` (exclusive of `k1`, so a trailing partial cell is not emitted):
  `sample_cell(trace, phase + (k + 0.5)*spb, spb, vote)`.

### `sample_cell(trace, centre, spb, vote) -> (bool, usize)`

- `index = round_half_even(centre)` clamped to `0..=n-1`.
- `!vote || spb < 4.0` → `(trace[index], index)`. Below four samples per bit there is no
  middle to vote over.
- Otherwise **majority vote over the cell's middle half**: `half = ((spb*0.25) as usize).max(1)`,
  count highs over `offset in -half..=half` with clamped indices, result is
  `high * 2 >= total` — ties resolve high. This absorbs edge jitter that a single centre
  sample would be at the mercy of.

### `both_ways(decode, score)`

Runs `decode(false)` and `decode(true)` and returns the reverse result iff
`score(&reverse) > score(&forward)` (ties keep forward). A capture whose start is corrupt —
triggered mid-byte — but whose tail is clean still yields the tail, exactly where a
forward-only pass desyncs and loses it.

## UART

`backend/src/decoder/uart.rs`. Asynchronous, LSB-first, start/stop framed. Decodes on the
bit grid rather than by hunting start edges, and picks the byte phase by whichever framing
validates the most frames: in a gapless stream every data 1→0 edge mimics a start bit, so
edge-hunting frames at the wrong boundary and produces garbage that still looks byte-shaped.

### `UartOptions`

| Field | Type | Default | Meaning |
|---|---|---|---|
| `sample_interval_ns` | `Option<f64>` | `None` | sample interval in ns; with `baud` it locks the bit period |
| `baud` | `Option<f64>` | `None` | line rate in bits per second |
| `bits` | `usize` | `8` | data bits per frame |
| `parity` | `Parity` | `Parity::None` | `None`, `Even`, `Odd` |
| `stops` | `usize` | `1` | stop bits per frame |
| `idle` | `Idle` | `Idle::High` | resting level: `High`, `Low`, or `Auto` (best effort; unreliable on some continuous streams — prefer stating it) |
| `both_ways` | `bool` | `false` | also try the time-reversed trace and keep whichever decodes more frames |

### `decode(trace, options)`

- `idle == Idle::Auto` → `decode_auto_polarity`.
- `options.both_ways` → `common::both_ways(|reverse| decode_once(trace, reverse, options), |frames| frames.len())` — the score is the frame count.
- otherwise → `decode_once(trace, false, options)`.

**`decode_auto_polarity`** re-enters `decode` once with `Idle::High` and once with `Idle::Low`
(so `both_ways`, if set, still applies within each polarity). With
`shorter = min(len)` and `longer = max(len).max(1)`, if `shorter >= 0.8 * longer` the counts
are considered close and the tie is broken by `common::idle_level(trace)`; otherwise the
longer result wins.

### `decode_once(trace, reverse, options)`

1. `n = trace.len()`; `n < 2` → empty. With `reverse`, work on a reversed copy.
2. **Initial samples per bit**: with both `baud > 0` and `sample_interval_ns > 0`,
   `initial_spb = (1e9/baud) / interval_ns`; otherwise `initial_spb = min_pulse(trace)`.
   `initial_spb < 2.0` → empty.
3. **Bit grid**: `edge_indices = edges(trace)`. With ≥ 3 edges,
   `(spb, phase) = refine_period(&edge_indices, initial_spb)`; otherwise
   `(initial_spb, first edge or 0.0)`. `spb < 2.0` → empty.
4. `parity_bits = (parity != None) as usize`; `frame_len = 1 + bits + parity_bits + stops`
   (10 with the defaults).
5. `(bits, centres) = sample_grid(trace, spb, phase, /*vote=*/true)`; `bits.len() < frame_len`
   → empty.
6. `idle = (options.idle == Idle::High)` — the mark level.
7. `FrameLayout::new(reverse, bits, parity_bits, stops, frame_len)`.
8. **Candidate match at every cell offset**: for each of
   `bits.len() - (frame_len - 1)` windows, `layout.match_frame(...)` → `Option<(u8, bool)>`.
9. **Two framings, keeping whichever validates more:**
   - **(A) fixed-offset tiling** — for each `offset in 0..frame_len`, take matches at
     `offset, offset+frame_len, …` and keep the longest resulting event list. This is the
     correct byte boundary for a gapless stream, where a wrong phase piles up stop-bit
     violations on real data.
   - **(B) greedy resync walk** — `i = 0`; on `Some(m)` emit an event and `i += frame_len`,
     on `None` `i += 1`. Needed when frames are separated by idle gaps, which fixed tiling
     cannot follow.
   - Selection: `if best_tiled.len() >= walked.len() { best_tiled } else { walked }` — tiling
     wins ties. Both naturally skip an invalid leading frame and continue from the first
     clean one.
10. With `reverse`, the frame list is reversed before returning.

### Frame layout and validation

`FrameLayout` holds `start_cell`, `data_cells`, `parity_cell`, `stop_cells`, `frame_len`.

| Direction | Layout | Cells |
|---|---|---|
| forward | `[start][data LSB..MSB][parity?][stop*]` | `start_cell = 0`; `data_cells[b] = 1+b`; `parity_cell = 1+bits`; `stop_cells[s] = 1+bits+parity_bits+s` |
| reversed | `[stop*][parity?][data MSB..LSB][start]`, `base = stops + parity_bits` | `start_cell = frame_len-1`; `data_cells[b] = base + (bits-1-b)`; `parity_cell = stops`; `stop_cells = 0..stops` |

`match_frame`:

- **Reject** if `cells[start_cell] == idle` — the start bit must sit at the opposite level to
  idle.
- **Reject** if any stop cell `!= idle`.
- Value: bit `b` is set iff `cells[data_cells[b]] == idle`. A logical 1 is the mark level,
  which equals idle, so this also reads an inverted line correctly with no extra inversion.
- `ok`: `Even` → `(value.count_ones() + (parity cell == idle)) % 2 == 0`; `Odd` → the same
  sum `% 2 == 1`; no parity → `true`. **A failing parity still yields an event, with
  `ok = false`** — only start/stop violations reject a frame.

`event()` spans `centres[i] .. centres[i + frame_len - 1]`, remapped to `n-1-b .. n-1-a` when
reversed, emitted as `Kind::Byte`.

## SPI

`backend/src/decoder/spi.rs`. Clock plus one data line, with optional chip select. With only
clock and data, and neither gaps nor chip select, byte boundaries are genuinely ambiguous —
nothing in the signal marks them.

### `SpiOptions`

| Field | Type | Default | Meaning |
|---|---|---|---|
| `cpol` | `u8` | `0` | clock idle level |
| `cpha` | `u8` | `0` | clock phase |
| `msb_first` | `bool` | `true` | bit order within a word |
| `bits` | `usize` | `8` | bits per word |
| `word_gap` | `f64` | `10.0` | an idle-clock gap longer than this many median periods splits bursts |
| `max_missed` | `usize` | `8` | a gap of 2 up to this many periods is treated as missed clock edges |
| `anchor` | `Anchor` | `Anchor::Auto` | byte-boundary anchoring |
| `auto_mode` | `bool` | `false` | ignore `cpol`/`cpha` and detect the sampling edge from the signal |

`Anchor`: `Start` groups forward from each burst's start, dropping a trailing partial; `End`
anchors the first burst to its end, dropping the leading partial (correct when the
transaction ended cleanly but the capture began mid-byte); `Auto` (default) picks `End` when
the clock stopped well before the record ended yet was already running at sample 0.
Whole-byte bursts decode identically either way.

### `detect_sample_rising(clock, data)`

Data is shifted on one clock edge and held stable across the other — the sampling edge — so
data transitions cluster near the shift edge, and the sampling edge is the opposite one.

1. Collect clock `rising`, `falling`, and `data_edges`.
2. Bail out to `true` (rising, mode 0) if `data_edges.len() < 3 || rising.len() < 2 || falling.len() < 2`.
3. Merge the clock edges into a sorted `Vec<(index, is_rising)>`.
4. For each data edge, find the nearest clock edge on either side (`partition_point`, clamped
   to `1..=len-1`); a tie (`d - left <= right - d`) favours the left one. Count how many
   nearest edges are rising → `near_rising`.
5. `shift_on_rising = near_rising >= data_edges.len() - near_rising` (ties → rising is the
   shift edge); return `!shift_on_rising`.

### Clock-edge choice

`sample_rising = if auto_mode { detect_sample_rising(clock, data) } else { cpol == cpha }`.
So modes 0 (0,0) and 3 (1,1) sample on the rising edge; modes 1 and 2 on the falling edge.
`clock_edges` are the indices in `1..n` matching that polarity.

`median_period` is computed **only when `chip_select.is_none() && clock_edges.len() > 2`**:
the median of consecutive edge gaps.

### Burst collection

Bits are collected into bursts (`Vec<Vec<(bool, usize)>>`), splitting on deselect or an
idle-clock gap and reconstructing edges the clock lost:

- **With chip select** (active low): `active = !cs[edge.min(cs.len()-1)]`. Inactive → close
  the current burst and `continue`, so the edge is gated out and contributes no bit. Becoming
  active again after being inactive starts a new burst.
- **Without chip select**, when `median_period` and a previous edge exist: `gap = edge - previous`,
  `periods = round_half_even(gap/median)`.
  - If `(2..=max_missed).contains(&periods)` **and** `|gap - periods*median| <= 0.5*median`:
    with `clock_analog` supplied and `!gap_has_pulse(...)` the gap is a real idle and the
    burst closes; otherwise the missing edges are synthesised at
    `previous + round_half_even(k*median)` for `k in 1..periods`.
  - Else if `word_gap > 0.0 && gap > word_gap * median` and the current burst is non-empty,
    the burst closes (word gap).
- Every clock edge then samples the data line: `data[index.min(len-1)]`.

Empty bursts are dropped afterwards.

**`gap_has_pulse(clock_analog, a, b, median_period)`** decides whether the clock actually
pulsed inside a suspicious gap — timing alone cannot separate missed edges from a real
inter-word idle, but the raw analog clock can. It inspects the **middle** of the gap, away
from the transition tails: with `m = round_half_even(median_period).max(1)`,
`lo_index = (a + m/2).min(b-1)`, `hi_index = (b - m/2).max(a+2)`, using that segment if it
holds ≥ 3 samples, else `clock_analog[a+1..b]`. A segment shorter than 2 samples returns
`true` (assume a pulse), as does a flat record: with `lo = percentile(clock_analog, 1.0)` and
`hi = percentile(clock_analog, 99.0)` taken over the **whole** clock record,
`swing = hi - lo`, a swing below `1e-9` returns `true`. Otherwise **both** conditions are
required: `(seg_max - seg_min) > 0.4 * swing` **and** `seg_min < mid && seg_max > mid`, with
`mid = (lo + hi) / 2` — the segment's excursion judged against the record's global rails.
Ring-down at the end of a burst
routinely lifts the idle rail by a sizeable fraction of the swing without ever approaching
mid, and a gap misread as a pulse injects phantom bits that shift every byte until the next
burst boundary.

### Anchor decision and word assembly

`anchor_end` is `true` for `Anchor::End`, `false` for `Anchor::Start`. For `Anchor::Auto` it
is computed only when `median_period`, a first and a last clock edge all exist **and**
`chip_select.is_none()`: `clean_start = first > word_gap*median`,
`clean_end = (n - last) > word_gap*median`, `anchor_end = clean_end && !clean_start`.
Otherwise `false`.

Word assembly: burst 0 with `anchor_end` drops its leading partial
(`&burst[burst.len() % bits ..]`); then `chunks_exact(options.bits)`. Per chunk,
`msb_first` accumulates `value = (value << 1) | bit`, otherwise `value |= bit << j`. Each
word is emitted as `Event { start: group[0].1, end: group.last().1, value: Some(value as u8),
ok: true, kind: Kind::Byte }`. The accumulator is a `u32` truncated with `as u8`, so
`bits > 8` silently truncates. SPI never sets `ok = false`.

## I²C

`backend/src/decoder/i2c.rs`. SCL plus SDA. START is SDA falling while SCL is high, STOP is
SDA rising while SCL is high; bits are sampled on SCL **rising** edges, MSB first, 8 data bits
plus an ACK. The bus is self-framing, so none of the bit-grid machinery is needed — but a
capture window that catches no START has no boundary to lock onto, which is what the
end-anchored fallback is for.

`Anchor`: `Start` (forward only), `Auto` (default — forward when a START was captured, else
end-anchored), `End` (force the fallback).

`decode(scl, sda, anchor)` always runs `decode_forward` first, then:
`Anchor::Start` → the forward events; `Anchor::Auto` **with** a `Kind::Start` event → the
forward events; otherwise → `decode_end_anchored(scl, sda, events)`.

### `decode_forward`

`n = min(scl.len(), sda.len())`, iterating `i in 1..n`. State: the collected `bits`,
`byte_start`, `in_frame`, `expect_address`, `expect_address_low`.

1. **While SCL is high across the pair** (`scl[i] && scl[i-1]`):
   - SDA `1→0` → emit `RepeatedStart` if already `in_frame`, else `Start` (a one-sample
     event, `value: None`, `ok: true`); clear the bit buffer, set `in_frame = true`,
     `expect_address = true`, `expect_address_low = false`; `continue`.
   - SDA `0→1` → emit `Stop` (one sample, `value: None`, `ok: true`); clear the bit buffer,
     `in_frame = false`, `expect_address_low = false`; `continue`.
2. **Bit sampling**: while `in_frame` and SCL rises (`scl[i] && !scl[i-1]`), record
   `byte_start` if unset and push `sda[i]`. On the ninth bit:
   - `value` = `bits[..8]` folded MSB-first; `ack = !bits[8]` (a low ninth bit is ACK).
   - **Kind**: `expect_address` and `(value & 0xF8) == 0xF0` → the first byte of a 10-bit
     address (`11110xx` + R/W): `Address`, and the next byte is expected to be the low half.
     Else `expect_address` → `Address` (7-bit address). Else `expect_address_low` →
     `Address` (10-bit second byte). Else `Byte`.
   - Emit `Event { start: byte_start, end: i, value: Some(value), ok: ack, kind }` and clear
     the buffer.

The **R/W bit is not a separate field** — it is the LSB of the `Address` event's `value`
(a write to address `0x50` decodes as `0x50 << 1`). `Event` has no read/write flag.

Kind → bus marker mapping is what `Event::text()` renders: `Start` → `S`,
`RepeatedStart` → `Sr`, `Stop` → `P`; `Address` and `Byte` render as two hex digits, with a
trailing `!` when `ok` is false (a NACK, for I²C).

### `decode_end_anchored`

For a capture triggered mid-transaction — the direct analog of SPI end-anchoring.

1. Collect SCL `rises` and `falls` over `n = min(len)`.
2. `last_stop` = the `start` of the last `Kind::Stop` event from the forward decode.
3. **Usable rises**: with a STOP, the cutoff is the last falling edge before it (else the stop
   index itself) and only rises below the cutoff count — a STOP releases SDA with SCL held
   high, adding a rising edge that is neither a data nor an ACK bit. With no STOP, all rises
   are usable.
4. Fewer than 9 usable rises → return the forward events unchanged.
5. Drop the leading partial: `&usable[usable.len() % 9 ..]`.
6. The output starts as **only the STOP events** from the forward decode (START, Address and
   Byte events are discarded); each `chunks_exact(9)` group then emits
   `Event { start: group[0], end: group[8], value: MSB-first fold of sda over group[..8],
   ok: !sda[group[8]], kind: Kind::Byte }`. Every byte is a plain `Byte` — the address is
   off-screen.
7. The result is sorted by `start`.

## Choosing a capture that decodes

The single most important property of a capture is **samples per bit** (per clock period for
SPI/I²C). The UI's `samplesPerCycle` setting (default `20`) drives the timebase choice in
`frontend/src/timebase.ts` `capturePlan` and in the backend's `CaptureConfig::timebase_ns`,
which both snap the requested resolution down onto the scope's timebase ladder.

What the code sets as hard limits:

| Limit | Where | Effect |
|---|---|---|
| `spb < 2.0` | `uart::decode_once`, twice (initial estimate and after refinement) | no events at all |
| `spb < 4.0` | `common::sample_cell` | per-cell majority voting is skipped; the single centre sample is used, so jitter is no longer absorbed |
| fewer than 4 coarse transitions | `common::threshold_local` | falls back to the global threshold, losing local-envelope tracking |
| fewer than 3 edges | `common::refine_period` / `fit_grid` | the bit period is not refined; the nominal estimate is used as-is |
| gap > `word_gap` (10) × median period | `spi::decode` | splits a burst — an inter-word idle shorter than this does not reframe |
| gap of `2..=max_missed` (8) periods | `spi::decode` | treated as missed clock edges (or, with the analog clock supplied and no pulse found, as a real idle) |

**Too coarse** (a handful of samples per bit): below 4 samples per bit the vote is disabled
and a single jittered sample decides a bit; below 2 the UART decoder returns nothing. Fine
pulses may also fail to register as transitions at all, which pushes `threshold_local` onto
its global fallback and inflates the estimated bit period.

**Too fine** (a very slow timebase relative to the bit rate) costs record coverage rather
than correctness: at a fixed depth, more samples per bit means fewer bits in the window. A
longer message needs a slower timebase or a deeper record — deep capture at a fixed timebase
covers the same time window with more samples, not a longer one.

For UART specifically, supplying **both** `baud` and `sample_interval_ns` locks the initial
bit period; without both, the decoder falls back to `min_pulse`, which is only a rough
estimate and is wrong whenever the shortest run in the record is not a single bit.

## Testing

All three suites live in `backend/tests/` and need no instrument.

### `decoder.rs` — synthesised signals, exact expected bytes

Runs in the default suite. Helpers: `ramp()` = `0..=255`, `stretch(bits, spb)` repeats each
bit, `as_volts` maps `true → 3.3` and `false → 0.0`.

- **Front end**: `ramp_ratio_measures_consecutive_bytes`, `percentile_interpolates_like_numpy`,
  `edges_land_on_the_new_level`, `idle_level_is_the_longest_run`,
  `refine_period_recovers_the_true_bit_period` (edges at `5 + k*10`, seeded 9.9 → `spb ≈ 10.0`,
  `phase ≈ 5.0` within 1e-6), `refine_period_falls_back_when_degenerate`,
  `sample_grid_votes_over_each_cell`, `thresholding_recovers_a_logic_trace_from_volts`,
  `thresholding_handles_flat_and_empty_input`.
- **UART**: every parity at 20 samples/bit through the analog path
  (`uart_decodes_framed_bytes_for_every_parity`), a gapless continuous stream
  (`uart_decodes_a_gapless_continuous_stream`), tail recovery from a trace cut at 37 %
  (`uart_recovers_the_tail_when_the_capture_starts_mid_byte`, > 100 bytes and
  `ramp_ratio >= 0.99`), an inverted line with `Idle::Low` and with `Idle::Auto`, and empty /
  constant traces yielding nothing.
- **SPI**: all four modes × MSB/LSB first, `auto_mode` detecting the sampling edge in every
  mode, reframing on idle-clock gaps, dropping only the partial byte when cut mid-byte,
  end-anchoring a gapless burst with a clean tail, and using the analog clock to tell a
  missed edge from a real gap (`[0xA5, 0x5A]` decoding exactly).
- **I²C**: `i2c_decodes_address_data_and_markers` (one START, one STOP, one `Address` whose
  value is `0x50 << 1`, data bytes equal to the ramp, everything ACKed),
  `i2c_end_anchors_when_the_start_was_missed`, `i2c_labels_a_repeated_start` (two
  transactions → 2 STARTs and 2 STOPs).
- **Rendering**: `events_render_like_a_scope_annotation` checks `text()` produces `"S"`,
  `"P"` and a two-hex-digit byte.

```sh
cargo test -p mso5202d --test decoder
```

### `decoder_corpus.rs` — ramp scoring over the free-running corpus

`#[ignore]`d and **must be run with `--release`**: it reads 276 MB of captures from
`../scope_dump/decoder_corpus` and decodes ~2 million samples per case (about 20 s optimised,
over ten minutes unoptimised). Constants: `SCORES = "tests/decoder_scores.json"`,
`PROTOCOLS = ["spi", "uart", "i2c"]`, `BROKEN_BELOW = 0.30`, `NOTABLE = 0.02`.

Each manifest case (`freq`, `depth`, `spc`, `files`) is loaded via `waveform::parse_csv` into
a channel of `volts` + `threshold_volts(&volts)` + `dt_s`, then decoded: UART with
`sample_interval_ns` and `baud: Some(freq)`; SPI and I²C with both channel assignments tried
and whichever yields more `values()` entries kept (a capture is tagged by the order its
sources were saved, so labels can be swapped).

The score is `ramp_ratio` over **`Kind::Byte` events only** — an I²C `Address` is a decoded
byte but not part of the generator's ramp, so counting it would understate a perfect decode —
plus a byte count. The run rewrites the committed `tests/decoder_scores.json`, so `git diff`
on that file is the improvement/regression report; it also prints per-protocol tables with
per-case deltas, mean ramp, `n/N at >=0.99`, an overall mean and delta in points, and
explicit `improved` / `REGRESSED` lists for cases moving more than ±`NOTABLE`. The only hard
assertions are that the corpus produced some cases and that each protocol's mean ramp is at
least `BROKEN_BELOW`. A missing corpus directory prints and returns.

```sh
pnpm decoder:score
# equivalently:
cargo test -p mso5202d --test decoder_corpus --release -- --ignored --nocapture
git diff backend/tests/decoder_scores.json
```

### `decoder_triggered.rs` — byte-for-byte grading

`#[ignore]`d, `--release`. Corpus `../scope_dump/decoder_corpus_triggered`,
`PROTOCOLS = ["uart", "spi", "i2c"]`, `SCORES = "tests/decoder_scores_triggered.json"`.

Each capture holds exactly one commanded burst starting at pattern index 0 with idle either
side, so the expected bytes are known exactly and are read from the manifest's hex `expected`
field (`decode_hex`) rather than recomputed — keeping the grader independent of how the
pattern is generated. Loading and per-protocol decoding match the corpus test; the swap
tie-break compares `data_bytes` lengths.

`accuracy(decoded, expected)` is the fraction of expected bytes decoded correctly **at the
best alignment**: `0.0` if either side is empty; `search = min(decoded.len(), expected.len(), 16)`;
for each `offset in -search..=search` count matching positions; return `best / expected.len()`.
The alignment search covers a capture that clipped a leading byte or picked up a spurious
one.

The test prints `case | decoded | expected | accuracy`, marking `OK` at
`accuracy >= 0.9999` and `**` otherwise, then the mean accuracy and how many cases are
byte-perfect, and writes the scoreboard for `git diff` review. **It has no assertions** — it
is purely a graded report, and a missing or empty corpus just prints and returns.

```sh
cargo test -p mso5202d --test decoder_triggered --release -- --ignored --nocapture
git diff backend/tests/decoder_scores_triggered.json
```

## Diagrams

- [Decoder pipeline](diagrams/decoder-pipeline.drawio.png) — volts → threshold → decode → events → UI.
- [UART decode](diagrams/uart-decode.drawio.png) — bit grid, frame layout, tiling vs walk.
- [SPI decode](diagrams/spi-decode.drawio.png) — edge selection, burst splitting, word assembly.
- [I²C decode](diagrams/i2c-decode.drawio.png) — START/STOP handling and the end-anchored fallback.
