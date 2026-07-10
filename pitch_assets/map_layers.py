"""Slide 12 (FIG 12A): the QGIS plugin's grouped, styled layer output.

Composites several netweevil results the way the plugin loads them as grouped
layers: a service-area reachable network, a route, and the transit stop layer —
all styled together over the dark basemap, framed on the Groningen core.
"""
import sys, json, numpy as np
sys.path.insert(0, "pitch_assets")
from render import *
from matplotlib.collections import LineCollection

W_IN, H_IN = 4.55, 4.25

sa = json.load(open("pitch_assets/out/service_area_network.json"))
rb = json.load(open("pitch_assets/out/route_restrict_batch.json"))
stops = np.array(json.load(open("pitch_assets/out/gtfs_stops_bbox.json")))

# service-area layer: the 5-minute reachable network (compact, central)
band5 = next(f for f in sa["features"] if f["threshold_id"] == "5_min")
sa_segs = [merc_arr(ln) for ln in band5["geometry"]["coordinates"] if len(ln) >= 2]

# route layer: a central city route
items = {it["route_id"]: it for it in rb["items"]}
route = items["strict_4"]["route"]
rg = merc_arr(route["geometry"])
o = route["origin"]; dest = route["destination"]
ox, oy = merc(o["snapped_lon"], o["snapped_lat"])
dx, dy = merc(dest["snapped_lon"], dest["snapped_lat"])

fig, ax = new_fig(W_IN, H_IN)
# frame on the Groningen core around the centre origin
cxm, cym = merc(6.5665, 53.2160)
span = 3000
ax.set_xlim(cxm - span, cxm + span)
ax.set_ylim(cym - span / (W_IN / H_IN), cym + span / (W_IN / H_IN))
basemap(ax, labels=True)


def to_segs(m):
    return [m[i:i + 2] for i in range(len(m) - 1)]


# layer 1: service-area reachable network (sky-blue web)
ax.add_collection(LineCollection(sa_segs, colors=[to_rgba(SKY, 0.45)],
                                 linewidths=0.55, zorder=2.5, capstyle="round"))
# layer 2: transit stops (faint dots)
sm = merc_arr(stops)
ax.scatter(sm[:, 0], sm[:, 1], s=4.0, color=to_rgba(AMBER, 0.40), zorder=3, linewidths=0)
# layer 3: route on top (mint)
glow_lines(ax, to_segs(rg), MINT, lw=2.6, glow=(8, 0.06), base_z=5, alpha=0.98)

glow_marker(ax, ox, oy, INK, size=70)
glow_marker(ax, dx, dy, MINT, size=60)

title_block(ax, "QGIS PLUGIN OUTPUT", "Grouped, styled\nresult layers", fig_tag="FIG 12A",
            w=0.46, tsize=11.0)
legend_box(ax, 0.035, 0.27,
           [(MINT, "Route"), (SKY, "Service area"), (AMBER, "Transit stops")],
           w=0.34, title="LAYER TREE", line=True)
credit(ax)
finish(fig, "pitch_assets/maps/fig_layers.png")
