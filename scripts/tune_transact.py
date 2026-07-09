#!/usr/bin/env python3
"""Find the fastest reliable TRANSACT_POST_S for *your* hardware.

Runs real settings round-trips at decreasing post-write margins and reports the
success rate + average latency for each. Pick the smallest margin that still
scores N/N, then set TRANSACT_POST_S in mso5202d.py to it.

    cd scripts && python3 tune_transact.py
"""
import time
import mso5202d as M

MARGINS = (0.050,0.030, 0.020, 0.015, 0.012, 0.010, 0.008, 0.006, 0.004)
N = 40

def main():
    sc = M.Scope()
    print(f"margin(ms)   ok/{N}   avg_ms   verdict")
    best = None
    for m in MARGINS:
        M.TRANSACT_POST_S = m
        ok = 0
        t0 = time.time()
        for _ in range(N):
            try:
                r = sc.read_settings()
                if len(r) == 214 and r[0] == 0x81:
                    ok += 1
            except Exception:
                pass
        avg = (time.time() - t0) / N * 1000
        verdict = "reliable" if ok == N else ("flaky" if ok > N * 0.9 else "FAILS")
        if ok == N:
            best = m
        print(f"  {m*1000:6.1f}    {ok:3d}/{N}   {avg:6.1f}   {verdict}")
    sc.close()
    if best is not None:
        print(f"\n-> smallest fully-reliable margin: {best*1000:.0f} ms.  "
              f"Set  TRANSACT_POST_S = {best}  in mso5202d.py "
              f"(leave a little headroom — e.g. one step up — if you want safety margin).")
    else:
        print("\n-> none were fully reliable; keep TRANSACT_POST_S = 0.03.")

if __name__ == "__main__":
    main()
