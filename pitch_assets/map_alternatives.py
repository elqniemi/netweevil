import sys, json, numpy as np
sys.path.insert(0, "pitch_assets")
from render import *
import matplotlib.patheffects as pe

W_IN, H_IN = 4.55, 4.25

d = json.load(open("pitch_assets/out/route_alternatives.json"))

best_geom = d["geometry"]
best_time = d["summary"]["total_travel_time_s"]
best_dist = d["summary"]["total_distance_m"]

# rank alternatives by travel time so the legend reads fastest -> slowest
alts = sorted(d["alternatives"], key=lambda a: a["summary"]["total_travel_time_s"])

# alt colours, thinnest/most-different last
ALT_COLORS = [SKY, LAV, AMBER]

o = d["origin"]; dest = d["destination"]
ox, oy = merc(o["snapped_lon"], o["snapped_lat"])
dx, dy = merc(dest["snapped_lon"], dest["snapped_lat"])

fig, ax = new_fig(W_IN, H_IN)

# fit extent to every route so nothing clips
all_pts = [merc_arr(best_geom)] + [merc_arr(a["geometry"]) for a in alts]
allxy = np.vstack(all_pts + [np.array([[ox, oy], [dx, dy]])])
set_extent(ax, allxy[:, 0], allxy[:, 1], pad=0.16, aspect_wh=W_IN / H_IN)
basemap(ax, labels=True)


def to_segs(m):
    return [m[i:i + 2] for i in range(len(m) - 1)]


# draw slowest/most-divergent alternative first (bottom), fastest optimal on top
for a, col in reversed(list(zip(alts, ALT_COLORS))):
    seg = to_segs(merc_arr(a["geometry"]))
    glow_lines(ax, seg, col, lw=1.9, glow=(5, 0.05), base_z=3, alpha=0.94)

# the optimal route: thickest, brightest, on top so it always reads as the hero
glow_lines(ax, to_segs(merc_arr(best_geom)), MINT, lw=2.6, glow=(8, 0.06),
           base_z=6, alpha=0.97)

# origin / destination terminals: neutral white dots, labelled A and B
glow_marker(ax, ox, oy, INK, size=72)
glow_marker(ax, dx, dy, INK, size=72)
ax.text(ox, oy, "  A", color=INK, fontsize=8, va="center", ha="left", zorder=11,
        fontweight="bold", path_effects=[pe.withStroke(linewidth=2.4, foreground=BG)])
ax.text(dx, dy, "  B", color=INK, fontsize=8, va="center", ha="left", zorder=11,
        fontweight="bold", path_effects=[pe.withStroke(linewidth=2.4, foreground=BG)])

# header panel
title_block(ax, "ROUTE ALTERNATIVES", "The best route, and\nthe good-enough ones",
            fig_tag="FIG 11A", w=0.58, tsize=11.0)


def mins(sec):
    return sec / 60.0


# horizontal legend bar along the bottom: the routes all sit in the upper two
# thirds, so a bottom strip stays clear of every path (including alt 1's
# south-western dip) while keeping the brand panel styling.
entries = [(MINT, "fastest", f"{mins(best_time):.1f} min")]
for i, (a, col) in enumerate(zip(alts, ALT_COLORS), start=1):
    extra = mins(a["summary"]["total_travel_time_s"]) - mins(best_time)
    entries.append((col, f"alt {i}", f"+{extra:.1f} min"))

bx, by, bw, bh = 0.035, 0.045, 0.930, 0.140
panel(ax, bx, by, bw, bh, z=10, alpha=0.90)

n_routes = len(alts) + 1
head = (f"ONE QUESTION, SEVERAL ANSWERS   ·   {n_routes} ranked paths   ·   "
        f"{best_dist / 1000:.1f} km fastest via Groningen")
ax.text(bx + 0.022, by + bh - 0.028, head, transform=ax.transAxes, color=MUTE,
        fontsize=5.8, fontweight="bold", va="center", ha="left",
        family="DejaVu Sans Mono", zorder=12)

n = len(entries)
col_w = (bw - 0.052) / n
name_y = by + 0.074
val_y = by + 0.036
for i, (col, name, val) in enumerate(entries):
    cx0 = bx + 0.028 + i * col_w
    ax.plot([cx0, cx0 + 0.040], [name_y, name_y], transform=ax.transAxes,
            color=col, lw=2.8, solid_capstyle="round", zorder=12,
            path_effects=[pe.Stroke(linewidth=6.5, foreground=to_rgba(col, 0.18)),
                          pe.Normal()])
    ax.text(cx0 + 0.056, name_y, name, transform=ax.transAxes, color=INK,
            fontsize=7.2, fontweight="bold", va="center", ha="left", zorder=12)
    ax.text(cx0 + 0.056, val_y, val, transform=ax.transAxes, color=MUTE,
            fontsize=7.0, va="center", ha="left", zorder=12,
            family="DejaVu Sans Mono")

credit(ax)
finish(fig, "pitch_assets/maps/fig_alternatives.png")
