# MSO5202D — state machines & interaction flows

How to *drive* the scope over USB: the command sequences, the waits, and the
verification between steps. This is the procedural companion to the two reference
docs — `MSO5202D-protocol.md` (the byte-level wire protocol) and
`MSO5202D-rendering.md` (turning samples into a trace). Everything here is
hardware-verified.

The implementation is the Rust driver crate: the flows below are the operation plans in
`backend/src/control/` (`capture.rs` builds them, `mod.rs` runs them), and the host-side
USB stack that carries them is `backend/src/usb/transport.rs` (§8). The Python scripts in
`scripts/` are the reverse-engineering reference tooling the crate was derived from.

The scope is a small embedded instrument with a slow key-scan loop and a
**fragile USB/SD-card coupling**. Driving it reliably is less about individual
commands than about *ordering, pacing, and reading state back between steps*.

---

## 0. Golden rules

1. **Always read the settings between commands.** Poll `0x01` (→ `decode_settings`)
   to see the real state — `TRIG-STATE`, `CONTROL-MENUID` — before the next key or
   write. Never fire a key blind and assume it landed (the key mailbox is
   single-slot and the scan loop is slow). This is *closed-loop* control.

2. **Never USB-reset the device.** A USB reset disturbs the scope's USB **host**
   controller — the same silicon that hosts the front-panel flash drive — dropping the
   drive → Save→CSV fails with **"USB device undetected."** Connect without a reset; a
   no-reset connection is fully functional (§1). `[verified]`

3. **The settings block is READ-ONLY. Configure with keys.** The scope is driven
   exclusively by key events (`0x13`) and knob steps; `0x01` settings reads are only
   ever used to *verify*. A `0x11` block write sets the field but skips the key handler's
   side effects — LED, on-screen radio, acquisition reconfiguration, card detection — so
   the field and the instrument disagree (4a/4b), and a `0x11` depth write on a running
   scope crash-reboots it. The vendor virtual panel behaves the same way: keys and reads
   only. Shell `0x43` and the reads `0x01`/`0x02`/`0x10`/`0x20` are the only non-key
   operations. `[verified]`

4. **Never stop the scope during prepare.** It stays **RUNNING** the whole way through
   configuration; the only STOP is the capture's single sequence. `[verified 2026-07-11]`

