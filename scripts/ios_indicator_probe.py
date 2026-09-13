"""Photograph a simulator repeatedly after launching an app, and say per frame
whether the home indicator bar is drawn.

Throwaway diagnostic for the runner-only home indicator. Usage:
  python3 scripts/ios_indicator_probe.py UDID OUTDIR LABEL [BUNDLE] [COUNT] [INTERVAL]
"""
import json, subprocess, sys, time
from pathlib import Path

BAR = (387, 2583, 819, 2598)   # iPhone 17 Pro @3x home indicator rectangle
FLANKS = ((300, 2583, 380, 2598), (826, 2583, 906, 2598))

def measure(path):
    try:
        import numpy as np
        from PIL import Image
    except ImportError:
        return None
    a = np.asarray(Image.open(path).convert("RGB")).astype(int)
    if a.shape[0] < BAR[3]:
        return {"size": list(a.shape[:2])}
    bar = a[BAR[1]:BAR[3], BAR[0]:BAR[2]].mean(axis=(0, 1))
    flank = np.concatenate([a[y0:y1, x0:x1] for (x0, y0, x1, y1) in FLANKS], axis=1).mean(axis=(0, 1))
    return {"bar": [round(float(v)) for v in bar], "flank": [round(float(v)) for v in flank],
            "contrast": round(float(abs(bar - flank).max()), 1)}

def main():
    udid, out, label = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
    bundle = sys.argv[4] if len(sys.argv) > 4 else "com.apple.Preferences"
    count = int(sys.argv[5]) if len(sys.argv) > 5 else 30
    interval = float(sys.argv[6]) if len(sys.argv) > 6 else 0.0
    out.mkdir(parents=True, exist_ok=True)
    if bundle != "-":
        subprocess.run(["xcrun", "simctl", "terminate", udid, bundle], capture_output=True)
        time.sleep(1.5)
        t_launch = time.time()
        subprocess.run(["xcrun", "simctl", "launch", udid, bundle], check=True, capture_output=True)
    else:
        t_launch = time.time()
    frames = []
    for i in range(count):
        path = out / f"{label}-{i:02d}.png"
        t0 = time.time()
        subprocess.run(["xcrun", "simctl", "io", udid, "screenshot", "--type", "png", str(path)],
                       check=True, capture_output=True)
        t1 = time.time()
        m = measure(path) or {}
        frames.append({"frame": i, "since_launch": round(t0 - t_launch, 3), "shot_took": round(t1 - t0, 3), **m})
        print(json.dumps(frames[-1]), flush=True)
        if interval:
            time.sleep(interval)
    (out / f"{label}.json").write_text(json.dumps(frames, indent=1))

if __name__ == "__main__":
    main()
