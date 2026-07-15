#!/usr/bin/env python3
"""Decode every saved capture in decoding/decoder_tests/{spi,uart,i2c}/waves/ and score it against the
expected 0x00..0xFF ramp. Hardware-free — runs on the CSV corpus. Reports per-case ramp ratio
and a per-protocol summary, so decoder changes can be measured against real captures.

    python3 -m decoding.decode_corpus [spi|uart|i2c ...]   # from scripts/; default: all three
"""
import sys, os, json
sys.path.insert(0, '.')
from mso5202d import parse_wavedata_csv
from mso5202d_plot import decode_capture

PROTOS = ('spi', 'uart', 'i2c')
CORPUS = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'decoder_tests')


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


def run(proto):
    waves = f"{CORPUS}/{proto}/waves"
    man = json.load(open(f"{waves}/manifest.json"))
    print(f"\n=== {proto.upper()} ===")
    print(f"{'rate':>9} {'depth':>5} {'spc':>4} {'dt_ns':>8} {'bytes':>6} {'ramp':>6}")
    rows = []
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
        rows.append((r, rr, nb))
        print(f"{r['freq']:>9} {r['depth']:>5} {r['spc']:>4} {str(r.get('dt_ns')):>8} "
              f"{nb:>6} {rr:>6.3f}")
    hi = sum(1 for _, rr, _ in rows if rr >= 0.99)
    med = sum(1 for _, rr, _ in rows if 0.5 <= rr < 0.99)
    print(f"  -> {hi}/{len(rows)} at ramp≥0.99, {med} partial (0.5–0.99), "
          f"{len(rows) - hi - med} poor")
    return rows


def main():
    protos = [a for a in sys.argv[1:] if a in PROTOS] or list(PROTOS)
    allrows = {}
    for p in protos:
        allrows[p] = run(p)
    print("\n=== SUMMARY ===")
    for p in protos:
        rows = allrows[p]
        hi = sum(1 for _, rr, _ in rows if rr >= 0.99)
        print(f"{p:>5}: {hi}/{len(rows)} cases decode the ramp at ≥0.99")


if __name__ == '__main__':
    main()
