#!/usr/bin/env python3
"""
MSO5202D serial-protocol analyzer — capture a synchronized 2-channel snapshot
off the scope, then decode/view it as UART, SPI or I²C offline.

The scope's two analog channels are only phase-aligned if they come from ONE
acquisition, so `capture` freezes the scope (arm SINGLE → STOP) and reads both
channels out of that one frozen buffer. Everything after that (threshold, decode,
draw) works from the stored .npz, so you can pan/zoom and re-decode without
touching hardware. See docs/MSO5202D-protocol.md ("Serial decoding").

    # freeze both channels into a file
    python3 mso5202d_decode.py capture cap.npz

    # decode the stored capture (channel 0 = CH1 probe, 1 = CH2 probe)
    python3 mso5202d_decode.py decode cap.npz --proto uart --line 0
    python3 mso5202d_decode.py decode cap.npz --proto spi  --clk 0 --data 1
    python3 mso5202d_decode.py decode cap.npz --proto i2c  --scl 0 --sda 1

    # waveform + decode overlay (GUI, or --png for headless)
    python3 mso5202d_decode.py view cap.npz --proto spi --clk 0 --data 1 --png out.png

    # sanity-check that the two frozen channels are edge-aligned (wire one signal
    # to both probes): reports the CH1↔CH2 lag, which should be ≈0 samples
    python3 mso5202d_decode.py sync cap.npz
"""
import argparse
import json
import sys
import time
import numpy as np

from decoding import threshold, decode_uart, decode_spi, decode_i2c


# --- capture (synchronized frozen snapshot) --------------------------------------
# Front-panel key ids (0-indexed /keyprotocol.inf position; see protocol.md F.2).
KEY_SINGLE = 18      # CT-SINGLESEQ — arm a single acquisition, lands in STOP
KEY_RUNSTOP = 19     # CT-RS       — Run/Stop toggle


def _key(sc, keyid):
    """Send a front-panel key event (0x13 | keyid | press)."""
    sc.transact(bytes([0x13, keyid, 0x01]))


def _field_off(name):
    """Byte offset + width of a settings field within the 213-byte block."""
    from mso5202d import SETTINGS_PARAMS
    o = 0
    for n, w in SETTINGS_PARAMS:
        if n == name:
            return o, w
        o += w
    raise KeyError(name)


def set_timebase(sc, idx):
    """Set the horizontal timebase to TB index `idx` (0..31 → 2 ns…40 s/div) by
    writing the whole settings block (selector 0x11). HORIZ-WIN-TB is the knob
    value; HORIZ-TB (real acquisition TB) is clamped at index 6 = 200 ns/div, so
    set it to max(idx, 6). Returns the resulting SAMPLE-INTERVAL-ns."""
    from mso5202d import decode_settings
    block = bytearray(_raw_settings(sc)[1:])   # strip the 0x81 echo → 213 bytes
    owin, _ = _field_off('HORIZ-WIN-TB')
    otb, _ = _field_off('HORIZ-TB')
    block[owin] = idx
    block[otb] = max(idx, 6)
    sc.transact(b'\x11' + bytes(block))
    time.sleep(0.3)
    return decode_settings(_raw_settings(sc)).get('SAMPLE-INTERVAL-ns')


def pick_tb(period_ns, target_samples=25):
    """Choose the TB index whose sample interval gives ~`target_samples` per
    `period_ns` (one bit / one clock period). Sample interval = TDIV/200, so the
    ideal TDIV = period·200/target; pick the nearest available index (≥6)."""
    from mso5202d import TB_TO_NS
    ideal = period_ns * 200 / target_samples
    cands = {i: v for i, v in TB_TO_NS.items() if i >= 6}
    return min(cands, key=lambda i: abs(cands[i] - ideal))


