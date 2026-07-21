# MSO5202D — state machines & interaction flows

How to *drive* the scope over USB: the command sequences, the waits, and the
verification between steps. This is the procedural companion to the two reference
docs — `MSO5202D-protocol.md` (the byte-level wire protocol) and
`MSO5202D-rendering.md` (turning samples into a trace). Everything here is
hardware-verified; the flows are what `scripts/mso5202d_plot.py`,
`scripts/mso5202d_decode.py`, and `scripts/mso5202d_shell.py` implement, and what the
Rust backend (`backend/src`) mirrors one-to-one — the host-side USB stack that carries
them all is §8.

The scope is a small embedded instrument with a slow key-scan loop and a
**fragile USB/SD-card coupling**. Driving it reliably is less about individual
commands than about *ordering, pacing, and reading state back between steps*.

---

## 0. Golden rules

1. **Always read the settings between commands.** Poll `0x01` (→ `decode_settings`)
   to see the real state — `TRIG-STATE`, `CONTROL-MENUID` — before the next key or
   write. Never fire a key blind and assume it landed (the key mailbox is
   single-slot and the scan loop is slow). This is *closed-loop* control.

2. **Do NOT `dev.reset()` when the SD card is needed.** A USB reset disturbs the
   scope's USB **host** controller (the same silicon that hosts the SD flash
   drive), dropping the card → Save→CSV fails with **"USB device undetected."**
   Connect with `Scope(reset=False)` for any deep-capture/save flow. `[verified]`

3. **Minimise `0x11` settings-block writes around a save.** A `0x11` write also
   disturbs card detection (same failure mode as the reset). The vendor virtual
   panel **never** writes `0x11` and **never** resets — it drives everything via key
   events and only *reads* settings. Do `0x11` prep *before* the save path, not
   inside it, and expect that a physical panel press (or menu re-navigation) may be
   needed to re-detect the card if the flow was `0x11`-heavy. `[verified]`

4. **Prefer buttons to `0x11` writes; never stop during prepare.** Anything with a
   front-panel LED/menu indicator (store depth, channel on/off, trigger source) must be
   driven by **key events**, not `0x11` settings writes — a `0x11` write can change the
   field yet leave the physical indicator wrong (see 4a/4b), and a `0x11` *depth change*
   on a running scope crash-reboots it. The scope stays **RUNNING** the whole prepare; the
   only STOP is the capture single-seq. (Shell `0x43` and raw reads `0x01`/`0x02`/`0x10`/
   `0x20` are the only non-button operations.) `[verified 2026-07-11]`

