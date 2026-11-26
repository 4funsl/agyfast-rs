#!/usr/bin/env python3
"""Generate assets/compare.svg: plain Antigravity vs agy-fast vs agy-rs. Standard library only.

Numbers: hard benchmark, solo agent, Gemini 4 Hydrogen Low, one run each, all 14/15
(docs/BENCHMARKS.md). Run: python3 assets/make-compare.py
"""
from pathlib import Path

W, H = 1200, 520
BG, PANEL, TEXT, DIM = "#0F172A", "#111C33", "#E2E8F0", "#64748B"
ROWS = [("No plugin", "#64748B"), ("agy-fast 0.3.1", "#38BDF8"), ("agy-rs", "#34D399")]
# title, unit note, values per row (None = not applicable), value labels
PANELS = [
    ("Billable tokens (weighted)", "lower is better", [273176, 238088, 206669], ["273k", "238k  −13%", "207k  −24%"]),
    ("LLM turns", "lower is better", [25, 20, 16], ["25", "20", "16"]),
    ("Wall time", "lower is better", [184, 151, 146], ["184 s", "151 s", "146 s"]),
    ("Tokens added to every session", "rules + tool schemas", [None, 4763, 2706], ["none", "≈4,800", "≈2,700"]),
]


def main():
    out = []
    pw, ph, gap, left, top = 555, 190, 30, 30, 80
    for k, (title, note, vals, labels) in enumerate(PANELS):
        x, y = left + (k % 2) * (pw + gap), top + (k // 2) * (ph + 20)
        out.append(f'<rect x="{x}" y="{y}" width="{pw}" height="{ph}" rx="10" fill="{PANEL}"/>')
        out.append(f'<text x="{x + 20}" y="{y + 32}" fill="{TEXT}" font-size="17" font-weight="700">{title}</text>')
        out.append(f'<text x="{x + pw - 20}" y="{y + 32}" fill="{DIM}" font-size="13" text-anchor="end">{note}</text>')
        peak = max(v for v in vals if v)
        for i, ((name, color), v, label) in enumerate(zip(ROWS, vals, labels)):
            by = y + 58 + i * 42
            bold = ' font-weight="700"' if name == "agy-rs" else ""
            out.append(f'<text x="{x + 20}" y="{by + 17}" fill="{color}" font-size="14"{bold}>{name}</text>')
            bw = 250 * v / peak if v else 0
            if bw:
                out.append(f'<rect x="{x + 160}" y="{by}" width="{bw:.0f}" height="24" rx="4" fill="{color}"/>')
            out.append(f'<text x="{x + 170 + bw:.0f}" y="{by + 17}" fill="{TEXT}" font-size="14"{bold}>{label}</text>')
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" font-family="ui-monospace,SFMono-Regular,Menlo,Consolas,monospace" role="img" aria-label="Same task in plain Antigravity, with agy-fast and with agy-rs: weighted tokens 273k, 238k, 207k; LLM turns 25, 20, 16; wall time 184, 151, 146 seconds; tokens added per session none, about 4,800, about 2,700">
<rect width="{W}" height="{H}" fill="{BG}"/>
<text x="30" y="40" fill="{TEXT}" font-size="20" font-weight="700">Same task, same model, same score (14/15)</text>
<text x="30" y="62" fill="{DIM}" font-size="13">hard benchmark · solo agent · Gemini 4 Hydrogen Low · one run per configuration · docs/BENCHMARKS.md</text>
{chr(10).join(out)}
</svg>
'''
    path = Path(__file__).with_name("compare.svg")
    path.write_text(svg)
    print(f"wrote {path} ({len(svg)} bytes)")


if __name__ == "__main__":
    main()