def _raw_settings(sc, tries=5):
    """read_settings, retried until it returns a valid 214-byte payload (a 0x11
    write or a busy scope can transiently leak a short ack frame into the read)."""
    raw = sc.read_settings()
    for _ in range(tries):
        if len(raw) == 214 and raw[0] == 0x81:
            return raw
        time.sleep(0.15)
        raw = sc.read_settings()
    raise RuntimeError(f"bad settings read (len={len(raw)})")


def _state(sc):
    from mso5202d import decode_settings
    return decode_settings(_raw_settings(sc))


def _running(sc):
    return _state(sc)['TRIG-STATE'] != 0


def freeze(sc, settle=0.6):
    """Freeze a FRESH acquisition so both channels read out of ONE simultaneous
    capture of the current signal.

    Crucial ordering: the scope must be RUNNING (acquiring the live signal) and
    then STOPped — a scope that is already stopped holds a stale buffer, and
    reading it just returns whatever was on screen before. So we ensure it is
    running, let it acquire for `settle`, then stop. Run/Stop (key 19) is a
    toggle whose presses can be dropped, so each transition presses *until* the
    desired state is observed. STOP freezes the last simultaneous 2-channel
    acquisition, which is exactly what we read out. (SINGLE is avoided — it only
    lands in STOP once a real trigger fires, which a mis-set trigger level may
    never deliver.) Returns (prior_trig_state, settings_after)."""
    prior = _state(sc)['TRIG-STATE']
    for _ in range(6):                        # ensure running → acquires fresh data
        if _running(sc):
            break
        _key(sc, KEY_RUNSTOP)
        time.sleep(0.3)
    time.sleep(settle)                        # let a fresh acquisition complete
    for _ in range(6):                        # then stop (press until STOP sticks)
        if not _running(sc):
            break
        _key(sc, KEY_RUNSTOP)
        time.sleep(0.4)
    return prior, _state(sc)


def check_settings(s, channels=(0, 1)):
    """Print each channel's vertical setup and return a list of warnings for
    settings that will corrupt a logic decode (channel off, probe ≠ 1×, AC/GND
    coupling, invert on). These don't change the byte encoding's math but they do
    change what the trace looks like — the wrong one silently mis-decodes."""
    from mso5202d import VERT_PROBE_NAMES, VERT_COUP_NAMES
    issues = []
    for ch in channels:
        n = ch + 1
        disp = s.get(f'VERT-CH{n}-DISP')
        probe = s.get(f'VERT-CH{n}-PROBE')
        coup = s.get(f'VERT-CH{n}-COUP')
        inv = s.get(f'VERT-CH{n}-RPHASE')
        bw = s.get(f'VERT-CH{n}-20MHZ')
        print(f"  CH{n}: {'ON ' if disp else 'OFF'}  probe={VERT_PROBE_NAMES.get(probe, '?')}"
              f"  coup={VERT_COUP_NAMES.get(coup, '?')}  invert={'On' if inv else 'Off'}"
              f"  bw={'20MHz' if bw else 'Full'}")
        if not disp:
            issues.append(f"CH{n} display is OFF — it won't be captured")
        if probe not in (0, None):
            issues.append(f"CH{n} probe is {VERT_PROBE_NAMES.get(probe)} — set it to 1× for the "
                          f"biggest, cleanest logic swing")
        if coup == 1:
            issues.append(f"CH{n} coupling is AC — use DC so the logic levels sit still")
        if coup == 2:
            issues.append(f"CH{n} coupling is GND — no signal will be seen")
        if inv:
            issues.append(f"CH{n} invert (RPHASE) is ON — decode polarity will be flipped; turn it off")
    return issues


