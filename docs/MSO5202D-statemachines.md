# MSO5202D — state machines & interaction flows

How to *drive* the scope over USB: the command sequences, the waits, and the
verification between steps. This is the procedural companion to the two reference
docs — `MSO5202D-protocol.md` (the byte-level wire protocol) and
`MSO5202D-rendering.md` (turning samples into a trace). Everything here is
hardware-verified; the flows are what `scripts/mso5202d_plot.py`,
`scripts/mso5202d_decode.py`, and `scripts/mso5202d_shell.py` implement.

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

4. **Change store depth only while STOPPED.** Writing `ACQURIE-STORE-DEPTH` via
   `0x11` on a *running* scope **crash-reboots** it. Stop first, then write. `[verified]`

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

## 3. Capture — two paths by depth

`deep_capture()` in `mso5202d_plot.py` chooses one of two capture/read paths:

### 3.0 4K (screen) analog — direct `0x02` read, no SD card `[verified 2026-07-11]`

For a 4K analog capture (the common serial-decode case), skip the SD card entirely:

```
STOP → 0x11 prep (channels + trigger + depth 4K) → RUN→STOP FREEZE → 0x02 read CH1, CH2
```

- **RUN→STOP freeze, not SINGLE.** SINGLE waits for a specific edge and — on a bursty
  serial line — can stay *armed* (TRIG-STATE=5) indefinitely, then Force-Trig grabs a
  mostly-idle window (verified: ~14 edges, undecodable). A RUN→STOP freeze lets the scope
  acquire the live stream, so the frozen window is full of signal (verified: ~65 edges,
  SPI decodes to the ramp `3A 3B 3C 3D`). Freeze also captures **one simultaneous
  2-channel acquisition**, so CH1/CH2 are inter-channel aligned.
- **Double-read each channel** — the `0x02` channel switch is one-deep pipelined
  (protocol.md §5): the first read after switching returns the previous channel, so read
  twice and keep the second, else CH1 and CH2 come back byte-identical.
- Fast (~15 s vs ~2 min for the CSV route) and needs no card. This is what the plotter's
  "Trigger & Capture" uses at 4K for UART/SPI/I²C decoding.

LA is **not** available this way (`02 01 05` is broken) — an LA capture falls through to
the CSV route below.

### 3.0.0 Prepare / Capture split `[verified 2026-07-11]`

Capture is two phases (two GUI buttons, like a bench scope: set up once, hit Single often):

- **① `prepare_capture()`** — the slow, idempotent SETUP: Default Setup + configure channels/
  trigger/depth (+ auto-timebase probe). The depth is written while STOPPED (a running-scope
  depth write reboots it), then the scope is left **running** (live) and ready. Run once.
- **② `capture_prepared()`** — the fast trigger + read-back: SINGLE-SEQ (deep) or RUN→STOP
  freeze (4K) → bring back every enabled channel. **Re-pressable** — each press grabs a fresh
  record with no re-configure (4K ≈ 1.7 s each; Prepare ≈ 6 s once). `deep_capture()` is just
  the two called back-to-back (headless/tests). NB a repeated *deep* capture starts the CSV
  Source wherever the last cycle left it, so labels may swap (the decoder tries both orderings;
  re-Prepare for a guaranteed CH1-first).

### 3.0.1 Idempotent reset + timebase spread `[verified 2026-07-11]`

**Reset to a known state first (idempotency).** A capture should not depend on how the panel
was left — the CSV Source in particular persists and is not readable. Press **Default Setup
(keyid 21)** before configuring: it is **card-safe** (a Save straight after DS writes a file)
and resets to a known state — **CSV Source = CH1**, CH2 off, depth 4K, a default timebase — so
a deep multi-channel save then cycles CH1→CH2 deterministically. Confirm it landed via
`CONTROL-MENUID == 25` (the DefaultSetup menu), then let it settle ~1.5 s. `_prep_block`
re-enables CH2 and sets probe/coupling/V-div/trigger/depth afterwards. `deep_capture(reset=True)`.

**Spread a deep record over more time (more frames).** Deep memory at a fixed timebase gives
the **same time window** as the screen (≈ `19.2 × TDIV`), just more samples — so 40K over
200 µs/div is the same ~4 ms window as 4K, only ~5 SPI bytes. To capture *many* frames, slow
the timebase. The deep sample interval is `deep_dt = 19.2 · TDIV / deep_samples` (deep_samples:
40K→40064, 512K→400064). To put ~15–25 samples on the finest pulse, set
`TDIV = (pulse / target) · deep_samples / 19.2`. `deep_capture(auto_tb=True)` probes the signal
at 4K (finest pulse), picks that TDIV, then does the deep capture — verified: 40K @ 20 kHz SPI
went from 5 → **101 decoded bytes** over an 80 ms record (2 µs/sample, ~25 samples/bit).

### 3.1 Deep (40K/512K/1M) or LA — SINGLE-SEQ + Save→CSV

Goal: get one deep trigger-aligned record onto the PC. There is no deep sample stream
over USB (protocol.md §10.7) — the record must be saved to the SD card as CSV and read
back. `deep_capture()`:

