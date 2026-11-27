#!/usr/bin/env python3
"""Generate assets/demo.svg: an animated before/after comparison. Standard library only.

Numbers are the measured hard-benchmark runs (Gemini 4 Hydrogen Low, one run each,
same 14/15 score); see docs/BENCHMARKS.md. Run: python3 assets/make-demo.py
"""
from pathlib import Path
from xml.sax.saxutils import escape

W, H = 1200, 630
RUN, HOLD = 9.0, 5.0          # seconds of animation: the run (184 s scaled), then the summary
CYCLE = RUN + HOLD
BG, PANEL, TEXT, DIM, RED, ACCENT = "#0F172A", "#111C33", "#E2E8F0", "#64748B", "#F87171", "#34D399"

LEFT = dict(title="Antigravity, no plugin", secs=184, color=RED, stats="25 turns · 51 tool calls · 184 s · 273k tokens", lines=[
    "grep -rn BatchPlan src/", "read_file src/core/batch.rs", "read_file src/core/budget.rs",
    "read_file src/tools/search.rs", "read_file src/tools/outline.rs", "edit_file src/core/batch.rs",
    "edit_file src/tools/search.rs", "edit_file src/tools/search.rs", "cargo test   (full output)",
    "read_file src/tools/check.rs", "edit_file src/tools/check.rs", "edit_file src/cli.rs",
    "cargo clippy (full output)", "edit_file src/mcp/schema.rs", "cargo test   (full output)",
    "… 51 tool calls in 25 turns"])
RIGHT = dict(title="Antigravity + agyfast-rs", secs=146, color=ACCENT, stats="16 turns · 29 tool calls · 146 s · 207k tokens", lines=[
    "search        4 queries, definition: true", "check         cargo test → 3 failures, summary",
    "read_file     2 files", "edit          5 files + verify: cargo test",
    "search        fn fit, definition: true", "edit          2 files + verify",
    "check         build && clippy && test → ok", "… 29 tool calls in 16 turns"])


def pct(t):
    return f"{100 * t / CYCLE:.2f}%"


def panel(x, side, idx, css, out):
    pw, top = 560, 70
    end = RUN * side["secs"] / 184
    out.append(f'<rect x="{x}" y="{top}" width="{pw}" height="410" rx="10" fill="{PANEL}"/>')
    out.append(f'<text x="{x + 20}" y="{top + 32}" fill="{side["color"]}" font-size="19" font-weight="700">{side["title"]}</text>')
    n = len(side["lines"])
    for i, line in enumerate(side["lines"]):
        name = f"l{idx}_{i}"
        at = end * (i + 0.5) / n
        css.append(f"@keyframes {name}{{0%,{pct(at)}{{opacity:0}}{pct(at + 0.15)},98%{{opacity:1}}100%{{opacity:0}}}}"
                   f".{name}{{opacity:0;animation:{name} {CYCLE}s linear infinite}}")
        out.append(f'<text class="{name}" x="{x + 20}" y="{top + 66 + i * 20}" fill="{TEXT}" font-size="14">'
                   f'<tspan fill="{DIM}">›</tspan> {escape(line)}</text>')
    # progress bar: grows for the scaled duration of the run, then stays
    bar = f"b{idx}"
    full = (pw - 40) * side["secs"] / 184
    css.append(f"@keyframes {bar}{{0%{{width:0px}}{pct(end)},98%{{width:{full:.0f}px}}100%{{width:0px}}}}"
               f".{bar}{{animation:{bar} {CYCLE}s linear infinite}}")
    out.append(f'<rect x="{x + 20}" y="{top + 425}" width="{pw - 40}" height="8" rx="4" fill="#1E293B"/>')
    out.append(f'<rect class="{bar}" x="{x + 20}" y="{top + 425}" width="0" height="8" rx="4" fill="{side["color"]}"/>')
    done = f"d{idx}"
    css.append(f"@keyframes {done}{{0%,{pct(end)}{{opacity:0}}{pct(end + 0.2)},98%{{opacity:1}}100%{{opacity:0}}}}"
               f".{done}{{opacity:0;animation:{done} {CYCLE}s linear infinite}}")
    out.append(f'<text class="{done}" x="{x + 20}" y="{top + 458}" fill="{side["color"]}" font-size="15" font-weight="700">{side["stats"]}</text>')


def main():
    css, out = [], []
    panel(30, LEFT, 0, css, out)
    panel(610, RIGHT, 1, css, out)
    css.append(f"@keyframes sum{{0%,{pct(RUN)}{{opacity:0}}{pct(RUN + 0.4)},98%{{opacity:1}}100%{{opacity:0}}}}"
               f".sum{{opacity:0;animation:sum {CYCLE}s linear infinite}}")
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" font-family="ui-monospace,SFMono-Regular,Menlo,Consolas,monospace" role="img" aria-label="Same coding task in Antigravity without and with agyfast-rs: 25 turns and 184 seconds become 16 turns and 146 seconds, with 24 percent fewer tokens">
<style>{"".join(css)}</style>
<rect width="{W}" height="{H}" fill="{BG}"/>
<text x="30" y="42" fill="{TEXT}" font-size="20" font-weight="700">One coding task, same result</text>
<text x="{W - 30}" y="42" fill="{DIM}" font-size="14" text-anchor="end">hard benchmark · Gemini 4 Hydrogen Low</text>
{chr(10).join(out)}
<g class="sum">
<text x="{W / 2}" y="578" fill="{TEXT}" font-size="19" font-weight="700" text-anchor="middle">Same result (14/15 checks) · <tspan fill="{ACCENT}">36% fewer turns · 43% fewer tool calls · 24% fewer tokens</tspan></text>
<text x="{W / 2}" y="606" fill="{DIM}" font-size="13" text-anchor="middle">one run per configuration; see docs/BENCHMARKS.md</text>
</g>
</svg>
'''
    path = Path(__file__).with_name("demo.svg")
    path.write_text(svg)
    print(f"wrote {path} ({len(svg)} bytes)")


if __name__ == "__main__":
    main()
