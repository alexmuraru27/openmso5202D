#!/usr/bin/env python3
"""Score every saved capture in decoding/decoder_tests/{spi,uart,i2c}/waves/ against the expected
0x00..0xFF ramp, and track the scores in a baseline manifest so any decoder change can be diffed for
improvement/regression. Hardware-free — runs on the CSV corpus.

    python3 -m decoding.decode_corpus                 # score all; diff vs baseline if present
    python3 -m decoding.decode_corpus spi             # score only one protocol
    python3 -m decoding.decode_corpus --save          # write/update the baseline (decode_scores.json)

Workflow: run once with --save to snapshot the current decoder as the baseline; after any decoder
change, run without --save to see per-case Δ (▲ improved / ▼ regressed) and the overall change.
The baseline lives at decoding/decoder_tests/decode_scores.json (commit it to track progress)."""
import sys, os, json
sys.path.insert(0, '.')
from mso5202d import parse_wavedata_csv
from mso5202d_plot import decode_capture

PROTOS = ('spi', 'uart', 'i2c')
# The wave corpus lives under scope_dump/ so both the Python and the Rust test suites
# score against the same captures; the baseline stays next to the decoder it measures.
_HERE = os.path.dirname(os.path.abspath(__file__))
CORPUS = os.path.abspath(os.path.join(_HERE, '..', '..', 'scope_dump', 'decoder_corpus'))
SCORES = os.path.join(_HERE, 'decode_scores.json')


def ramp_ratio(vals):
    if len(vals) < 2:
        return 0.0, len(vals)
    good = sum(1 for i in range(1, len(vals)) if vals[i] == (vals[i - 1] + 1) & 0xFF)
    return good / (len(vals) - 1), len(vals)


def decode_params(proto, freq):
    if proto == 'uart':
        return {'proto': 'uart', 'line': 0, 'baud': freq}
    if proto == 'spi':
        return {'proto': 'spi', 'clk': 0, 'data': 1}
    return {'proto': 'i2c', 'scl': 0, 'sda': 1}


def score_proto(proto):
    """Score every case of one protocol → dict keyed 'proto/freq/depth/spc'."""
    waves = f"{CORPUS}/{proto}/waves"
    man = json.load(open(f"{waves}/manifest.json"))
    out = {}
    for r in man:
        results = []
        for fn in r['files']:
            path = f"{waves}/{r['depth']}/{fn}"
            if not os.path.exists(path):
                continue
            res = parse_wavedata_csv(open(path).read())
            res['source'] = 'CH1' if '_CH1' in fn else ('CH2' if '_CH2' in fn else None)
            results.append(res)
        if not results:
            continue
        ev, _, _ = decode_capture(results, decode_params(proto, r['freq']))
        vals = [int(e['value']) for e in ev if e['kind'] == 'byte']
        rr, nb = ramp_ratio(vals)
        key = f"{proto}/{r['freq']}/{r['depth']}/{r['spc']}spc"
        out[key] = {'proto': proto, 'freq': r['freq'], 'depth': r['depth'], 'spc': r['spc'],
                    'ramp': round(rr, 3), 'bytes': nb}
    return out


def _fmt_delta(cur, base):
    if base is None:
        return '    (new)'
    d = cur - base
    if abs(d) < 0.005:
        return f'  ={base:.3f}'
    return f'  {"▲" if d > 0 else "▼"}{d:+.3f} (was {base:.3f})'


def main():
    args = sys.argv[1:]
    save = '--save' in args
    protos = [a for a in args if a in PROTOS] or list(PROTOS)
    baseline = json.load(open(SCORES)) if os.path.exists(SCORES) else {}

    cur = {}
    for p in protos:
        cur.update(score_proto(p))

    for p in protos:
        keys = [k for k in cur if cur[k]['proto'] == p]
        print(f"\n=== {p.upper()} ===")
        print(f"{'rate':>9} {'depth':>5} {'spc':>4} {'bytes':>6} {'ramp':>6}   vs baseline")
        for k in sorted(keys, key=lambda k: (cur[k]['freq'], cur[k]['depth'], -cur[k]['spc'])):
            c = cur[k]; b = baseline.get(k, {}).get('ramp')
            print(f"{c['freq']:>9} {c['depth']:>5} {c['spc']:>4} {c['bytes']:>6} {c['ramp']:>6.3f}"
                  f"{_fmt_delta(c['ramp'], b)}")
        n = len(keys)
        mean = sum(cur[k]['ramp'] for k in keys) / n if n else 0.0
        hi = sum(cur[k]['ramp'] >= 0.99 for k in keys)
        bkeys = [k for k in keys if k in baseline]
        bmean = sum(baseline[k]['ramp'] for k in bkeys) / len(bkeys) if bkeys else None
        tail = '' if bmean is None else f"   (baseline mean {bmean * 100:.1f}%, Δ {(mean - bmean) * 100:+.1f} pts)"
        print(f"  -> mean ramp {mean * 100:.1f}% | {hi}/{n} at ≥0.99{tail}")

    # overall
    allk = list(cur)
    mean = sum(cur[k]['ramp'] for k in allk) / len(allk) if allk else 0.0
    bk = [k for k in allk if k in baseline]
    bmean = sum(baseline[k]['ramp'] for k in bk) / len(bk) if bk else None
    print("\n=== OVERALL ===")
    print(f"mean decode {mean * 100:.1f}% over {len(allk)} cases"
          + ('' if bmean is None else f"  |  baseline {bmean * 100:.1f}%  →  Δ {(mean - bmean) * 100:+.1f} pts"))
    regressed = [k for k in bk if cur[k]['ramp'] < baseline[k]['ramp'] - 0.02]
    improved = [k for k in bk if cur[k]['ramp'] > baseline[k]['ramp'] + 0.02]
    if improved:
        print(f"improved ({len(improved)}): " + ", ".join(improved))
    if regressed:
        print(f"REGRESSED ({len(regressed)}): " + ", ".join(regressed))

    if save:
        merged = {**baseline, **cur}                  # update only the scored protocols
        json.dump({k: merged[k] for k in sorted(merged)}, open(SCORES, 'w'), indent=1)
        print(f"\nbaseline saved → {os.path.relpath(SCORES)} ({len(cur)} cases updated)")


if __name__ == '__main__':
    main()