```
CONNECT reset=False
│
├─ trigger_capture(depth):
│    ├─ STOP            press RUN/STOP (19) until TRIG-STATE==0   [poll each time]
│    ├─ PREP  (0x11, one write, while stopped):
│    │        CH1/CH2 DISP=1, PROBE=1×, COUP=DC, RPHASE=off, 20MHz=full, VB=1V/div,
│    │        CH1 POS=0 / CH2 POS=−2div, TRIG Edge/CH1/Auto, TRIG-VPOS=40 (≈+1.6V),
│    │        ACQURIE-STORE-DEPTH=depth
│    ├─ ARM SINGLE      press SINGLE (18)                          ← do NOT manual-stop
│    └─ WAIT            poll TRIG-STATE; the scope triggers on the signal edge and
│                       captures one full-depth record. (Force-Trig (47) only as a
│                       last resort.) A manual RUN→STOP here freezes an EMPTY screen —
│                       single-seq is what captures real data.
│
├─ SAVE→CSV  (per channel source; keys only, NO 0x11):
│    ├─ MENU-SR (11)  → poll menuid==47
│    ├─ CSV     (3)   → poll menuid==48
│    ├─ [Source (1) ×N]   0=CH1, 1=CH2, 2=LA         (cycles the Source radio)
│    ├─ Save    (2)   1st press → opens the FileList
│    └─ Save    (2)   2nd press → writes /mnt/udisk/WaveData<n>.csv
│
├─ WAIT-FOR-FILE:   poll `ls /mnt/udisk` (retry the flaky read) until a NEW
│                   WaveData*.csv appears AND its size stops changing (deep files
│                   grow for seconds while written).
│
├─ READ-BACK:       read_file("/mnt/udisk/WaveData<n>.csv") over 0x10 → parse_wavedata_csv
│
└─ RESTORE:         STOP → 0x11 depth=4K → CONTROL-DISP-MENU=0 (main screen)
```

**A completed single-seq reads TRIG-STATE = 5 (SINGLE), not 0 `[verified 2026-07-11]`.**
When SINGLE captures, the scope **stops itself** with the record and the RUN/STOP button
goes **red** — but `TRIG-STATE` reads **5 (SINGLE-captured/stopped)**, not 0 (STOP). Treat 5
as STOPPED (`_STOPPED_STATES = {0, 5}`). Two bugs came from misreading 5 as "armed": (a)
polling for state==0 that never comes → forcing; (b) a stop request (`_run_stop`) then
pressing RUN/STOP on a state-5 record, which **starts it running** — that was the "512K scope
kept running, only CH1" symptom. So after a single-seq: wait for state ∈ {0,5}, and do **not**
toggle Run/Stop.

**The save is asynchronous — "Operation in progress" `[verified 2026-07-11]`.** After the
two-press Save the scope shows an orange **"Operation is in progress! Please Wait……"** banner
over the FileList and **ignores all key presses** until the write finalizes. The card `ls`
sees the final `WaveData<n>.csv` only when the scope renames its temp file at the **END** of
the write (~40 s for a 512K/7.7 MB file). So: press the two-press Save **once and wait
patiently** — do **NOT** re-press during the write (extra Save presses corrupt the save and
advance the Source past CH2 → the "512K skips CH2" bug). Wait for the banner to clear
(framebuffer `0x20`; `_wait_save_done`) before the next Source cycle, then resync + settle ~2 s
(the framebuffer grabs need a pause before the next key). This is what "verify state, don't
blind-wait" means — a slow SD card just makes the banner last longer, and we watch it, not a
fixed timer.

**Trigger level matters.** With no level *on* the signal, SINGLE arms forever
(never crosses) — set `TRIG-VPOS` mid-logic (≈ +1.6 V for 3.3 V logic at 1 V/div).
`[verified]`

**Prerequisite: the SD card must be mounted** (`df /mnt/udisk` → a vfat device, not
`ubi0:rootfs`). Save is a **silent no-op** with no card, and no `/dsocsv.tmp` is
written (the save aborts at the USB-disk check). There is no card-free path.

**Two channels (SPI/I²C) — works at every depth incl. 512K `[verified 2026-07-11]`:** save
CH1 then CH2 from the *same* frozen record. The Source radio order is **CH1→CH2→LA**; a single
`keyid 1` press advances one step. After a Save the scope is on the CSV page with the FileList
open; cycle Source **directly (keyid 1), NOT via Back** (Back goes *up* to the S/R base where
keyid 1 = Ref). The whole sequence, per channel:

1. two-press Save **once**, then wait for the `WaveData<n>.csv` (patiently — see the async note);
2. `_wait_save_done` (banner clears) → `resync` + settle ~2 s;
3. one Source press (CH1→CH2), settle ~1.5 s;
4. next Save.

**512K IS dual-channel** — CH1 and CH2 come back as genuinely different records (verified: the
two files differ in 86 % of samples and decode as SPI). The earlier "512K is single-channel"
suspicion was wrong; the real bug was the rapid Save re-pressing above. Read-back is **deferred**
(save all channels first, then read) so a 7.7 MB read doesn't sit between Source cycles.
`save_sources=None` brings back every enabled channel; the two files are index-aligned.

**Card-detection caveat:** saving Source = **LA while the pod is off** writes no file (the
earlier "card undetected / no `/dsocsv.tmp`" symptom was often just the Source being parked
on LA, not the card). A genuinely disturbed card is rarer and a front-panel key press
re-detects it. The `0x11` prep write did **not** break card detection in testing — deep
saves succeed straight after it.

**LA over CSV:** the CSV Source selector includes **LA** — Save with Source=LA exports the
16-channel pod (`#threshold` header → `is_la`). LA forces the record to 4K (deep memory is
analog-only).

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

---

## 7. Shell (`0x43`) interaction notes

The root shell (`mso5202d_shell.py`) is used here only for read-only card checks
(`df /mnt/udisk`, `ls /mnt/udisk`). Quirks it handles (protocol.md Appendix F.4):
firmware appends `> msg` to the command (wrap multi-command in `{ …; }`), the reply
can race one command behind (unique end-marker + retry), and a stalled command trips
the watchdog reboot — so keep shell commands short and never read a live process's
`/proc/<pid>/*`. Reading files off the scope is done with the `0x53 0x10` file-read,
not shell `cat`.
