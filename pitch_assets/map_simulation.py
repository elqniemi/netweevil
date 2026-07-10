import sys, json, numpy as np
sys.path.insert(0, "pitch_assets")
from render import *
from matplotlib.collections import LineCollection

W_IN, H_IN = 4.55, 4.25
gj = json.load(open("pitch_assets/out/sim/simulation-groningen_peak-congestion.geojson"))
feats = gj["features"]
print("congestion edges:", len(feats))

segs = []
cong = []
veh = []
for f in feats:
    c = f["geometry"]["coordinates"]
    if len(c) < 2:
        continue
    segs.append(merc_arr(c))
    p = f["properties"]
    cong.append(p["congestion_level"])
    veh.append(p.get("vehicles_entered", 0))
segs = np.array(segs)
cong = np.array(cong)
veh = np.array(veh)

# focus extent on the busy core (weight by vehicles)
busy = veh > np.percentile(veh, 60)
core = segs[busy] if busy.sum() > 50 else segs
cx_pts = core.reshape(-1, 2)

fig, ax = new_fig(W_IN, H_IN)
# centre on the destinations / dense core
cxm, cym = merc(6.5685, 53.2150)
span = 7200  # metres half-width
ax.set_xlim(cxm - span, cxm + span)
ax.set_ylim(cym - span / (W_IN / H_IN), cym + span / (W_IN / H_IN))
basemap(ax, labels=True)

# Google-Maps-style traffic: green free-flow -> amber -> red -> dark-red jam,
# thin crisp lines, no glow.
GREEN, GAMBER, GRED, GDARK = "#2DBE6C", "#FBBC04", "#EA4335", "#9A1B12"
GMAPS = LinearSegmentedColormap.from_list(
    "gmaps", [(0.00, GREEN), (0.32, GREEN), (0.45, GAMBER),
              (0.62, GRED), (1.00, GDARK)])
VMAX = 0.85

order = np.argsort(cong)               # jams drawn last, on top
norm = Normalize(vmin=0.0, vmax=VMAX)
lw = 0.65 + 1.05 * np.clip(cong, 0, VMAX) / VMAX
colors = GMAPS(norm(cong))
seg_list = [segs[i] for i in order]
col_list = [colors[i] for i in order]
lw_list = [lw[i] for i in order]
lc = LineCollection(seg_list, colors=col_list, linewidths=lw_list, capstyle="round",
                    joinstyle="round", zorder=4)
ax.add_collection(lc)

# slow-zone outline
zone = np.array([[6.560, 53.215], [6.578, 53.215], [6.578, 53.223],
                 [6.560, 53.223], [6.560, 53.215]])
zm = merc_arr(zone)
ax.plot(zm[:, 0], zm[:, 1], color=AMBER, lw=1.1, ls=(0, (4, 3)), alpha=0.8, zorder=6)
ax.text(zm[2, 0], zm[2, 1], " slow zone", color=AMBER, fontsize=6.0, va="bottom",
        ha="left", zorder=7, family="DejaVu Sans Mono",
        path_effects=[pe.withStroke(linewidth=2.0, foreground=BG)])

# destination magnets
for lon, lat in [(6.5685, 53.2191), (6.5665, 53.2110), (6.5760, 53.2190)]:
    mx, my = merc(lon, lat)
    glow_marker(ax, mx, my, INK, size=42, ring=True)

# summary numbers
res = json.load(open("pitch_assets/out/sim/simulation-groningen_peak-result.json"))
summ = res.get("summary", {})
n_ag = summ.get("agents_total", "?")
n_rr = summ.get("total_reroutes", "?")

title_block(ax, "AGENT-BASED SIMULATION · PEAK", "Where the morning\npeak jams up", fig_tag="FIG 10A",
            w=0.50, tsize=11.0)
legend_box(ax, 0.035, 0.245, [(GREEN, "Free flow"), (GAMBER, "Slowing"),
                              (GRED, "Heavy"), (GDARK, "Jammed")],
           w=0.34, title="TRAFFIC SPEED", line=True)
panel(ax, 0.62, 0.055, 0.345, 0.050, z=10, alpha=0.86)
ax.text(0.637, 0.080, f"{n_ag} cars · {n_rr} reroutes", transform=ax.transAxes,
        color=MUTE, fontsize=6.2, va="center", ha="left", family="DejaVu Sans Mono", zorder=12)
credit(ax)
finish(fig, "pitch_assets/maps/fig_simulation.png")
