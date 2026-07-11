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

## 3. Deep capture — the full flow

Goal: get one deep (4K/40K/512K/1M) trigger-aligned record onto the PC. There is no
deep sample stream over USB (protocol.md §10.7) — the record must be saved to the SD
card as CSV and read back. `deep_capture()` in `mso5202d_plot.py`:

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

**Trigger level matters.** With no level *on* the signal, SINGLE arms forever
(never crosses) — set `TRIG-VPOS` mid-logic (≈ +1.6 V for 3.3 V logic at 1 V/div).
`[verified]`

**Prerequisite: the SD card must be mounted** (`df /mnt/udisk` → a vfat device, not
`ubi0:rootfs`). Save is a **silent no-op** with no card, and no `/dsocsv.tmp` is
written (the save aborts at the USB-disk check). There is no card-free path.

**Two channels (SPI/I²C):** save CH1 then CH2 from the *same* frozen record — enter
the CSV menu, Save (source CH1), then cycle Source once (→ CH2) and Save again. The
two files are index-aligned. `save_sources=(0,1)`.

**LA over CSV `[open]`:** the CSV Source selector includes **LA** — Save with Source=LA
exports the 16-channel pod as a CSV (a way past the broken `02 01 05` live LA read).
Format not yet characterised; needs a card to capture one.

---

## 4. Save→CSV softkey map (menu 47 → 48)

Verified by screenshotting each menu (`0x20`):

| Menu | keyid 1 | keyid 2 | keyid 3 | keyid 4 | keyid 5 | keyid 6 |
|---|---|---|---|---|---|---|
| **S/R** (47) | Ref | SetUp | **CSV** | — | — | — |
| **CSV** (48) | **Source** (CH1→CH2→LA) | **Save** | Recall | **delete ⚠** | FileList | Back |

- **Save is two presses** — 1st opens the FileList (destination browser), 2nd writes.
  A single press is a no-op. `[verified]`
- **`keyid 4 = delete`** — never issue it blind; it erases card files.
- The vendor's real save is just `MENU-SR(11) → CSV(3) → Save(2)` (its FN0/Back
  presses in captures are the operator escaping the slow, stuck app — not part of
  saving). `[verified from the virtual-panel pcap]`

---

## 5. Timing & waits

| Step | Wait | Why |
|---|---|---|
| After a key press | ~0.3–0.8 s + poll | slow key-scan loop; verify the transition landed |
| After `0x11` write | ~0.4–0.5 s | let the block apply |
| RUN → STOP transition | press-until-`TRIG-STATE`-observed | Run/Stop is a toggle; presses drop |
| SINGLE arm → capture | poll `TRIG-STATE` up to ~25 s | waits for a signal edge |
| Save 1st→2nd press | ~0.6–0.8 s | let the FileList open |
| After 2nd Save press | poll file **size until stable** | deep CSV writes take seconds (512K ≈ tens of s) |
| File read-back (0x10) | ~800 KB/s | 512K ≈ 7.7 MB ≈ 10 s; 1M ≈ 19 MB ≈ 25 s |

`_wait_new_csv` scales its hard timeout with depth (4K 30 s … 1M 220 s) but returns
as soon as the size stabilises.

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
