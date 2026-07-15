#!/usr/bin/env python3
"""Build a long-term serial-waveform corpus for a protocol (SPI / UART / I2C): every listed
rate × {4K,40K,512K} memory depth × {12,100} samples-per-bit, captured as REAL deep records
(single-seq → Save→CSV → read-back) and saved verbatim into decoding/decoder_tests/<proto>/waves/.

    python3 capture_corpus.py spi     # (already captured — resumes/skips validated)
    python3 capture_corpus.py uart
    python3 capture_corpus.py i2c

Per case:
  - set the ESP32 generator (continuous 0x00..0xFF ramp) for this protocol + rate,
  - FRESH scope state every capture (Default Setup) so each is independent,
  - SEC/DIV set for `spc` samples per bit/clock (deep_tdiv_for_bit, max_freq = the actual rate),
  - capture the protocol's channel(s), then VERIFY we downloaded ACTUAL wave data (right sample
    count, real swing + logic transitions on every expected channel) — retry up to MAX_ATTEMPTS,
  - write each channel's exact CSV bytes to waves/<depth>/<proto>_<rate><unit>_<spc>spc_CHn.csv,
  - record metadata (timebase, dt, sizes, decoded-ramp score, validated?) in waves/manifest.json.

The ramp is decoded and its score kept as metadata only — a poor decode does NOT fail a case;
the purpose here is gathering real captures, analysis comes later. Resumable (validated cases
skip) and self-healing (reconnects if a capture reboots the scope)."""
import sys, os, time, json, subprocess
sys.path.insert(0, '.')
import numpy as np
from mso5202d import Scope
from mso5202d_plot import deep_capture, decode_capture, deep_tdiv_for_bit, _DEEP_SAMPLES
from decoding import threshold_volts

DEPTHS = [(0, '4k'), (4, '40k'), (6, '512k')]
MIN_SWING = 0.5              # volts — a channel below this is flat/idle (no real signal)
MIN_EDGES = 5               # a real capture has at least this many logic transitions
MAX_ATTEMPTS = 3

# Per-protocol config. `channels` = scope channels to enable; `nch` = how many must carry a real
# signal to count as "got data" (UART drives only CH1). `decode(rate)` = decode_capture params.
# `spcs(rate)` = samples-per-bit list; 100·rate must stay under the scope's real-time ceiling.
PROTOS = {
    'spi': dict(unit='hz', freqs=[10000, 500000, 2000000, 20000000],
                channels=(1, 2), nch=2,
                decode=lambda f: {'proto': 'spi', 'clk': 0, 'data': 1},
                spcs=lambda f: [12, 100] if f <= 10_000_000 else [12, 30]),
    'uart': dict(unit='baud', freqs=[9600, 115200, 921600],
                 channels=(1,), nch=1,
                 decode=lambda f: {'proto': 'uart', 'line': 0, 'baud': f},
                 spcs=lambda f: [12, 100]),
    'i2c': dict(unit='hz', freqs=[10000, 400000, 1000000],
                channels=(1, 2), nch=2,      # CH1=SCL, CH2=SDA
                decode=lambda f: {'proto': 'i2c', 'scl': 0, 'sda': 1},
                spcs=lambda f: [12, 100]),
}


def ramp_stats(vals):
    if len(vals) < 2:
        return dict(n=len(vals), ratio=0.0, run=len(vals))
    good = sum(1 for i in range(1, len(vals)) if vals[i] == (vals[i - 1] + 1) & 0xFF)
    best = cur = 1
    for i in range(1, len(vals)):
        if vals[i] == (vals[i - 1] + 1) & 0xFF:
            cur += 1; best = max(best, cur)
        else:
            cur = 1
    return dict(n=len(vals), ratio=round(good / (len(vals) - 1), 3), run=best)


def set_esp(proto, rate):
    r = subprocess.run(["python3", "mso5202d_espgen.py", "set", proto, str(rate), "continuous"],
                       capture_output=True, text=True, timeout=30)
    return r.returncode == 0, (r.stdout + r.stderr).strip()


def connect(tries=6):
    last = None
    for _ in range(tries):
        try:
            return Scope(reset=False)               # reset=False: a dev.reset() disturbs the SD card
        except Exception as e:
            last = e; time.sleep(4)
    raise last


def evaluate(caps, depth_code, decode_params, nch):
    """Verify we downloaded ACTUAL wave data (not empty/flat/truncated). `ok` = `nch` channels
    each with the full sample count, a real swing and logic transitions. The ramp is decoded and
    scored as metadata only (a poor decode does NOT fail the capture). Returns (ok, stats)."""
    exp = _DEEP_SAMPLES[depth_code]
    analog = [r for r in caps if not r.get('is_la') and r.get('volts') is not None]
    ev, dt, used = decode_capture(caps, decode_params)
    vals = [int(e['value']) for e in ev if e['kind'] == 'byte']
    st = ramp_stats(vals)                        # metadata only
    sizes = [r.get('size') for r in analog]
    swings, edges = [], []
    for r in analog:
        v = np.asarray(r['volts'])
        swings.append(round(float(np.percentile(v, 99) - np.percentile(v, 1)), 2) if len(v) else 0.0)
        d = threshold_volts(v).astype(np.int8)
        edges.append(int((np.abs(np.diff(d)) >= 1).sum()) if len(d) > 1 else 0)
    size_ok = len(analog) >= nch and all(s == exp for s in sizes[:nch] or [None])
    has_signal = (len(analog) >= nch
                  and sum(s >= MIN_SWING for s in swings) >= nch
                  and sum(e >= MIN_EDGES for e in edges) >= nch)
    st.update(sizes=sizes, size_ok=size_ok, nch=len(analog), swings=swings, edges=edges,
              dt_ns=(round(dt * 1e9, 3) if dt else None))
    ok = size_ok and has_signal
    return ok, st