4a. **Set store depth with the Acquire-menu F5 softkey, single-edge + poll — not `0x11`.** A
   `0x11` depth write leaves the on-screen **LongMem radio stale at 4K** (scope moves slower
   yet the menu lies), so walk the visible menu: open Acquire (keyid 13 → `CONTROL-MENUID` 17),
   then step F5 (keyid 5) through the ring `4K→40K→512K→1M→(4K)` (codes 0→4→6→7). **F5 advances
   one step per `0x13` frame** (one key event per frame; the 2nd byte is a don't-care). Send **one
   frame per step** and, after each, **poll `ACQURIE-STORE-DEPTH` until it reaches the next step**
   before sending the next — one frame = one step, so never send a second frame for the same step.
   No render delay — the field settles within ~1–2 s and the poll catches it; whole ring 4K→1M in
   ~4 s (`_set_depth_via_keys`). From the Default-Setup 4K start it takes exactly the ring distance.
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
   needs CH2 off first (`_set_channels_via_keys`). **`TRIG-SRC` is likewise not writable via
   `0x11`** (verified: a write stays on the old source) — trigger source needs its own menu keys
   (not yet wired; trigger currently stays on CH1). `[verified 2026-07-11]`

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

`Scope(reset=False)`. The kernel auto-binds `cdc_subset` (the VID:PID is that
driver's default), so it must be detached, but the `dev.reset()` in the default
recipe is what breaks the card — skip it. A no-reset connection is otherwise fully
functional (settings, waveform, framebuffer, keys, file-read all verified over it).
Stale RX bytes after a dirty prior session are cleared with `_resync()`.

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

**Return to the main screen** (close any open side menu): write
`CONTROL-DISP-MENU = 0` via `0x11`. `[verified]` (This is a `0x11` write — do it at the
*end* of a flow, not just before a save; see rule 3.)

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
| settings write | `11` + 213 B | `91` | configure (prep); scope busy ~3.4 s after |
| key event | `13 <keyid> <state>` | `93` | front-panel key (softkey ids in §4) |
| framebuffer | `20` | `a0` (~768 KB) | read the screen — Source radio, save banner |
| acquire | `02 01 <ch>` | `82` size/data/end | screen-buffer waveform (**probe only**) |
| file read | `10 <path>` | `90` multi-frame | pull a WaveData CSV back (~800 KB/s) |
| shell `ls` | `0x43 11 "ls …"` | `0x91` + stdout | list `/mnt/udisk` (read-only) |

### ① `prepare_capture()` — slow, once — **KEY-ONLY (settings memory is READ-only)**
Every step is a front-panel key; the settings block is only *read* (`01`→`81`) to verify. No
`0x11` config write — a `0x11` sets the field but skips the key handler's side-effects (LED,
on-screen radios, acquisition reconfig, SD-card detection), so config-by-`0x11` is what broke
Save→CSV and rebooted the scope. The scope stays RUNNING throughout (only the capture stops it).
```
Default Setup ....... OUT 13 21 01 · poll 01→81 MENUID==25     idempotent known state, all side-effects
channels on ......... OUT 13 18 (CH1) / 13 1e (CH2) · verify    CH buttons (LED + acquisition correct)
                       via 4K wave-data
V/div → 1 V ......... OUT 13 1d/1c (CH1 ±) 23/22 (CH2 ±)        off the 100 mV DS DEFAULT — a 3.3 V
                       · poll CHn-VDIV-mV until 1000            signal at 100 mV/div is 33 div (clipped!)
set depth (F5) ...... OUT 13 0d (open Acquire 17) · OUT 13 05   cycle LongMem 4K→40K→512K→1M until depth
                       · poll 01→81 until depth==target
SEC/DIV ............. OUT 13 28/29 · poll ns/div (=SI×200)      step to target; known rate → compute it,
                                                               else auto-probe. ±labels are INVERTED
trigger level ....... left at TRIG-VPOS 0 (DS default)          0 V already triggers a 3.3 V CMOS signal
                                                               (its low rail = 0 V = screen centre)
                                                               → leaves scope RUNNING, ready
```
Knob key ids used (keyprotocol.inf §9.2): CH1 V/div −/+ = `1c`/`1d` (28/29), CH2 = `22`/`23`
(34/35), SEC/DIV = `28`/`29` (40/41, slower/faster **inverted** — resolve from the read-back),
trigger level −/+/push = `2b`/`2c`/`2d` (43/44/45). Set-50% = `2e` (46) is a **no-op over USB
injection** (works on the physical key) — not used. The ONE remaining `0x11` is the LA-pod
enable (`LA-SWI`, no mapped key yet).

### ② `capture_prepared()` — fast, re-pressable
```
SINGLE-SEQ .......... OUT 13 18 01 · poll 01→81 until TRIG-STATE ∈ {0,5}   self-stops, button RED
open CSV menu ....... OUT 13 11, 13 03 · poll 01→81 CONTROL-MENUID 47→48
for each enabled channel (deterministic CH1→CH2→LA):
  select Source ..... grab 20→a0 (read radio) · OUT 13 01 until radio == ch
  Save .............. OUT 13 02   (×2 if FileList closed, ×1 if already open)  → WaveData<n>.csv
  wait file ......... 0x43 ls until the file appears + size stable
  wait "busy" ....... grab 20→a0 until the orange "Operation in progress" banner clears
read back ........... OUT 10 <path> → 90  per file  → parse_wavedata_csv
[delete_after] ...... OUT 13 04 ×(N+1) · 0x43 ls verify        front-panel delete, NEVER shell rm
```

Three rules are baked into the DAG (details in §3.0.1): a single-seq stops at **TRIG-STATE 5**
(not 0) — never toggle Run/Stop after; the save is **async** ("Operation in progress" banner) —
press Save **once** and wait it out; a Save **leaves the FileList open**, so channels after the
first need **one** Save press, not two (else a spurious extra file).

> **Historical note:** a removed 4K-only fast path read the screen buffer **directly** over
> `0x02` (freeze + double-read; the `0x02` channel switch is one-deep pipelined, protocol.md §5).
> The direct read still lives in `_direct_acquire` — used only by the auto-timebase probe above.

### 3.0.0 Two GUI buttons

- **① `prepare_capture()`** — slow idempotent SETUP, **all via key presses** (no `0x11`): Default
  Setup → channels on → V/div → depth (F5) → SEC/DIV. Everything is set by stepping a knob key and
  polling the read-back; the scope stays **running** throughout. Run once (~15 s).
- **② `capture_prepared()`** — the fast trigger + read-back (the ② DAG above). **Re-pressable**
  (a fresh record per press, no re-configure). `deep_capture()` = the two back-to-back.

### 3.0.1 The rules the DAG bakes in `[verified 2026-07-11]`

- **Reset first (idempotency).** Default Setup (keyid 21) → known state (Source=CH1, CH2 off,
  4K, default TB); card-safe; confirm via `CONTROL-MENUID == 25`, settle ~1.5 s. A capture then
  never depends on how the panel was left (the CSV Source isn't in the settings blob).
- **Single-seq is the ONLY capture mechanism — never a manual RUN→STOP.** A manual stop is not
  trigger-aligned and can latch a stale/partial buffer; every record (4K and deep) is a single-seq
  that lets the scope stop itself on a real trigger. Ensure the scope is RUNNING before arming
  SINGLE (a single-seq armed from STOPPED latches the stale buffer).
- **Single-seq stops at TRIG-STATE 5**, not 0 — the RUN/STOP button goes red at 5.
  `_STOPPED_STATES = {0,5}`. Misreading 5 as "running" made `_run_stop` toggle it back into RUN
  (the "512K kept running / only CH1" bug). Wait for state ∈ {0,5}; **never toggle Run/Stop after**.
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
  not via Back** (Back goes up to the S/R base where keyid 1 = Ref). `_select_source` reads the
  radio off the `0x20` framebuffer and presses until it matches → files are CH1, CH2, … correctly
  labelled (no blind cycling / "CH2 twice"). Read-back is **deferred** (save all, then read) so a
  7.7 MB read doesn't sit between Source changes.
- **Needs a mounted SD card** (`df /mnt/udisk` = vfat, not `ubi0:rootfs`). No card → Save is a
  silent no-op. Saving Source = **LA with the pod off** also writes nothing (a common false
  "card disturbed"). **Configuring by `0x11` instead of keys was associated with Save→CSV writing
  no file and with reboots; the fully key-only prepare (above) saves reliably.** A `0x11` field
  write does not run the key handler's side-effects (LED / on-screen radio verified; card
  detection empirically), so drive config by key and only *read* settings memory. `[2026-07-15]`
- **512K is dual-channel** — CH1/CH2 come back genuinely different (86 % of samples differ, decode
  as SPI). Delete-after uses the front-panel delete key (keyid 4), **never** shell `rm`.
- **End-of-capture cleanup.** Leave the scope live + clean: **resume RUN first** (from the
  single-seq STOP), then press **Back** (keyid 6) to close the FileList. Key-only — the side menu
  staying visible is cosmetic and the next capture's Default Setup clears it, so there is no need
  to write `CONTROL-DISP-MENU = 0` (the old `0x11` menu-hide, now dropped along with every other
  config write).
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
**The caller supplies the highest frequency to resolve** (the GUI's `MAX_SIGNAL_FRQ` field, e.g.
`20M`) and we solve `deep_dt = period/target` for SEC/DIV:
`TDIV = period · (200·mult) / samples_per_clock` = `period·(deep_samples−64)/(20·samples_per_clock)`
— `deep_tdiv_for_bit()`, set closed-loop via the SEC/DIV ± keys. No signal probe: the earlier
`auto_tb` (`_probe_pulse_ns`/`_direct_acquire`) was **removed** — you know the rate, so you state it.
Two limits from the sample-rate behaviour (§below): past the ADC ceiling a finer SEC/DIV stops
helping (200 ns = 8 ns/div → same CSV); zooming out drops the rate (`rate = 200·mult/SEC_per_div`,
capped at the ADC max) — 1 ms/div on 40K is only ~2 MSa/s. Examples: 40K @ 20 kHz → **101 bytes**;
512K @ 20 kHz → **1012 bytes**; 4K @ 20 MHz → 800 ns/div, 12.5 samples/clock; 2M @ 40K → 8 µs/div.

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
the scope's own delete key — **never** a shell `rm`. Efficient loop (`_clear_wavedata` /
`clear_wavedata` in `mso5202d_plot.py`): count with **one** `ls`, press **delete `N+1`
times** (1 opens the FileList, `N` delete), then **one** `ls` to verify — repeat a couple
of rounds only if the single-slot key mailbox dropped a press. `deep_capture(delete_after=
True)` runs this after the captures are read back to the PC; there's also a "Clear card
CSVs" button in the GUI. Uses only front-panel keys + read-only `ls` (no `rm`).

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
| **After a `0x11` write** | **scope busy ~3.4 s** | the block reapplies the whole 213-byte config; **the next read blocks until it's done** (verified: first `0x01` read after a write = 3.4 s, subsequent reads 0.02 s) |
| RUN → STOP transition | press-until-`TRIG-STATE`-observed | Run/Stop is a toggle; presses drop |
| SINGLE arm → capture | poll `TRIG-STATE` up to ~25 s | waits for a signal edge |
| Save 1st→2nd press | ~0.6–0.8 s | let the FileList open |
| After 2nd Save press | poll file **size until stable** | deep CSV writes take seconds (512K ≈ tens of s) |
| File read-back (0x10) | ~800 KB/s | 512K ≈ 7.7 MB ≈ 10 s; 1M ≈ 19 MB ≈ 25 s |