def cmd_capture(args):
    from mso5202d import Scope, TRIG_STATE_NAMES
    sc = Scope()
    try:
        if args.timebase is not None:
            si = set_timebase(sc, args.timebase)
            print(f"[+] timebase set to index {args.timebase} ({si} ns/sample)")
        prior, s = freeze(sc)
        print(f"[+] frozen (TRIG-STATE={TRIG_STATE_NAMES.get(s['TRIG-STATE'])})")
        print("[i] channel setup (want: ON, probe 1×, DC coupling, invert Off):")
        issues = check_settings(s)
        for w in issues:
            print(f"  ⚠ {w}")
        print("[+] reading both channels from the one frozen buffer…")
        w0 = np.frombuffer(sc.read_waveform(0), dtype=np.uint8)
        w1 = np.frombuffer(sc.read_waveform(1), dtype=np.uint8)
        # Amplitude sanity: a decodable logic trace should swing ≳0.5 div.
        from mso5202d_plot import to_divs
        for ch, w in ((0, w0), (1, w1)):
            if len(w):
                d = to_divs(w, s.get(f'VERT-CH{ch+1}-POS') or 0)
                swing = float(np.percentile(d, 95) - np.percentile(d, 5))
                if swing < 0.5:
                    print(f"  ⚠ CH{ch+1} swing is only {swing:.2f} div — signal flat or "
                          f"mis-scaled; lower V/div or check the probe")
        meta = {k: s.get(k) for k in (
            'SAMPLE-INTERVAL-ns', 'SAMPLERATE-HZ', 'TDIV-ns',
            'VERT-CH1-POS', 'VERT-CH2-POS', 'CH1-VDIV-mV', 'CH2-VDIV-mV',
            'VERT-CH1-DISP', 'VERT-CH2-DISP', 'VERT-CH1-PROBE', 'VERT-CH2-PROBE',
            'VERT-CH1-COUP', 'VERT-CH2-COUP', 'VERT-CH1-RPHASE', 'VERT-CH2-RPHASE')}
        np.savez(args.file, ch0=w0, ch1=w1, meta=np.array(json.dumps(meta)))
        print(f"[+] CH1: {len(w0)} samples  CH2: {len(w1)} samples  "
              f"@ {meta['SAMPLE-INTERVAL-ns']} ns/sample")
        print(f"[+] saved {args.file}")
        # Leave the scope as we found it: resume Run if it had been running.
        if prior != 0:
            _key(sc, KEY_RUNSTOP)
            print("[+] resumed Run")
    finally:
        sc.close()


# --- load + threshold ------------------------------------------------------------
def load(path):
    """Load a capture → (channels dict {0,1: uint8 array}, meta dict)."""
    z = np.load(path, allow_pickle=False)
    meta = json.loads(str(z['meta']))
    chans = {0: z['ch0'], 1: z['ch1']}
    return chans, meta


def digital(chans, meta, ch):
    """Threshold one stored channel into a boolean logic trace using its POS."""
    y = chans[ch]
    if not len(y):
        raise SystemExit(f"channel {ch} (CH{ch+1}) is empty in this capture — "
                         f"was its probe/display on?")
    pos = meta.get(f'VERT-CH{ch+1}-POS') or 0
    return threshold(y, pos)


# --- decode (text) ---------------------------------------------------------------
def _print_bytes(events, kind_filter=('byte', 'addr')):
    vals = [e for e in events if e['kind'] in kind_filter]
    for i in range(0, len(vals), 16):
        row = vals[i:i + 16]
        print(f"  {i:4}: " + ' '.join(f"{e['text']:>3}" for e in row))
    return [e.get('value') for e in vals]