def write_files(caps, waves, depthname, proto, rate, unit, spc):
    paths = []
    for r in caps:
        if r.get('is_la') or r.get('raw') is None:
            continue
        src = r.get('source', 'CHx')
        raw = r['raw']
        p = f"{waves}/{depthname}/{proto}_{rate}{unit}_{spc}spc_{src}.csv"
        with open(p, 'wb' if isinstance(raw, (bytes, bytearray)) else 'w') as f:
            f.write(raw)
        paths.append(os.path.basename(p))
    return paths


def main():
    proto = sys.argv[1] if len(sys.argv) > 1 else 'spi'
    if proto not in PROTOS:
        print(f"usage: capture_corpus.py {{{'|'.join(PROTOS)}}}"); return
    cfg = PROTOS[proto]
    corpus = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'decoding', 'decoder_tests')
    waves = f"{corpus}/{proto}/waves"
    manifest_path = f"{waves}/manifest.json"
    for _, dn in DEPTHS:
        os.makedirs(f"{waves}/{dn}", exist_ok=True)
    man = {}
    if os.path.exists(manifest_path):
        man = {(r['freq'], r['depth'], r['spc']): r for r in json.load(open(manifest_path))}

    def save_manifest():
        json.dump(sorted(man.values(), key=lambda r: (r['freq'], r['depth_code'], -r['spc'])),
                  open(manifest_path, 'w'), indent=1, default=int)

    sc = connect()
    cases = [(f, dc, dn, spc) for f in cfg['freqs'] for spc in cfg['spcs'](f) for (dc, dn) in DEPTHS]
    total = len(cases)
    for idx, (freq, dc, dn, spc) in enumerate(cases, 1):
        key = (freq, dn, spc)
        if man.get(key, {}).get('validated'):
            print(f"[{idx}/{total}] skip {proto} {freq}{cfg['unit']} {dn} {spc}spc (validated)", flush=True)
            continue
        ok_set, msg = set_esp(proto, freq)
        if not ok_set:
            print(f"[!] ESP set {proto} {freq} failed: {msg}", flush=True)
        time.sleep(1.2)
        tb = deep_tdiv_for_bit(1e9 / freq, _DEEP_SAMPLES[dc], spc)
        wait_trig = max(25, int(20 * tb * 1e-9) + 15)
        dparams = cfg['decode'](freq)
        best = None                              # (caps, st, ok)
        for attempt in range(1, MAX_ATTEMPTS + 1):
            log = []
            try:
                caps = deep_capture(sc, dc, log.append, channels=cfg['channels'], reset=True,
                                    tb_target_ns=tb, delete_after=True, wait_trig=wait_trig)
                ok, st = evaluate(caps, dc, dparams, cfg['nch'])
                better = (best is None or (ok and not best[2])
                          or (ok == best[2] and st['ratio'] > best[1]['ratio']))
                if better:
                    best = (caps, st, ok)
                print(f"[{idx}/{total}] {proto} {freq}{cfg['unit']} {dn} {spc}spc try{attempt}: "
                      f"data={ok} swings={st['swings']} edges={st['edges']} size_ok={st['size_ok']} "
                      f"| ramp={st['ratio']}({st['n']}B) dt={st['dt_ns']}ns -> {'OK' if ok else 'retry'}",
                      flush=True)
                if ok:
                    break
            except Exception as e:
                print(f"[{idx}/{total}] ERR {proto} {freq}{cfg['unit']} {dn} {spc}spc try{attempt}: "
                      f"{type(e).__name__}: {e}", flush=True)
                for m in log[-3:]:
                    print(f"        · {m}", flush=True)
                try: sc.close()
                except Exception: pass
                time.sleep(3)
                try:
                    sc = connect()
                except Exception as e2:
                    print(f"        reconnect failed: {e2}", flush=True); save_manifest(); return
        if best is None:
            man[key] = dict(freq=freq, depth=dn, depth_code=dc, spc=spc,
                            validated=False, files=[], error="all attempts errored")
            save_manifest(); continue
        caps, st, ok = best
        files = write_files(caps, waves, dn, proto, freq, cfg['unit'], spc)
        man[key] = dict(freq=freq, depth=dn, depth_code=dc, spc=spc, tb_ns=round(tb, 1),
                        dt_ns=st['dt_ns'], act_spc=(round(1e9 / freq / st['dt_ns'], 1)
                                                    if st['dt_ns'] else None),
                        swings=st['swings'], edges=st['edges'],
                        n_bytes=st['n'], ramp=st['ratio'], run=st['run'],
                        sizes=st['sizes'], size_ok=st['size_ok'],
                        validated=bool(ok), files=files)
        tag = "OK (has data)" if ok else "NO valid data (empty/flat/truncated) — FLAGGED"
        print(f"        -> {tag}: {files}", flush=True)
        save_manifest()
    save_manifest()
    nval = sum(1 for r in man.values() if r.get('validated'))
    print(f"CORPUS DONE [{proto}] — {nval}/{len(man)} validated", flush=True)


if __name__ == '__main__':
    main()