`_wait_new_csv` scales its hard timeout with depth (4K 30 s … 1M 220 s) but returns
as soon as the size stabilises.

### 5.1 Making capture fast — skip work, act on state `[verified 2026-07-11]`

The `0x11`-write busy period (~3.4 s) is the single biggest avoidable cost, so:

- **Only write `0x11` when a field actually changed.** `_prep_block` builds the target block
  from the *current* settings and writes only if it differs — on a repeat capture the scope
  is already configured, so prep is free (no write, no 3.4 s busy). This alone turns a repeat
  4K capture from ~13 s into **~2.8 s**.
- **Don't restore with a `0x11` write on the 4K path.** Leaving the scope stopped + configured
  (no depth/menu restore write) means the *next* capture's prep also finds nothing changed and
  skips its write. (The deep/CSV path still restores depth to 4K — it genuinely changed it.)
- **4K reads never touch the SD card.** The direct `0x02` path returns before opening the shell
  or `ls`-ing the card (an `ls /mnt/udisk` alone costs ~10 s).
- **Act on the confirmed state, don't just print it.** After prep, re-apply until the depth
  reads back correct (a write can silently miss right after a burst of rapid captures); after a
  direct read, re-freeze + re-read if a channel came back empty/garbled (`_has_signal`).
- **`_resync` drains in 64 KB chunks with a 60 ms timeout** (bounded ~4 s), so an occasional
  desync costs a second or two, not the tens of seconds a small-chunk drain took.