def cmd_decode(args):
    chans, meta = load(args.file)
    dt = meta.get('SAMPLE-INTERVAL-ns')
    if args.proto == 'uart':
        d = digital(chans, meta, args.line)
        ev = decode_uart(d, sample_interval_ns=dt, baud=args.baud,
                         parity=args.parity, stops=args.stops)
        print(f"UART on CH{args.line+1}: {len(ev)} frames"
              + (f" @ {args.baud} baud" if args.baud else " (auto-baud)"))
        vals = _print_bytes(ev)
        bad = [e for e in ev if not e['ok']]
        if bad:
            print(f"  ⚠ {len(bad)} framing/parity errors")
    elif args.proto == 'spi':
        clk = digital(chans, meta, args.clk)
        dat = digital(chans, meta, args.data)
        ev = decode_spi(clk, dat, cpol=args.cpol, cpha=args.cpha,
                        msb_first=not args.lsb)
        print(f"SPI  clk=CH{args.clk+1} data=CH{args.data+1} "
              f"mode{args.cpol}{args.cpha} {'LSB' if args.lsb else 'MSB'}: "
              f"{len(ev)} bytes")
        vals = _print_bytes(ev)
    elif args.proto == 'i2c':
        scl = digital(chans, meta, args.scl)
        sda = digital(chans, meta, args.sda)
        ev = decode_i2c(scl, sda)
        nb = sum(e['kind'] in ('byte', 'addr') for e in ev)
        print(f"I2C  scl=CH{args.scl+1} sda=CH{args.sda+1}: "
              f"{sum(e['kind']=='start' for e in ev)} START, "
              f"{sum(e['kind']=='stop' for e in ev)} STOP, {nb} bytes")
        _print_bytes(ev, kind_filter=('addr', 'byte'))
        nak = [e for e in ev if e['kind'] in ('addr', 'byte') and not e['ack']]
        if nak:
            print(f"  ⚠ {len(nak)} NAKed bytes")


# --- view (waveform + decode overlay) --------------------------------------------
def cmd_view(args):
    import matplotlib
    if args.png:
        matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from mso5202d_plot import (to_divs, x_divs, style_scope, CH_COLORS,
                               BG, FG, GRID, SAMPLES_PER_DIV)

    chans, meta = load(args.file)

    # Decode + pick which channels are the signals so we can overlay logic traces.
    if args.proto == 'uart':
        used = [args.line]
        ev = decode_uart(digital(chans, meta, args.line),
                         sample_interval_ns=meta.get('SAMPLE-INTERVAL-ns'),
                         baud=args.baud, parity=args.parity, stops=args.stops)
    elif args.proto == 'spi':
        used = [args.clk, args.data]
        ev = decode_spi(digital(chans, meta, args.clk),
                        digital(chans, meta, args.data),
                        cpol=args.cpol, cpha=args.cpha, msb_first=not args.lsb)
    else:
        used = [args.scl, args.sda]
        ev = decode_i2c(digital(chans, meta, args.scl),
                        digital(chans, meta, args.sda))

    n = max((len(chans[c]) for c in (0, 1) if len(chans[c])), default=SAMPLES_PER_DIV)
    fig, ax = plt.subplots(figsize=(13, 5)); fig.patch.set_facecolor(BG)
    style_scope(ax, n / SAMPLES_PER_DIV)
    # Analog traces (faint) + a thresholded logic overlay for the decoded lines.
    for ch in (0, 1):
        y = chans[ch]
        if not len(y):
            continue
        pos = meta.get(f'VERT-CH{ch+1}-POS') or 0
        ax.plot(x_divs(len(y)), to_divs(y, pos), lw=0.8, color=CH_COLORS[ch],
                alpha=0.35, label=f"CH{ch+1}")
    for slot, ch in enumerate(dict.fromkeys(used)):    # unique, keep order
        d = digital(chans, meta, ch).astype(float)
        base = 3.0 - slot * 1.6                          # stack logic rows near top
        ax.plot(x_divs(len(d)), base + d * 1.2, lw=1.1, color=CH_COLORS[ch],
                solid_capstyle='round')
        ax.text(0.05, base + 0.6, f"CH{ch+1}", color=CH_COLORS[ch], fontsize=8, va='center')

    # Decode annotations: a marker line at each event start + its label below.
    for e in ev:
        x = e['start'] / SAMPLES_PER_DIV
        color = {'start': '#2ec27e', 'stop': '#e01b24'}.get(e['kind'], FG)
        ax.axvline(x, color=color, lw=0.5, alpha=0.5)
        ax.text(x, -3.4, e['text'], color=color, fontsize=6, rotation=90,
                va='bottom', ha='center', clip_on=True)

    ax.set_title(f"{args.proto.upper()} decode — {args.file}  "
                 f"({len([e for e in ev if e['kind'] in ('byte','addr')])} bytes)",
                 color=FG, fontsize=9)
    leg = ax.legend(loc='upper right', fontsize=8, facecolor=BG, edgecolor=GRID)
    for t in leg.get_texts():
        t.set_color(FG)
    fig.tight_layout()
    if args.png:
        fig.savefig(args.png, dpi=110, facecolor=BG)
        print(f"[+] saved {args.png}")
    else:
        plt.show()


