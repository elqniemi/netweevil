"""Development-timeline figure for the netweevil pitch deck.

A horizontal calendar timeline (mid-March -> early July 2026) where every
active commit day is a lollipop sized by commit count, bursts carry milestone
labels, and the long idle stretches are left visually empty on purpose.

The whole story: a handful of intense days, months of calendar.

Run with the maps venv:
    /home/elmeriniemi/stuff/netan/.venv_maps/bin/python pitch_assets/fig_timeline.py
"""
import datetime as dt
import numpy as np
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import matplotlib.patheffects as pe
from matplotlib.colors import to_rgba

# ----------------------------------------------------------------------------
# Brand palette (matches the deck / render.py)
# ----------------------------------------------------------------------------
BG     = "#0B101A"   # deck background
PANEL  = "#101828"   # panel fill
BORDER = "#223349"   # subtle 1px border (~#223)
INK    = "#E6EAF2"   # titles / near-white
MUTE   = "#8A93A6"   # secondary text
DIM    = "#57657B"   # faint structure
MINT   = "#2DE5A0"   # primary accent
SKY    = "#4FC3F7"
LAV    = "#B99AFF"
AMBER  = "#FFC14E"
CORAL  = "#FF6B6B"

plt.rcParams["font.family"] = "DejaVu Sans"
MONO = "DejaVu Sans Mono"

# ----------------------------------------------------------------------------
# Data: verified from `git log` in /home/elmeriniemi/stuff/netan
#   first commit 2026-03-19, latest 2026-07-02, 75 commits, single author.
# ----------------------------------------------------------------------------
DAYS = [
    (dt.date(2026, 3, 19), 2),
    (dt.date(2026, 3, 20), 2),
    (dt.date(2026, 3, 26), 4),
    (dt.date(2026, 5,  9), 9),
    (dt.date(2026, 5, 10), 1),
    (dt.date(2026, 6, 10), 26),
    (dt.date(2026, 7,  1), 29),
    (dt.date(2026, 7,  2), 2),
]
X = [d.toordinal() for d, _ in DAYS]
C = [c for _, c in DAYS]

# Bursts -> milestone labels (honest to what actually landed on those days).
# "days" indexes into DAYS. "tier" picks the label height band (alternating
# high/low so horizontally-close bursts never collide).
BURSTS = [
    dict(days=[0, 1], tier="A", align="left",   color=MINT,  tag="KICKOFF",
         desc="core engine · HTTP API · QGIS plugin"),
    dict(days=[2],    tier="B", align="center", color=SKY,   tag="ROUTING",
         desc="bidirectional search · first CCH"),
    dict(days=[3, 4], tier="A", align="center", color=LAV,   tag="TRANSIT",
         desc="GTFS routing · analysis runs"),
    dict(days=[5],    tier="A", align="center", color=AMBER, tag="SCALE-UP",
         desc="correct CCH · simulation · crate split"),
    dict(days=[6, 7], tier="B", align="right",  color=CORAL, tag="OVERTURE",
         desc="Overture import · CCH matrix · isochrones"),
]

# ----------------------------------------------------------------------------
# Geometry
# ----------------------------------------------------------------------------
DATE_MIN = dt.date(2026, 3, 6)
DATE_MAX = dt.date(2026, 7, 16)
XLIM = (DATE_MIN.toordinal(), DATE_MAX.toordinal())
YLIM = (-15.0, 61.0)

TIER = {"A": 48.0, "B": 37.0}   # milestone tag baseline
DESC_DROP = 3.6                  # description sits this far below the tag

MONTHS = [
    (dt.date(2026, 3, 1), dt.date(2026, 4, 1), "MAR"),
    (dt.date(2026, 4, 1), dt.date(2026, 5, 1), "APR"),
    (dt.date(2026, 5, 1), dt.date(2026, 6, 1), "MAY"),
    (dt.date(2026, 6, 1), dt.date(2026, 7, 1), "JUN"),
    (dt.date(2026, 7, 1), dt.date(2026, 8, 1), "JUL"),
]

# Idle-gap callouts between bursts (from -> to, day count).
GAPS = [
    (dt.date(2026, 3, 26), dt.date(2026, 5, 9), "44 days"),
    (dt.date(2026, 5, 10), dt.date(2026, 6, 10), "31 days"),
    (dt.date(2026, 6, 10), dt.date(2026, 7, 1), "21 days"),
]


def clampx(o):
    return max(XLIM[0], min(XLIM[1], o))


