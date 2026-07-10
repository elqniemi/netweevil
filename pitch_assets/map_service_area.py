import sys, json, numpy as np
sys.path.insert(0, "pitch_assets")
from render import *

W_IN, H_IN = 4.28, 4.25
d = json.load(open("pitch_assets/out/service_area_network.json"))

# Order bands outer -> inner so the bright core draws last (on top)
bands = {f["threshold_id"]: f for f in d["features"]}
order = [("15_min", LAV, "15 min"), ("10_min", SKY, "10 min"), ("5_min", MINT, "5 min")]

origin_lon, origin_lat = 6.5665, 53.2194
ox, oy = merc(origin_lon, origin_lat)

fig, ax = new_fig(W_IN, H_IN)

all_xy = []
band_segs = {}
for tid, col, lab in order:
    feat = bands[tid]
    geom = feat["geometry"]
    lines = geom["coordinates"]
    segs = []
    for ln in lines:
        if len(ln) < 2:
            continue
        m = merc_arr(ln)
        segs.append(m)
        all_xy.append(m)
    band_segs[tid] = segs

stack = np.vstack(all_xy)
set_extent(ax, stack[:, 0], stack[:, 1], pad=0.06, aspect_wh=W_IN / H_IN)

basemap(ax, labels=True)

lw_by = {"15_min": 0.45, "10_min": 0.55, "5_min": 0.7}
for tid, col, lab in order:
    glow_lines(ax, band_segs[tid], col, lw=lw_by[tid], glow=(2.2, 0.035),
               base_z=3 + order.index((tid, col, lab)) * 0, alpha=0.92)

glow_marker(ax, ox, oy, INK, size=70)

title_block(ax, "SERVICE AREA · DRIVE TIME", "Drive-time reach across\nthe street network", fig_tag="FIG 7A", w=0.61, tsize=11.0)
legend_box(ax, 0.035, 0.34, [(MINT, "0-5 min"), (SKY, "5-10 min"), (LAV, "10-15 min")],
           w=0.27, title="REACHED WITHIN", line=True)
# stat strip
n_edges = sum(s["reachable_edge_count"] for s in d["summaries"])
length_km = sum(s["reachable_network_length_m"] for s in d["summaries"]) / 1000
panel(ax, 0.035, 0.038, 0.50, 0.058, z=10, alpha=0.86)
ax.text(0.052, 0.067, f"{n_edges:,} edges   ·   {length_km:,.0f} km of road reached",
        transform=ax.transAxes, color=MUTE, fontsize=6.2, va="center", ha="left",
        family="DejaVu Sans Mono", zorder=12)
credit(ax)
finish(fig, "pitch_assets/maps/fig_service_area.png")
