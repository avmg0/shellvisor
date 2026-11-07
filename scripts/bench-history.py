#!/usr/bin/env python3
"""Keep Shellvisor's performance history and draw it.

    scripts/bench-history.py add bench.json      # append (or replace) one release
    scripts/bench-history.py draw                # docs/images/bench.svg from the history

The history is docs/bench/history.json: one entry per release, measured by the
Release workflow on the same GitHub runner type each time (ubuntu-latest,
3 tabs, dev layout), so the numbers compare release to release. The chart is
a plain SVG (no libraries) that follows the reader's light or dark theme.
"""
import json
import os
import sys
from datetime import date

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HISTORY = os.path.join(ROOT, "docs", "bench", "history.json")
SVG = os.path.join(ROOT, "docs", "images", "bench.svg")

# (key, title, unit, budget)
METRICS = [
    ("binary_mb", "Binary size", "MB", 5.0),
    ("rss_mb", "Memory, idle (3 tabs + panes)", "MB", 15.0),
    ("first_frame_ms", "First frame", "ms", 100.0),
    ("idle_cpu_pct", "CPU, idle", "%", 0.5),
]


def vkey(v):
    return tuple(int(x) if x.isdigit() else 0 for x in v.split("."))


def load():
    try:
        with open(HISTORY) as f:
            return json.load(f)
    except FileNotFoundError:
        return []


def save(h):
    os.makedirs(os.path.dirname(HISTORY), exist_ok=True)
    with open(HISTORY, "w") as f:
        json.dump(h, f, indent=1)
        f.write("\n")


def add(path):
    with open(path) as f:
        e = json.load(f)
    e.setdefault("date", date.today().isoformat())
    h = [x for x in load() if x.get("version") != e["version"]]
    h.append(e)
    h.sort(key=lambda x: vkey(x["version"]))
    save(h)
    return h


def esc(s):
    return str(s).replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def fmt(v, unit):
    if unit == "%":
        return f"{v:.2f}%"
    if v >= 100:
        return f"{v:.0f} {unit}"
    return f"{v:.1f} {unit}"


def draw(h):
    W, PW, PH, GAP, TOP = 860, 400, 150, 30, 46
    H = TOP + 2 * (PH + 46) + 10
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" font-family="-apple-system,Segoe UI,Helvetica,Arial,sans-serif">',
        "<style>",
        ".bg{fill:#ffffff}.t{fill:#1f2328}.m{fill:#59636e}.g{stroke:#d1d9e0}.b{stroke:#cf222e}.l{stroke:#0969da}.d{fill:#0969da}",
        "@media (prefers-color-scheme: dark){.bg{fill:#0d1117}.t{fill:#e6edf3}.m{fill:#9198a1}.g{stroke:#30363d}.b{stroke:#f85149}.l{stroke:#4493f8}.d{fill:#4493f8}}",
        "</style>",
        f'<rect class="bg" width="{W}" height="{H}" rx="8"/>',
        f'<text class="t" x="20" y="28" font-size="16" font-weight="600">Shellvisor per release</text>',
        f'<text class="m" x="{W - 20}" y="28" font-size="12" text-anchor="end">GitHub ubuntu-latest runner · dashed line = budget</text>',
    ]
    for i, (key, title, unit, budget) in enumerate(METRICS):
        x0 = 20 + (i % 2) * (PW + GAP)
        y0 = TOP + (i // 2) * (PH + 46)
        pts = [(e["version"], float(e[key])) for e in h if key in e]
        last = fmt(pts[-1][1], unit) if pts else "—"
        out.append(f'<text class="t" x="{x0}" y="{y0 + 14}" font-size="13" font-weight="600">{esc(title)}</text>')
        out.append(f'<text class="t" x="{x0 + PW}" y="{y0 + 14}" font-size="13" text-anchor="end">{esc(last)}</text>')
        top, bottom = y0 + 26, y0 + PH
        left, right = x0 + 34, x0 + PW - 6
        hi = max([budget] + [v for _, v in pts]) * 1.1
        for f in (0, 0.5, 1):
            y = bottom - (bottom - top) * f
            out.append(f'<line class="g" x1="{left}" y1="{y:.1f}" x2="{right}" y2="{y:.1f}" stroke-width="1"/>')
            out.append(f'<text class="m" x="{left - 6}" y="{y + 4:.1f}" font-size="10" text-anchor="end">{hi * f:.3g}</text>')
        yb = bottom - (bottom - top) * budget / hi
        out.append(f'<line class="b" x1="{left}" y1="{yb:.1f}" x2="{right}" y2="{yb:.1f}" stroke-width="1" stroke-dasharray="4 3"/>')
        if not pts:
            out.append(f'<text class="m" x="{(left + right) / 2}" y="{(top + bottom) / 2}" font-size="12" text-anchor="middle">first point after the next release</text>')
            continue
        n = len(pts)
        xs = [left + (right - left) * (k / (n - 1) if n > 1 else 0.5) for k in range(n)]
        ys = [bottom - (bottom - top) * v / hi for _, v in pts]
        if n > 1:
            d = " ".join(f"{x:.1f},{y:.1f}" for x, y in zip(xs, ys))
            out.append(f'<polyline class="l" fill="none" stroke-width="2" points="{d}"/>')
        step = max(1, n // 6)
        for k, ((ver, v), x, y) in enumerate(zip(pts, xs, ys)):
            out.append(f'<circle class="d" cx="{x:.1f}" cy="{y:.1f}" r="3"><title>{esc(ver)}: {esc(fmt(v, unit))}</title></circle>')
            if k % step == 0 or k == n - 1:
                out.append(f'<text class="m" x="{x:.1f}" y="{bottom + 14}" font-size="10" text-anchor="middle">{esc(ver)}</text>')
    out.append("</svg>")
    os.makedirs(os.path.dirname(SVG), exist_ok=True)
    with open(SVG, "w") as f:
        f.write("\n".join(out) + "\n")


def main():
    if len(sys.argv) >= 3 and sys.argv[1] == "add":
        draw(add(sys.argv[2]))
    elif len(sys.argv) == 2 and sys.argv[1] == "draw":
        draw(load())
    else:
        sys.exit(__doc__)
    print(SVG)


if __name__ == "__main__":
    main()