def main():
    fig = plt.figure(figsize=(12.0, 5.4), dpi=200, facecolor=BG)
    ax = fig.add_axes([0, 0, 1, 1])
    ax.set_facecolor(BG)
    ax.set_xlim(*XLIM)
    ax.set_ylim(*YLIM)
    ax.set_xticks([]); ax.set_yticks([])
    for s in ax.spines.values():
        s.set_visible(False)

    # --- faint calendar structure: month boundaries + labels -----------------
    for start, end, name in MONTHS:
        xs = start.toordinal()
        if XLIM[0] < xs < XLIM[1]:
            ax.plot([xs, xs], [-9, 52], color=BORDER, lw=1.0, alpha=0.35,
                    zorder=1)
        mid = (clampx(start.toordinal()) + clampx(end.toordinal())) / 2
        ax.text(mid, -10.6, name, color=DIM, fontsize=8.5, family=MONO,
                ha="center", va="center", zorder=2)

    # --- baseline ------------------------------------------------------------
    ax.plot([XLIM[0] + 1, XLIM[1] - 1], [0, 0], color=BORDER, lw=1.4,
            alpha=0.9, zorder=2, solid_capstyle="round")

    # --- idle-gap callouts ---------------------------------------------------
    for a, b, label in GAPS:
        xa, xb = a.toordinal() + 3, b.toordinal() - 3
        ax.plot([xa, xb], [0, 0], color=DIM, lw=1.2, alpha=0.55,
                linestyle=(0, (1, 3)), zorder=3, solid_capstyle="round")
        ax.text((xa + xb) / 2, 3.1, label, color=MUTE, fontsize=8.0,
                family=MONO, ha="center", va="center", zorder=3)

    # --- lollipops: one per active day, grouped by burst ---------------------
    for b in BURSTS:
        col = b["color"]
        for i in b["days"]:
            x, c = X[i], C[i]
            # stem with soft glow
            ax.plot([x, x], [0, c], color=col, lw=2.6, zorder=4,
                    solid_capstyle="round",
                    path_effects=[pe.Stroke(linewidth=7,
                                            foreground=to_rgba(col, 0.16)),
                                  pe.Normal()])
            # dot: area proportional to commit count, with layered glow
            s = 34 * c
            ax.scatter([x], [c], s=s * 5.0, color=to_rgba(col, 0.08),
                       linewidths=0, zorder=5)
            ax.scatter([x], [c], s=s * 2.3, color=to_rgba(col, 0.16),
                       linewidths=0, zorder=5)
            ax.scatter([x], [c], s=s, color=col, edgecolors=BG, linewidths=1.4,
                       zorder=6)
            # commit count label
            if c >= 8:
                ax.text(x, c, str(c), color=BG, fontsize=9.0, family=MONO,
                        fontweight="bold", ha="center", va="center", zorder=7)
            else:
                ax.text(x, c + 2.6, str(c), color=INK, fontsize=8.0,
                        family=MONO, ha="center", va="center", zorder=7)

    # --- milestone labels + leaders ------------------------------------------
    for b in BURSTS:
        col = b["color"]
        y = TIER[b["tier"]]
        xs = [X[i] for i in b["days"]]
        cs = [C[i] for i in b["days"]]
        align = b["align"]
        if align == "left":
            anchor, ha = min(xs), "left"
        elif align == "right":
            anchor, ha = max(xs), "right"
        else:
            anchor, ha = float(np.mean(xs)), "center"
        peak_i = int(np.argmax(cs))
        peak_x, peak_c = xs[peak_i], cs[peak_i]

        # leader: a thin line from just above the tallest dot up to the text
        top_y = y - DESC_DROP - 1.6
        ax.plot([peak_x, anchor], [peak_c + 3.0, top_y], color=col, lw=1.1,
                alpha=0.5, zorder=4, solid_capstyle="round")
        # tag + description
        ax.text(anchor, y, b["tag"], color=col, fontsize=10.5, family=MONO,
                fontweight="bold", ha=ha, va="center", zorder=7)
        ax.text(anchor, y - DESC_DROP, b["desc"], color=MUTE, fontsize=8.2,
                family=MONO, ha=ha, va="center", zorder=7)

    # --- title block (top-left) ---------------------------------------------
    tx = XLIM[0] + 2
    ax.text(tx, 58.0, "NETWEEVIL  ·  BUILD TIMELINE", color=MINT,
            fontsize=9.0, family=MONO, fontweight="bold", ha="left",
            va="center", zorder=8)
    ax.text(tx, 53.0, "Built in days, not months", color=INK, fontsize=21.0,
            fontweight="bold", ha="left", va="center", zorder=8)

    # --- total annotation (top-right stat block) ----------------------------
    rx = XLIM[1] - 3
    ax.text(rx, 57.6, "75", color=MINT, fontsize=27.0, family=MONO,
            fontweight="bold", ha="right", va="center", zorder=8)
    ax.plot([rx - 20, rx], [51.7, 51.7], color=BORDER, lw=1.0, alpha=0.9,
            zorder=8)
    ax.text(rx, 49.6, "commits  ·  8 days with commits", color=MUTE, fontsize=8.6,
            family=MONO, ha="right", va="center", zorder=8)
    ax.text(rx, 45.6, "43,535 lines of Rust", color=MUTE, fontsize=8.6,
            family=MONO, ha="right", va="center", zorder=8)

    out = "/home/elmeriniemi/stuff/netan/pitch_assets/maps/fig_timeline.png"
    fig.savefig(out, dpi=fig.dpi, facecolor=BG, pad_inches=0)
    plt.close(fig)
    print("wrote", out)


if __name__ == "__main__":
    main()