4a. **Set store depth with the Acquire-menu F5 softkey, single-edge + poll — not `0x11`.** A
   `0x11` depth write leaves the on-screen **LongMem radio stale at 4K** (scope moves slower
   yet the menu lies), so walk the visible menu: open Acquire (keyid 13 → `CONTROL-MENUID` 17),
   then step F5 (keyid 5) through the ring `4K→40K→512K→1M→(4K)` (codes 0→4→6→7). **F5 advances
   one step per `0x13` frame** (one key event per frame; the 2nd byte is a don't-care). Send **one
   frame per step** and, after each, **poll `ACQURIE-STORE-DEPTH` until it reaches the next step**
   before sending the next — one frame = one step, so never send a second frame for the same step.
   No render delay — the field settles within ~1–2 s and the poll catches it; whole ring 4K→1M in
   ~4 s (`depth_walk` in `backend/src/control/mod.rs`). From the Default-Setup 4K start it takes
   exactly the ring distance.
   1M is single-channel (DS baseline CH1-only satisfies it). `[verified on-screen 2026-07-11]`

4b. **Turn channels on/off with the CH1/CH2 buttons (keyid 24/30), not a `0x11`
   `VERT-CHx-DISP` write.** The `VERT-CHx-DISP` *field* is **decoupled** — a `0x11` write to it
   changes nothing visible (no LED, no acquisition) and the field reads the Default-Setup
   baseline regardless. Each CH button is a **toggle**: one `0x13 keyid` frame flips the channel
   shown↔hidden (the 2nd byte is a don't-care). Drive it closed-loop — read the channel state and
   send **one** frame only when it does not match the target, re-checking after each press.
   ~0.5–1 s button-to-state latency → settle ~1 s. **Verify with 4K wave data, not the field** (`_channel_enabled`): a disabled
   channel's `0x02` acquire returns EMPTY, an enabled one returns ~3200 samples (double-read to
   defeat the one-deep `0x02` channel pipeline). Set channels **before** the depth walk — 1M
   needs CH2 off first (`set_channel` / `Device::channel_has_data`). **`TRIG-SRC` is likewise not
   writable via `0x11`** — a write stays on the old source, so the trigger source is driven from
   its own menu softkey (§3.9). `[verified 2026-07-11]`

5. **Space key presses out (~0.3–0.8 s) and verify the effect.** The vendor app
   spams each key many times because its round-trip is slow; the scope registers
   each *distinct* transition once. Prefer verify-then-advance over spamming.

6. **Deep-file writes take seconds — poll until the size stabilises**, don't read on
   first appearance (a deep CSV is visible on the card while still being written).

---

## 1. Connection

```
find 049f:505a → detach cdc_subset → (NO reset) → claim interface 0 → clear_halt EP 0x02/0x81
```

The kernel auto-binds `cdc_subset` (the VID:PID happens to be that driver's default), so
it must be detached — but the USB reset that a naive recipe does at this point is what
breaks the card, so it is skipped. A no-reset connection is fully functional: settings,
waveform, framebuffer, keys and file-read are all verified over it. Stale RX bytes left
by a dirty prior session are cleared with a resync (§8.10).

---

## 2. Front-panel keys (`0x13`) and closed-loop menu navigation

A key press is `OUT 53 04 00 13 <keyid> <state> ck` (state ignored). `keyid` is the
0-based `/keyprotocol.inf` index (protocol.md §9). The **bezel softkeys map
top-to-bottom to `FN-1…FN-6` = keyid 1…6** (not `FN-0`).

**Closed-loop navigation** — press a menu key, then poll `CONTROL-MENUID` until it
reaches the expected value before pressing the next key. This prevents a stray
starting screen from sending presses into the wrong menu (e.g. into SETUP and
bumping a save Location):

```
press MENU-SR (keyid 11)      → poll CONTROL-MENUID until == 47   (S/R base)
press CSV     (keyid 3)       → poll CONTROL-MENUID until == 48   (CSV page)
```

Relevant `CONTROL-MENUID` values: `1`=CH1 menu, `17`=Acquire, `47`=Save/Recall base,
`48`=Save/Recall CSV, `18`=Save/Recall SETUP. Note the FileList dialog is drawn *on
top* of the CSV page — `CONTROL-MENUID` stays `48`, so use a screenshot (`0x20`) to
see FileList state, not the menuid.

**Return to the main screen** (close any open side menu): press **Back** (keyid 6). `Fn0`
also hides the menu bar, clearing `CONTROL-DISP-MENU`. A side menu left visible is cosmetic
— the next capture's Default Setup clears it.

---

## 3. Capture — one unified mechanism (all depths) `[2026-07-11]`

**Prepare once → Capture (re-pressable).** Every depth (4K…1M) uses the SAME path:
`SINGLE-SEQ trigger → Save→CSV per source → read the CSV(s) back`. Below, `OUT`/`IN` are the
bulk-USB frames — leader `0x53` unless noted; the IN payload echoes `selector | 0x80`
(byte-level detail in `MSO5202D-protocol.md`).

### USB primitives used here
| Op | OUT | IN | Purpose |
|---|---|---|---|
| settings read | `01` | `81` + 213 B | poll `TRIG-STATE` / `CONTROL-MENUID` / depth |
| key event | `13 <keyid> <state>` | `93` | front-panel key (softkey ids in §4) — the only way to configure |
| framebuffer | `20` | `a0` (~768 KB) | read the screen — Source radio, save banner |
| acquire | `02 01 <ch>` | `82` size/data/end | screen-buffer waveform (**probe only**) |
| file read | `10 <path>` | `90` multi-frame | pull a WaveData CSV back (~800 KB/s) |
| shell `ls` | `0x43 11 "ls …"` | `0x91` + stdout | list `/mnt/udisk` (read-only) |

### ① prepare — slow, once — **KEY-ONLY (settings memory is READ-only)**

Every step is a front-panel key; the settings block is only *read* (`01`→`81`) to verify. The
scope stays RUNNING throughout — only the capture stops it. The plan is built by
`CaptureSpec::prepare_plan` and each step is a closed-loop op in `backend/src/control/mod.rs`:

```
Default Setup ....... OUT 13 21 · poll 01→81 MENUID==25         idempotent known state, all side-effects
per channel (CH1 then CH2):
  on / off .......... OUT 13 18 (CH1) / 13 1e (CH2)             CH button is a TOGGLE — press only when
                       · verify via 4K wave-data                 the state does not already match
  probe → 1× ........ channel menu softkey, ring                off the 10× DS default: a direct-wired
                       · poll VERT-CHx-PROBE                     3.3 V reads 33 V at 10× and clips
  V/div ............. OUT 13 1d/1c (CH1 ±) 23/22 (CH2 ±)        off the 100 mV DS default — a 3.3 V
                       · poll CHn-VDIV-mV                        signal at 100 mV/div is 33 div (clipped)
  coupling / 20 MHz / invert ..... channel menu softkeys, rings
SEC/DIV ............. OUT 13 28/29 · poll ns/div (=SI×200)      computed from the stated max signal
                                                                 frequency (§3.0.2); ±labels INVERTED
trigger ............. type/source/slope/… softkeys (§3.9)       only when a trigger is specified
trigger values ...... multipurpose-knob walks (§3.9)            knob-only params: pulse width, V1/V2, …
trigger level ....... OUT 13 2b/2c · poll TRIG-VPOS             skipped in modes whose knob is inert
set depth (F5) ...... OUT 13 0d (open Acquire 17) · OUT 13 05   cycle LongMem 4K→40K→512K→1M until depth
                       · poll 01→81 until depth==target          → leaves scope RUNNING, ready
```

**Ordering is load-bearing.** The probe goes before the volts/div, because it multiplies what
every volts figure on that channel means. The trigger goes after the channels, because a
Default Setup would undo it and Alter configures per channel. Channels come before the depth
walk, because 1M needs CH2 off first.

Knob key ids (keyprotocol.inf §9.2): CH1 V/div −/+ = `1c`/`1d` (28/29), CH2 = `22`/`23`
(34/35), SEC/DIV = `28`/`29` (40/41, slower/faster **inverted** — resolve from the read-back),
trigger level −/+/push = `2b`/`2c`/`2d` (43/44/45). Set-50% = `2e` (46) is a **no-op over USB
injection** — it works on the physical key only, so it is not used.

### ② capture — fast, re-pressable (`CaptureSpec::capture_plan`)
```
SINGLE-SEQ .......... OUT 13 18 01 · poll 01→81 until TRIG-STATE ∈ {0,5}   self-stops, button RED
open CSV menu ....... OUT 13 11, 13 03 · poll 01→81 CONTROL-MENUID 47→48
for each enabled channel (deterministic CH1→CH2→LA):
  select Source ..... grab 20→a0 (read radio) · OUT 13 01 until radio == ch
  Save .............. OUT 13 02   (×2 if FileList closed, ×1 if already open)  → WaveData<n>.csv
  wait file ......... 0x43 ls until the file appears + size stable
  wait "busy" ....... grab 20→a0 until the orange "Operation in progress" banner clears
read back ........... OUT 10 <path> → 90  per file  → parse_csv
[delete_after] ...... OUT 13 04 ×(N+1) · 0x43 ls verify        front-panel delete, NEVER shell rm
```

Three rules are baked into the plan (details in §3.0.1): a single sequence stops at
**TRIG-STATE 5**, not 0 — never toggle Run/Stop after it; the save is **asynchronous**
("Operation in progress" banner) — press Save **once** and wait it out; a Save **leaves the
FileList open**, so every channel after the first needs **one** Save press, not two, or it
writes a spurious extra file.

Saves for all channels come first, read-backs after, so a multi-megabyte transfer never sits
between two Source changes.

### 3.0.0 The two GUI buttons

- **① Prepare** (`control::capture::prepare`) — the slow idempotent setup above, key-only.
  Run once, ~15 s.
- **② Arm capture** (`control::capture::capture`) — the fast trigger and read-back.
  **Re-pressable**: a fresh record per press with no reconfiguration.
- `control::capture::deep_capture` runs the two back to back.

### 3.0.1 The rules the DAG bakes in `[verified 2026-07-11]`

- **Reset first (idempotency).** Default Setup (keyid 21) → known state (Source=CH1, CH2 off,
  4K, default TB); card-safe; confirm via `CONTROL-MENUID == 25`, settle ~1.5 s. A capture then
  never depends on how the panel was left (the CSV Source isn't in the settings blob).
- **Single-seq is the ONLY capture mechanism — never a manual RUN→STOP.** A manual stop is not
  trigger-aligned and can latch a stale/partial buffer; every record (4K and deep) is a single-seq
  that lets the scope stop itself on a real trigger. Ensure the scope is RUNNING before arming
  SINGLE (a single-seq armed from STOPPED latches the stale buffer).
- **Single-seq stops at TRIG-STATE 5**, not 0 — the RUN/STOP button goes red at 5, and the
  stopped set is therefore `{0, 5}`. Wait for state ∈ {0,5} and **never toggle Run/Stop after**:
  read as "running", 5 turns a stop request into a start and the scope runs on through the save.
- **Verify the triggered/stopped state BEFORE the Save/Recall menu.** After the single-seq, check
  `TRIG-STATE ∈ {0,5}` before opening the CSV menu — if it is still armed/running (no trigger
  caught), Force-Trig a couple of times and, if it still never stops, **abort the save** rather
  than write a stale/empty record. `[verified 2026-07-15]`
- **V/div — raise it off the DS default.** Default Setup leaves **100 mV/div**, at which a 3.3 V
  logic signal is 33 divisions tall = **clipped off-screen** (Set-50% then can't measure it, and
  the decode is garbage). Step each enabled channel's V/div ± key (CH1 `1c`/`1d`, CH2 `22`/`23`)
  to **1 V/div**, closed-loop on `CHn-VDIV-mV`. The channel must be ON first (the V/div key is a
  no-op on a hidden channel). `[verified 2026-07-15]`
- **Trigger level: the DS default (TRIG-VPOS 0) already triggers.** A 3.3 V CMOS signal with
  channel POS = 0 sits from 0 V (low = screen centre) to +3.3 div, so its rising edges cross the
  0 V level — the scope reads TRIG'D at VPOS 0. No level-setting needed. For a *specific* level,
  step the ± keys (`2b`/`2c`); **Set-50% (`2e`) is a no-op over USB injection** (works on the
  physical key only). Force-Trig (keyid 47) is the last resort. `[verified 2026-07-15]`
- **The save is async.** After Save an orange **"Operation is in progress"** banner covers the
  FileList and the scope ignores keys; the WaveData file appears only when the temp is renamed
  at the END (~40 s for 512K). Press Save **once**, watch the banner (`0x20`, `_wait_save_done`)
  clear before the next key — never a fixed wait (a slow SD card just lasts longer).
- **A Save leaves the FileList open.** First channel = **two** Save presses (open + write); every
  later channel = **one** (write) — two would spawn a spurious extra file (the "3rd waveform").
- **Source select (deterministic).** Order CH1→CH2→LA (keyid 1 advances one). Cycle **directly,
  not via Back** — Back goes *up* to the S/R base, where keyid 1 is Ref instead. `select_source`
  reads the radio off the `0x20` framebuffer (`control::csv::selected_source`) and presses until
  it matches, so each file is correctly labelled with the channel it holds. Read-back is
  **deferred** — save every channel, then read — so a 7.7 MB transfer never sits between two
  Source changes.
- **Needs a mounted SD card** (`df /mnt/udisk` = vfat, not `ubi0:rootfs`). No card → Save is a
  silent no-op. Saving Source = **LA with the pod off** also writes nothing, which reads as a
  disturbed card and is not one. A key-only prepare (§3 ①) saves reliably. `[2026-07-15]`
- **512K is dual-channel** — CH1/CH2 come back genuinely different (86 % of samples differ, and
  decode as SPI). Delete-after uses the front-panel delete key (keyid 4), **never** shell `rm`.
- **End-of-capture cleanup.** Leave the scope live and clean: **resume RUN first** (out of the
  single-sequence stop), then press **Back** (keyid 6) to close the FileList. Key-only.
- **LA over CSV:** Source=LA exports the 16-channel pod (`#threshold` header → `is_la`); LA forces
  the record to 4K (deep memory is analog-only).

### 3.0.2 Timebase from the max signal frequency `[verified 2026-07-15]`

**Acquisition geometry:** the record is acquired over **exactly 20 divisions** with `record_len =
4000·mult` samples; `mult` = {1, 5, 10, 100, 200} for 4K / 20K / 40K / 512K / 1M. So:
```
deep samples/div = record_len / 20 = 200·mult
deep_dt          = SEC_per_div / (200·mult)
time_window      = 20 · SEC_per_div            (deep memory does NOT widen the window)
CSV rows         = record_len + 64             → 4064 / 40064 / 400064 / 800064   (1M = 800000!)
sample_rate      = 200·mult / SEC_per_div
```
(The on-screen `0x02` block is a **different** view — 3840 = 19.2 div of that 20-div record at the
base 200 samples/div. Timebase steps 2-4-8 per decade, index 0 = 2 ns/div.)

Choosing SEC/DIV is a **trade-off**: too coarse → too few samples/clock → aliased/undecodable; too
fine → the 20-div window holds too few bytes. Aim for **~10–12 samples/clock** on the fastest edge.
**The caller supplies the highest frequency to resolve** — the GUI's *Max frequency* field —
and `deep_dt = period/target` is solved for SEC/DIV:
`TDIV = period · (200·mult) / samples_per_clock` = `period·(deep_samples−64)/(20·samples_per_clock)`
— `control::capture::deep_tdiv_for_bit`, set closed-loop via the SEC/DIV ± keys. The rate is
stated by the caller, not probed off the signal.
Two limits from the sample-rate behaviour: past the ADC ceiling a finer SEC/DIV stops
helping (200 ns = 8 ns/div → same CSV); zooming out drops the rate (`rate = 200·mult/SEC_per_div`,
capped at the ADC max) — 1 ms/div on 40K is only ~2 MSa/s.

**Measured sample rates `[verified 2026-07-22, backend/src/bin/rate_sweep.rs, 4K]`.** The
`200·mult samples/div` geometry is an *upper bound*, not what you get. The scope snaps its
sample rate to a **1-2-4-8 ladder** and caps it at a **real-time ceiling that halves with the
second channel** (the ADC is shared). Measured `dt` per SEC/DIV rung:

| SEC/DIV | geometric dt | 1 ch | 2 ch |
|---|---|---|---|
| 2 ns | 0.01 ns | **1.00 ns** | **2.00 ns** |
| 8 ns | 0.04 ns | **1.00 ns** | **2.00 ns** |
| 20 ns | 0.10 ns | 1.25 ns | 2.50 ns |
| 40 ns | 0.20 ns | 1.25 ns | 2.50 ns |
| 80 ns | 0.40 ns | 1.25 ns | 2.50 ns |
| 200 ns | 1.00 ns | 1.25 ns | 2.50 ns |
| 400 ns | 2.00 ns | 2.50 ns | 2.50 ns |
| 800 ns | 4.00 ns | 5.00 ns | 5.00 ns |

So: **real-time ceiling ≈ 800 MSa/s (1 ch) / 400 MSa/s (2 ch)** — exactly halved — and even
well below it the rate-ladder snap makes the true `dt` **1.25×** the geometric figure (at
800 ns/div the geometry wants 250 MSa/s; the scope delivers 200). The 1 ns / 2 ns figures at
≤8 ns/div beat the real-time ceiling and are **equivalent-time** sampling of a repetitive
signal — not something a one-shot capture can rely on. Predicting resolution therefore means
`rate = ladder_snap(min(200·mult/SEC_per_div, ceiling(channels)))`, which is what the UI's
`capturePlan` does.

**Vertical scaling is exact.** Across every rung and both channel counts the export reported
`#voltbase` = the V/div actually set (1 V/div), and a 3.3 V logic signal read
−0.12 … +3.40 V — the CSV's volts column is already scope-calibrated, so no counts→volts
conversion is needed or wanted on the host. Examples: 40K @ 20 kHz → **101 bytes**;
512K @ 20 kHz → **1012 bytes**; 4K @ 20 MHz → 800 ns/div, 12.5 samples/clock; 2M @ 40K → 8 µs/div.

---

## 3.9 Trigger menu softkey map (menus 5 / 8 / 6 / 22 / 38 → 7 / 23 / 39)

Discovered on hardware with `cargo run -p mso5202d --bin trigger_probe`, which presses one
softkey from a known page and diffs the settings blob — whatever field moves is what the key
owns. Ring lengths are read from the value sequence the repeated presses walk through.
`[verified 2026-07-22]`

**Reaching a page.** The trigger key opens whichever page matches the **current**
`TRIG-TYPE`, so it cannot be used to get back to a page after the type has been cycled away
— it silently lands on Edge. Navigate by *type*: press `Fn1` until `TRIG-TYPE` reads the
wanted code; `CONTROL-MENUID` follows it (0→5, 1→8, 2→6, 3→22, 4→38, 5→24).

**Edge has two page ids.** `5` is where the type softkey lands, but an Edge trigger is also
shown as **`11`**, the trigger *base* page. A driver must accept either: when the type is
already Edge no softkey is pressed, so whatever page was open stays open, and a check that
demands `5` rejects a correct state. `[verified 2026-07-22]` The route that produces `11`
was not isolated — pressing the trigger key from a closed menu gives `5` every time, and
turning the trigger level knob opens no menu at all. `[gap]`

**The menu bar must be visible, not merely selected.** `CONTROL-MENUID` keeps its value
while the bar is hidden (`Fn0` toggles `CONTROL-DISP-MENU`), and softkeys sent to a hidden
bar do nothing whatsoever — indistinguishable from a run of dropped presses. Check
`CONTROL-DISP-MENU == 1` alongside the id. `[verified 2026-07-22]`

**Do not count a press that moved nothing as a step round a ring.** The key mailbox holds a
single slot and drops presses while the scope is busy, so a press that changed nothing is
far more often dropped than an end of the road. Counting it walks the ring budget to zero
while the value has not moved, and the failure reads "value unavailable in this mode" for a
value that was available all along — intermittent, and a different setting each run.
Re-press with a growing wait, and only give up after several in a row go nowhere.
`[verified 2026-07-22]`

### Page 1

| key | Edge (5) | Video (8) | Pulse (6) | Slope (22) | Overtime (38) |
|---|---|---|---|---|---|
| `Fn0` | hides / shows the menu bar (`CONTROL-DISP-MENU`) — not a setting | ← | ← | ← | ← |
| `Fn1` | **Type** — cycles `TRIG-TYPE` 0→1→2→3→4→5, and the menu id with it | ← | ← | ← | ← |
| `Fn2` | **Source** `TRIG-SRC`, ring of **5** | ring of **4** | ring of **4** | ring of **4** | ring of **2** |
| `Fn3` | **Slope** `TRIG-EDGE-SLOPE` | **Polarity** `TRIG-VIDEO-NEG` | **Polarity** `TRIG-PULSE-NEG` | **Slope** `TRIG-SLOPE-SET` | **Polarity** `TRIG-OVERTIME-NEG` |
| `Fn4` | **Mode** `TRIG-MODE` | **Standard** `TRIG-VIDEO-PAL` | **Mode** | **Mode** | **Mode** |
| `Fn5` | **Coupling** `TRIG-COUP`, ring of 5 | **Sync** `TRIG-VIDEO-SYN`, ring of 5; then the **multipurpose knob** sets `TRIG-VIDEO-LINE` | **Coupling** | **Coupling** | — |
| `Fn6` | — | — | → page 2 (**7**) | → page 2 (**23**) | → page 2 (**39**) |
| `Fn7` | opens the **Logic Analyzer** menu (61) — not a trigger key | ← | ← | ← | ← |

The source ring length is the selectable set for that type, and matches the restriction in
§8 of protocol.md: Edge = CH1/CH2/EXT/EXT-5/AC-line, Video/Pulse/Slope drop AC-line,
Overtime is CH1/CH2 only. A ring that is shorter than the enum is the *scope* refusing the
value, not a decode error.

### The slot model

Every trigger page follows the same layout, and knowing it makes the rest fall out:

- `Fn0` — the title bar (`Trigger ✕`); pressing it hides the menu, it is not a setting.
- `Fn1`–`Fn5` — the **five boxes, top to bottom**. A page with fewer boxes leaves slots
  empty, and they are empty *by position*: Overtime page 2 holds only `Coupling`, in the
  bottom slot, so it answers to `Fn5`.
- `Fn6` — the page turn.
- `Fn7` — **never press** (see below).

A softkey in an empty slot changes nothing and leaves the multipurpose knob wherever it was.
That is a trap for a diff-only probe: it reads as "this key selects the parameter the knob is
currently on", which looks like a working mapping and is not one. `[verified 2026-07-22]`

### Page 2

| key | Pulse (7) | Slope (23) | Overtime (39) |
|---|---|---|---|
| `Fn3` | — | **Vertical** — press cycles `TRIG-SLOPE-WIN` (V1 ↔ V2); the knob then tunes the selected threshold, `TRIG-SLOPE-V1` or `-V2` | — |
| `Fn4` | **When** `TRIG-PULSE-WHEN`, ring of 4 (`=` `≠` `>` `<`) | **When** `TRIG-SLOPE-WHEN`, ring of 4 | — |
| `Fn5` | **Pulse Width** — knob owns `TRIG-PULSE-TIME` | **Time** — knob owns `TRIG-SLOPE-TIME` | **Coupling** `TRIG-COUP`, ring of 5 |
| `Fn6` | back to page 1 — there is **no third page** | ← | ← |

**Overtime's time is on page 1, not page 2** — slot 5, where the other types put Coupling.
Its page 2 holds nothing but Coupling. A page-2 sweep therefore cannot find it, which is why
it was long thought unreachable. `[verified 2026-07-22]`

### What the scope tells you itself

The status bar carries two things a settings-blob diff cannot give you, and both were read
directly off `0x20` framebuffer grabs. `[verified 2026-07-22]`

- **An orange banner naming the knob and what it adjusts** — "Use V0 knob to adjust line
  number", "Use V0 knob to adjust time for overtime trigger". `V0` is the multipurpose knob.
  Where the scope says this, the parameter has no keyed entry.
- **The trigger readout**, which is the level in millivolts (`CH1 ∫ 240mV`) *except* under
  Video with Sync = LineNumber, where it shows the **line number** instead (`CH1 ⌐ 4`). The
  level is still live and still moves with the level knob in that mode — the readout simply
  gives the line the position a voltage normally occupies.

Overtime, for the avoidance of doubt, has **both**: a trigger level shown in millivolts, and
a hold time in nanoseconds adjusted with V0. Its menu has an `Overtime` box and no `Level`
box — but no trigger page has a Level box; the level is always the physical knob.

### The video line number

Only a parameter while Sync reads `LineNumber`, and it has **no box of its own** — it belongs
to the Sync box, and the knob picks it up once that box is selected. That makes it awkward to
reach: selecting the Sync box means *pressing* it, and each press advances Sync, so pressing
once to select it would move Sync off `LineNumber` and lose the parameter. Press it a whole
ring (5) instead: the box ends up selected and Sync back where it started.
`[verified 2026-07-22]`

Range is 1–525 on NTSC and 1–625 on PAL/SECAM.

### Continuous parameters

`TRIG-PULSE-TIME`, `TRIG-SLOPE-V1`, `TRIG-SLOPE-V2`, `TRIG-SLOPE-TIME`,
`TRIG-OVERTIME-TIME`, `TRIG-VIDEO-LINE` and their `TRIG-SWAP-CHx-*` counterparts have **no
keyed entry**. The softkey in their slot changes no field —
it hands the *multipurpose knob* that parameter — and the knob then moves it one step per
press. The knob's **push** does not step between parameters; it resets the current one.

**The step is fixed, and presses can be fired far faster than a verified one.** Measured on
the pulse width: 400 consecutive presses walked 0.5 → 4.5 µs at a constant **10 ns** per
press, with no scaling across the decade; the thresholds and the line number step one unit.
And a run of 40 presses landed in full at 200, 120, 80 and **50 ms** apart, beginning to drop
at 30 ms (36/40) and 20 ms (28/40). `[verified 2026-07-22]`

So these values *can* be set, not merely nudged: estimate the presses from the distance and
the step, fire them back to back at ~60 ms, read once, and make up any shortfall. A 76-step
walk costs about 8 s including the menu navigation — against roughly a minute if each press
were separately verified, and minutes more if each were a separate menu trip.

### Reading the screen instead of guessing

The `0x20` framebuffer grab is the fastest way to map a menu: it shows the softkey **labels**,
which name the keys that move no field at all — exactly where a settings-blob diff is blind.
`cargo run -p mso5202d --bin trigger_probe -- --shots <dir>` writes one 800×480 RGB frame per
trigger page. Both remaining gaps in this section were closed by looking at two of them.

### Two traps that produce confident, wrong maps

- **Press each key more than once.** A key that *steps* through the parameters a box holds
  is indistinguishable from one that selects the first, if it is only ever pressed once.
  `Fn3` on Slope page 2 looked like "selects V1" until the second press revealed V2.
- **Verify the page you are on before every press.** An early sweep reported `Fn3`/`Fn5` on
  page 2 as "navigates back to page 1". They do no such thing — the probe's own navigation
  had already dropped to page 1, and those were page-1 keys being read. Every conclusion
  from that run was wrong.

**`Fn7` must never be pressed.** It is not a menu softkey — it opens the dual-window /
logic-analyzer view, toggling `LA-SWI` and jumping to menu 61, which strands any navigation
in progress. Use `Fn0`–`Fn6` only. `[verified 2026-07-22]`

**The trigger level knob is inert in Slope.** Three presses moved `TRIG-VPOS` by 3 in Edge,
Video, Pulse and Overtime, and by **0** in Slope — on both pages, moving no other field
either. Slope compares against V1/V2 instead, so a driver must not converge on the level in
that mode: it presses until it concludes it has hit an end stop and fails the whole
configuration. `[verified 2026-07-22]`

### Alter (alternating trigger) — menu 24 and sub-pages 26–33

Alter runs a **separate trigger per channel**, alternating between them, so its settings live
in `TRIG-SWAP-CH1-*` / `TRIG-SWAP-CH2-*` rather than the main `TRIG-*` fields.
`[verified 2026-07-22]`

**Base page (24)** — `Type` box, then a `Setting` box holding two buttons:

| key | does |
|---|---|
| `Fn1` | Type — the main ring, same as every other trigger page |
| `Fn2` | **CH1** → opens that channel's page |
| `Fn3` | **CH2** → opens that channel's page |

**Channel pages.** Each channel has four, one per sub-type, and the id encodes both:

| | Edge | Pulse | Video | O.T. |
|---|---|---|---|---|
| CH1 | 26 | 27 | 28 | 29 |
| CH2 | 30 | 31 | 32 | 33 |

`Fn1` cycles `TRIG-SWAP-CHx-TYPE` over a ring of **4** — Slope and Alter are not offered —
and the menu id follows: from Edge the walk is 26 → 28 → 27 → 29 → 26 for codes 1, 2, 3, 0.
So the codes are 0 = Edge, 1 = Video, 2 = Pulse, 3 = O.T., while the *ids* run
Edge, Pulse, Video, O.T. — the two orders differ, which is easy to trip over.

`Fn6` is **Back** to menu 24, in the slot where a paged menu puts its page turn.

Slots 2–5 hold that sub-type's own settings, and the layout differs per sub-type. Read off
the screens themselves (`--alter <dir>` writes one frame per page):

| page | `Fn2` | `Fn3` | `Fn4` | `Fn5` |
|---|---|---|---|---|
| Edge (26/30) | Slope | Coupling ▲ | Coupling ▼ | — |
| Pulse (27/31) | Polarity | When | **Set PW** *(knob)* | Coupling |
| Video (28/32) | Polarity | Standard | Sync ▲ | Sync ▼ |
| O.T. (29/33) | Polarity | **Overtime** *(knob)* | Coupling ▲ | Coupling ▼ |

Two consequences that are easy to miss and that a settings-blob diff will not tell you:

- **A Video channel has no Coupling box at all.** `TRIG-SWAP-CHx-COUP` still holds whatever
  an earlier sub-type left in it, so reading it back for a Video channel reports a coupling
  the scope is not offering — and comparing it fails on a setting neither side ever applied.
- **Pulse and O.T. channels each have a knob-only value** — `TRIG-SWAP-CHx-PULSE-TIME` and
  `TRIG-SWAP-CHx-OVERTIME-TIME` — in slots 4 and 3 respectively. They sit *higher* than the
  equivalents on the main pages (slot 5), because the Alter pages carry one box fewer.

A list too long for one box gets **two** keys, one per scroll direction — the ▲▼ arrows are
drawn on screen. Measured on the Edge page: `Fn3` walked coupling 4→3→2→1 while `Fn4` walked
2→3→4→0.

**The trigger level under Alter.** The screen shows **two** trigger readouts, one per channel
(`CH1 ∏ 240mV` beside `CH2 ∫ 0.00V`), but the settings block has only one `TRIG-VPOS`. It
holds the level of whichever channel **`TRIG-SRC`** names, and the alternation moves that on
its own — so a read taken at the wrong instant reports the other channel's level as though it
were this one. Read it only while `TRIG-SRC` says the channel you mean.

**Only CH1's level is reachable by the level knob.** Twelve presses across both channel
pages walked CH1's on-screen level 0.00 V → 240 mV → 480 mV while CH2's stayed at 0.00 V
throughout — the knob does not follow the open page. `[verified 2026-07-22]` With the
acquisition **stopped** the knob moves nothing at all, on either page. How CH2's Alter level
is set is unknown. `[gap]`

**Opening a channel page mirrors that channel's settings into the main `TRIG-*` fields.**
Entering CH1's page moved `TRIG-SRC`, `TRIG-COUP` and `TRIG-EDGE-SLOPE`, none of which
describe the trigger in Alter mode — they track whichever channel page was last open. So the
main fields must **not** be read as the trigger configuration while `TRIG-TYPE` is 5; that is
why the driver reports Alter as "not readable" rather than decoding it into a single trigger.
`[verified 2026-07-22]`

**Not driven by this app** `[gap]`: Alter is mapped but not implemented — it needs a whole
per-channel configuration model, and the two channels' settings are independent of the single
`TriggerSetup` the rest of the app speaks.

---

## 4. Save→CSV softkey map (menu 47 → 48)

Verified by screenshotting each menu (`0x20`):

| Menu | keyid 1 | keyid 2 | keyid 3 | keyid 4 | keyid 5 | keyid 6 |
|---|---|---|---|---|---|---|
| **S/R** (47) | Ref | SetUp | **CSV** | — | — | — |
| **CSV** (48) | **Source** (CH1→CH2→LA) | **Save** | Recall | **delete ⚠** | FileList | Back |

- **Save is two presses** — 1st opens the FileList (destination browser), 2nd writes.
  A single press is a no-op. `[verified]`
- **`keyid 4 = delete`** — deletes card files. Its 1st press opens the FileList (first
  file selected); each further press deletes the **selected** file — **no confirm dialog**.
  `[verified 2026-07-11]`

### 4.1 Clearing the card (front-panel delete, no `rm`)

Deep CSVs are big (512K ≈ 7.7 MB, 1M ≈ 19 MB), so the card fills fast. Delete them with
the scope's own delete key — **never** a shell `rm`. Efficient loop (`clear_card` in
`backend/src/control/mod.rs`): count with **one** `ls`, press **delete `N+1` times** (1
opens the FileList, `N` delete), then **one** `ls` to verify — repeat a couple of rounds
only if the single-slot key mailbox dropped a press. `CaptureSpec::delete_after` runs it
once the captures are read back; the GUI also exposes it as "Clear all". Uses only
front-panel keys and a read-only `ls`.

**The `N+1` count applies only while the FileList is CLOSED.** The opening press is what
costs the extra one, and the FileList **stays open** afterwards — so a second round must
press exactly `N` times, not `N+1`. Pressing `N+1` with the list already open issues one
delete more than there are files to delete. `[verified 2026-07-20]`

**Blast radius is confined to CSVs.** The CSV FileList exposes only `WaveData*.csv`, so
delete presses cannot reach anything else on the card. Verified by clearing a card holding
8 CSVs (~24 MB) alongside 9 unrelated entries (`scoperoot`, `msodump`, `msoparam`,
`pic_141_*`, `cptest.txt`, `mso_test.txt`, `.Trash-1000`): all 8 CSVs went, all 9 others
survived, in 7.7 s. `[verified 2026-07-20]`
- The vendor's real save is just `MENU-SR(11) → CSV(3) → Save(2)` (its FN0/Back
  presses in captures are the operator escaping the slow, stuck app — not part of
  saving). `[verified from the virtual-panel pcap]`

---

## 5. Timing & waits

| Step | Wait | Why |
|---|---|---|
| After a key press | ~0.3–0.8 s + poll | slow key-scan loop; verify the transition landed |
| Knob-only parameter walk | ~60 ms between presses | fixed step per press; fire in bursts, read once (§3.9) |
| RUN → STOP transition | press-until-`TRIG-STATE`-observed | Run/Stop is a toggle; presses drop |
| SINGLE arm → capture | poll `TRIG-STATE` up to ~25 s | waits for a signal edge |
| Save 1st→2nd press | ~0.6–0.8 s | let the FileList open |
| After 2nd Save press | poll file **size until stable** | deep CSV writes take seconds (512K ≈ tens of s) |
| File read-back (0x10) | ~800 KB/s | 512K ≈ 7.7 MB ≈ 10 s; 1M ≈ 19 MB ≈ 25 s |

`await_new_file` scales its hard timeout with depth (`save_timeout`: 4K 30 s … 1M 220 s) but
returns as soon as the file size stabilises.

### 5.1 Making capture fast — skip work, act on state

- **Split setup from capture.** Everything slow and idempotent is in prepare, so the
  re-pressable capture does only the trigger, the export and the read-back. A second record
  with the same configuration costs nothing extra.
- **Converge, don't sleep.** Every step polls its own read-back and moves on the moment the
  value lands, rather than waiting a fixed worst-case delay (`control::converge`).
- **Act on the confirmed state.** A step that reads back wrong is re-applied, not reported —
  presses drop, and a dropped press is far more common than a real end stop (§3.9).
- **Burst the knob-only walks.** Verifying every press of a several-hundred-step walk costs
  minutes; firing at ~60 ms and reading once costs seconds (§3.9).
- **Resync drains in 64 KB chunks with a 60 ms timeout** (bounded, ~4 s worst case), so an
  occasional desync costs a second or two rather than tens of seconds (§8.10).

---

## 6. Failure modes & recovery

| Symptom | Cause | Recovery |
|---|---|---|
| **"USB device undetected, operation fail"** on Save | card detection disturbed by a USB reset or a `0x11` write | connect without a reset and configure by key only; a physical panel press re-detects the card |
| **Scope reboots** (USB re-enumerates) on depth change | `0x11` `ACQURIE-STORE-DEPTH` write while **running** | set depth with the Acquire F5 softkey (§0 rule 4a), never a block write |
| **Empty screen / no data** after capture | manual RUN→STOP froze before/without a trigger | use the single sequence with a trigger level on the signal; never manual-stop |
| **SINGLE never completes** (`TRIG-STATE` stays armed) | trigger level not on the signal | move `TRIG-VPOS` onto the signal; Force-Trig as a last resort, then abort rather than save a stale record |
| **`ls /mnt/udisk` returns empty** intermittently | shell one-behind race | retry the read (the card in use always has files) |
| **Save no-ops, no file, no `/dsocsv.tmp`** | no SD card mounted | insert the card; confirm `df /mnt/udisk` shows a vfat device |
| **Malformed `0x43 0x01`** crash-reboot | bare `0x01` with no LE16 count arg | never send it; it's the acquisition engine anyway, not an LA tap |
| **Link desync** (`bad SOF`) | stale RX bytes from a prior session | resync (§8.10) |
| **`bad leader 0x43`** on a `0x53` read | a trailing shell (`0x43`) reply lingered and was read by the next data-channel request | validate the reply leader **inside** the retry loop → resync + re-send (§8.3/§8.4); resync at the end of a shell command |
| **`bad leader 0x53` (wanted `0x43`)** during a shell `ls` | leftover `0x53` framebuffer frames bled into the shell read | collect the framebuffer to its real END so nothing is left queued (§8.6); resync before/after each grab (§8.7) |
| **`framebuffer too short: 212 B`** on a screen grab | a one-behind stale settings block (`0x81`, whose 2nd byte can look like a DATA subtype) was read as the framebuffer | retry the grab with a resync around it, accept only a full screen (§8.7) |
| **probe/menu key fires twice** | a fire-and-forget key press was retried on a stale reply | never re-validate a `0x13` key by its reply echo — keys are non-idempotent (§8.5) |

---

## 7. Shell (`0x43`) interaction notes

The root shell (`backend/src/device/shell.rs`; `scripts/mso5202d_shell.py` is the
interactive REPL version) is used here only for read-only card checks (`df /mnt/udisk`,
`ls /mnt/udisk`). Quirks it handles (protocol.md Appendix F.4):
firmware appends `> msg` to the command (wrap multi-command in `{ …; }`), the reply
can race one command behind (unique end-marker + retry), and a stalled command trips
the watchdog reboot — so keep shell commands short and never read a live process's
`/proc/<pid>/*`. Reading files off the scope is done with the `0x53 0x10` file-read,
not shell `cat`.

**The shell goes unreachable while a large record is being written.** During a deep
Save→CSV the scope stops answering `0x43` altogether: every command times out for
tens of seconds until the write finalises (measured on a 512 K / 7.7 MB export —
repeated 4 s timeouts, save wall-clock 50 s). The poll that waits for the new CSV
therefore **must treat a failed `ls` as "still busy" and keep waiting**, not as an
error; aborting on the first timeout kills a save that was progressing normally.
The same poll must still respect the no-re-press rule (§3.0.1): the file only
becomes visible when the scope renames its temp file at the very end.
[verified 2026-07-20]

---

## 8. USB transport layer — the full host-side stack

Everything above is *what* to send; this section is *how* the host must move bytes so the
scope answers reliably. It is implemented by `Transport` in `backend/src/usb/transport.rs`,
and identically by the Python reference `Scope` (`scripts/mso5202d.py`); the byte layout of a
frame lives in `MSO5202D-protocol.md` §2, this is the procedural handling around it. Two bulk
endpoints only:
**OUT `0x02`, IN `0x81`**. Every quirk below was needed to make prepare+capture work end to
end (4K / 40K / 512K, single- and dual-channel, `[verified 2026-07-20]`).

### 8.1 The transaction primitive — reader thread *before* the write

**The device only answers an OUT command if a bulk-IN read is already posted when the command
is written.** libusb's synchronous read blocks, so a transaction is:

```
spawn reader thread ──▶ reader signals "about to post the IN read" ──▶ post bulk-IN read (blocks)
main thread waits for that signal ──▶ sleep TRANSACT_POST (~15 ms) ──▶ write the OUT frame
                                   ──▶ join the reader ──▶ it returns the reply frame
```

`TRANSACT_POST` is the margin for the IN URB to actually reach the kernel before the OUT
races it. Below ~12 ms the write beats the read and the device goes **silent** (the
transaction times out); 15 ms is measured-reliable with headroom, and latency is
USB-round-trip-limited from there down, so lower is not faster. Re-tune per host with
`scripts/tune_transact.py`. If the write reports an error but the reader still got a frame,
**prefer the received frame** — the reply is what matters.

### 8.2 Frame assembly & the persistent RX buffer

Reads are accumulated into a **persistent RX buffer** that survives across transactions, so a
read that returns more than one frame's worth of bytes keeps the tail aligned for the next
read. Assembly: read until ≥ 3 bytes (leader + `len_LE16`), compute `total = len + 3`, read
until the buffer holds `total`, split off exactly that frame, leave the rest buffered. Chunk
size differs by implementation and does **not** affect correctness — Python reads 512 B per
`dev.read`, Rust reads 64 KB per `read_bulk`; both accumulate to a whole frame either way.

### 8.3 Validation lives INSIDE the retry loop *(the load-bearing rule)*

A transaction retries (default 2) with a `resync()` between attempts, and **the reply is
validated inside that loop, not after it.** On any validation failure — bad leader, wrong
length, bad checksum — the attempt resyncs and re-sends. Moving the check outside the loop (so
a bad frame errors instead of retrying) is exactly what made a stale cross-channel frame fatal
instead of self-healing. For the data channel validation is a full `verify` (leader `0x53` +
length + checksum); see §8.4 for the command channel.

### 8.4 Two channels share the endpoints → the leader-match rule

The data channel (`0x53`) and the command/shell channel (`0x43`) use the **same** bulk
endpoints. A reply frame's leader always equals its request's leader (`0x53`→`0x53`,
`0x43`→`0x43`). So the in-loop validation requires **the reply leader to match the request
leader**; a mismatch means a frame from the *other* channel was read — typically a shell reply
still dribbling out when the next data request posts its read (`bad leader 0x43`) — and it is
rejected → resync → re-send. To keep it from happening in the first place, **`resync()` at the
end of every shell command** (§8.11) so no trailing `0x43` frame is left for the next `0x53`
read. Command-channel replies carry no checksum we can trust, so they are validated by leader
only (their content is delimited by the shell marker, §7).

### 8.5 The one-behind race — and why keys are NOT echo-validated

A slow reply can arrive one request late: a delayed `0x01` settings reply (`0x81` echo) can be
read as the *next* request's first frame. It is a valid `0x53` frame, so leader+checksum pass —
only its **selector echo** (`0x81`) is wrong for, say, a `0x20` framebuffer request (which
echoes `0xa0`). It is tempting to also validate `reply[0] == selector | 0x80` and retry on a
mismatch — **do not.** A `0x13` key press is **fire-and-forget and NOT idempotent**: retrying
one because its ack was a one-behind frame **double-presses the key** (observed as a probe/menu
softkey advancing two ring steps instead of one). Python never validates the echo for this
reason. Instead the one-behind stale frame is absorbed where it lands: the multi-frame
collector skips it (§8.6) and the framebuffer grab resyncs and retries (§8.7).

### 8.6 Multi-frame replies — consume the END, skip everything else

A large reply is a **size** frame (subtype `0x00`), any number of **data** frames (`0x01`),
then an **end-marker** frame (`0x02`). The end marker **must be consumed** or the next read
starts mid-stream. The collector therefore:

- **appends** on subtype `0x01`,
- **stops** only on subtype `0x02` (END),
- **skips** anything else (a SIZE frame, or a stale one-behind frame) and keeps reading,

bounded by a large max frame count as a backstop. Stopping on the *first* non-data frame was a
real bug: a stale frame in front of the framebuffer ended collection early and left ~768 KB of
`0x53` frames queued, which then surfaced as `bad leader 0x53` inside the following shell `ls`.
Data offsets differ by reply: file-read and framebuffer data start at **offset 2** (echo +
subtype); acquire data starts at **offset 3** (echo + subtype + source byte, §8.8).

### 8.7 Framebuffer grab (`0x20` → `0xa0`, ~768 KB)

Reading the screen (for the CSV Source radio and the save banner) moves the whole framebuffer —
exactly `800·480·2` bytes across ~75 frames, **no size frame**. It is the grab most exposed to
desync, so:

- **`resync()` before *and* after every grab.** Before → start from a clean endpoint so a
  leftover frame is never mistaken for the first framebuffer frame; after → a 768 KB transfer
  can leave a tail that would bleed into the next command.
- **Retry the whole grab (up to 4×)** and **accept only a full-size screen.** A short/garbled
  grab (a one-behind stale frame, or the scope momentarily busy) is retried, never accepted.
- Convert RGB565 LE → RGB888 only after a full grab. `[verified 2026-07-20]`

### 8.8 Waveform acquire (`0x02 01 <ch>` → `0x82`)

Screen-buffer samples for one analog channel. Two quirks: (1) write a **`12 01 00` latch
first** (the vendor app pulses `0x12` around every acquire); (2) the data frame is `echo |
0x01 | src | samples…`, so samples start at **offset 3**. The channel switch is **one-deep
pipelined** — the *first* read after changing `<ch>` returns the *previous* channel's block —
so a caller comparing channels must **read twice and keep the second** (this is how a channel's
on/off state is verified in prepare, §4b: an empty acquire = channel off). A disabled channel
answers empty. Retries with a resync between, like every other read. (`02 01 05` LA is broken —
never used; §protocol.md §5.)

### 8.9 File read (`0x10 <path>` → `0x90`)

Multi-frame like §8.6 (offset 2), looped until the END marker, so it handles files far larger
than the 64 KB single-frame cap — a 512 K export is ~7.7 MB and reads back at ~800 KB/s
(≈10 s). After the transfer, `resync()` to clear any tail. A path the scope cannot serve
(e.g. the running `/dso_bin`) answers with a single byte forever, so cross-check the returned
length against the card's `ls` size when truncation would matter.

### 8.10 `resync()` — draining to a clean boundary

Called after a timeout, a bad frame, or a large transfer. It clears the RX buffer, then reads
the endpoint dry in **large chunks with a short (~60 ms) timeout**, bounded (~64 iterations, a
few seconds worst case) so a desync costs a second or two rather than tens of seconds. It keeps
reading until a read comes back empty/times out — it must **not** stop on a partial read,
because the scope dribbles frames and a premature stop leaves a trailing frame that re-desyncs
the next transaction. An interrupted big read (file or framebuffer) can leave hundreds of KB
queued, which the 64 KB chunks clear in ~16 iterations.

### 8.11 Shell channel (`0x43`) framing around a command

A shell exchange sends `0x43 0x11 "{ cmd; echo <marker>; }"` and reads the first `0x43` frame
plus continuation frames until a short read (the command channel has no END marker — "no more
output" is a read timeout). It **resyncs before** the command (drop any stale queued reply),
re-issues up to a few times until the reply carries the unique **end-marker** (replies race one
command behind), and **resyncs after** the marker is found so no trailing `0x43` frame is left
for the next data-channel read (§8.4). Keep commands short and read-only — a stalled command
trips the watchdog reboot (§7).

**That retry loop must absorb transport failures too, not just a wrong marker.** Two different
faults land here: a reply that lags one command behind, and an exchange that fails outright
because a leftover **data-channel** frame was read where the `0x43` reply belonged
(`bad leader 0x53`) — which a big `0x53` transfer just beforehand, such as a multi-megabyte
file read, can leave queued. The resync at the top of each attempt clears either, so a
transport error must be **retried, not propagated**: returning it immediately skips the very
resync that would have fixed it, and the command fails on a condition the next attempt would
have ridden out. `[verified 2026-07-21]`

### 8.12 Wire logging for diffing against the reference driver

Set **`MSO_USB_LOG=1`** (or `=stderr`, or `=/path/to/file`) to log every OUT frame, IN chunk,
and DRN (resync-drained) chunk with a millisecond timestamp and hex head — in the **same line
format** in both the Rust transport and, via `scripts/_usbcompare.py` (a pyusb read/write
monkeypatch), the Python driver. Running the same capture through both and diffing the two
traces — selector sequence, frame lengths, and inter-frame delays — is how each transport
divergence above was pinned down; keep it as the first tool when the two disagree.
`[verified 2026-07-20]`