# --- sync check ------------------------------------------------------------------
def cmd_sync(args):
    chans, meta = load(args.file)
    if not (len(chans[0]) and len(chans[1])):
        raise SystemExit("need both channels populated (wire one signal to both "
                         "probes) to check sync")
    from mso5202d_plot import to_divs
    a = to_divs(chans[0], meta.get('VERT-CH1-POS') or 0)
    b = to_divs(chans[1], meta.get('VERT-CH2-POS') or 0)
    a = a - a.mean(); b = b - b.mean()
    corr = np.correlate(a, b, 'full')
    lag = int(corr.argmax() - (len(b) - 1))
    dt = meta.get('SAMPLE-INTERVAL-ns')
    print(f"CH1↔CH2 lag: {lag} samples"
          + (f"  ({lag*dt:.1f} ns)" if dt else ""))
    if abs(lag) <= 1:
        print("[+] aligned — synchronized 2-channel capture confirmed.")
    else:
        print("[!] NOT aligned — the two reads are from different acquisitions; "
              "2-wire decode (SPI/I²C) will be unreliable. See protocol.md.")


# --- CLI -------------------------------------------------------------------------
def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)

    p = sub.add_parser('capture', help="freeze the scope and store both channels")
    p.add_argument('file', help="output .npz")
    p.add_argument('--timebase', type=int, default=None,
                   help="set horizontal TB index 0..31 (2 ns…40 s/div) before capture")
    p.set_defaults(func=cmd_capture)

    def add_proto(p):
        p.add_argument('file', help="capture .npz")
        p.add_argument('--proto', required=True, choices=('uart', 'spi', 'i2c'))
        p.add_argument('--line', type=int, default=0, help="UART: channel index (0=CH1,1=CH2)")
        p.add_argument('--baud', type=float, default=None, help="UART baud (default auto)")
        p.add_argument('--parity', default='none', choices=('none', 'even', 'odd'))
        p.add_argument('--stops', type=int, default=1)
        p.add_argument('--clk', type=int, default=0, help="SPI clock channel")
        p.add_argument('--data', type=int, default=1, help="SPI data channel")
        p.add_argument('--cpol', type=int, default=0, choices=(0, 1))
        p.add_argument('--cpha', type=int, default=0, choices=(0, 1))
        p.add_argument('--lsb', action='store_true', help="SPI LSB-first (default MSB)")
        p.add_argument('--scl', type=int, default=0, help="I²C SCL channel")
        p.add_argument('--sda', type=int, default=1, help="I²C SDA channel")

    p = sub.add_parser('decode', help="decode a stored capture to text")
    add_proto(p); p.set_defaults(func=cmd_decode)

    p = sub.add_parser('view', help="waveform + decode overlay (GUI / --png)")
    add_proto(p); p.add_argument('--png', metavar='PATH')
    p.set_defaults(func=cmd_view)

    p = sub.add_parser('sync', help="check the two frozen channels are edge-aligned")
    p.add_argument('file'); p.set_defaults(func=cmd_sync)

    a = ap.parse_args()
    a.func(a)


if __name__ == '__main__':
    sys.exit(main())