---

## 6. Failure modes & recovery

| Symptom | Cause | Recovery |
|---|---|---|
| **"USB device undetected, operation fail"** on Save | card detection disturbed by a `dev.reset()` or a `0x11` write | connect `reset=False`; keep `0x11` out of the save path; a physical panel press re-detects the card |
| **Scope reboots** (USB re-enumerates) on depth change | `0x11` `ACQURIE-STORE-DEPTH` write while **running** | STOP first, then write depth |
| **Empty screen / no data** after capture | manual RUN→STOP froze before/without a trigger | use SINGLE-seq with a trigger level on the signal; don't manual-stop |
| **SINGLE never completes** (`TRIG-STATE` stays armed) | trigger level not on the signal | set `TRIG-VPOS` mid-logic; the data is still captured even if STATE≠0 |
| **`ls /mnt/udisk` returns empty** intermittently | shell one-behind race | retry the read (the card in use always has files) |
| **Save no-ops, no file, no `/dsocsv.tmp`** | no SD card mounted | insert the card; confirm `df /mnt/udisk` shows a vfat device |
| **Malformed `0x43 0x01`** crash-reboot | bare `0x01` with no LE16 count arg | never send it; it's the acquisition engine anyway, not an LA tap |
| **Link desync** (`bad SOF`) | stale RX bytes from a prior session | `_resync()` |
| **`bad leader 0x43`** on a `0x53` read | a trailing shell (`0x43`) reply lingered and was read by the next data-channel request | validate the reply leader **inside** the retry loop → resync + re-send (§8.3/§8.4); resync at the end of a shell command |
| **`bad leader 0x53` (wanted `0x43`)** during a shell `ls` | leftover `0x53` framebuffer frames bled into the shell read | collect the framebuffer to its real END so nothing is left queued (§8.6); resync before/after each grab (§8.7) |
| **`framebuffer too short: 212 B`** on a screen grab | a one-behind stale settings block (`0x81`, whose 2nd byte can look like a DATA subtype) was read as the framebuffer | retry the grab with a resync around it, accept only a full screen (§8.7) |
| **probe/menu key fires twice** | a fire-and-forget key press was retried on a stale reply | never re-validate a `0x13` key by its reply echo — keys are non-idempotent (§8.5) |

---

## 7. Shell (`0x43`) interaction notes

The root shell (`mso5202d_shell.py`) is used here only for read-only card checks
(`df /mnt/udisk`, `ls /mnt/udisk`). Quirks it handles (protocol.md Appendix F.4):
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
scope answers reliably. It is implemented identically in the Python `Scope` (`mso5202d.py`)
and the Rust `Transport` (`backend/src/usb/transport.rs`); the byte layout of a frame lives in
`MSO5202D-protocol.md` §2, this is the procedural handling around it. Two bulk endpoints only:
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
