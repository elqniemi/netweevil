import sys, json, numpy as np
sys.path.insert(0, "pitch_assets")
from render import *

W_IN, H_IN = 4.55, 4.25

# grid params must match generator
lon0, lon1 = 6.46, 6.72
lat0, lat1 = 53.15, 53.30
nx, ny = 26, 22

req = json.load(open("pitch_assets/requests/transit_grid.json"))
res = json.load(open("pitch_assets/out/transit_grid_result.json"))

dest = {r["route_id"]: (r["destination"]["lon"], r["destination"]["lat"]) for r in req["requests"]}
tmin = {}
for it in res["items"]:
    rid = it["route_id"]
    r = it["result"]
    if r["outcome"] == "scheduled":
        tmin[rid] = r["summary"]["total_travel_time_s"] / 60.0

VMAX = 35.0
REACH = 37.0  # samples beyond this are treated as not reached
from scipy.interpolate import griddata
from scipy.spatial import cKDTree

# sample points (mercator) with reachable travel time
pts = []
vals = []
for i in range(nx * ny):
    rid = f"acc_{i}"
    if rid in tmin and tmin[rid] <= REACH:
        lon, lat = dest[rid]
        pts.append(merc(lon, lat))
        vals.append(min(tmin[rid], VMAX))
pts = np.array(pts); vals = np.array(vals)

# fine interpolation grid in mercator over the data extent
xmin, ymin = merc(lon0, lat0); xmax, ymax = merc(lon1, lat1)
gx = np.linspace(xmin, xmax, 280)
gy = np.linspace(ymin, ymax, 280)
GX, GY = np.meshgrid(gx, gy)
Z = griddata(pts, vals, (GX, GY), method="linear")

# mask pixels too far from any reachable sample (keeps it to the served area)
tree = cKDTree(pts)
dist, _ = tree.query(np.column_stack([GX.ravel(), GY.ravel()]), k=1)
dist = dist.reshape(GX.shape)
cell = (xmax - xmin) / (nx - 1)
Z = np.where(dist > cell * 0.85, np.nan, Z)

origin_lon, origin_lat = 6.5665, 53.2110
ox, oy = merc(origin_lon, origin_lat)

fig, ax = new_fig(W_IN, H_IN)
ax.set_xlim(xmin, xmax)
cy_ = (ymin + ymax) / 2
half = (xmax - xmin) / 2 / (W_IN / H_IN)
ax.set_ylim(cy_ - half, cy_ + half)

basemap(ax, labels=True)

ax.imshow(Z, extent=[xmin, xmax, ymin, ymax], origin="lower", cmap=RAMP,
          vmin=6, vmax=VMAX, alpha=0.82, zorder=3, interpolation="bilinear")
# isochrone contour lines (no clutter labels)
ax.contour(GX, GY, Z, levels=[15, 25, 35], colors=[to_rgba(INK, 0.40)],
           linewidths=0.6, zorder=4)

# GTFS stops overlay (faint)
stops = np.array(json.load(open("pitch_assets/out/gtfs_stops_bbox.json")))
sm = merc_arr(stops)
ax.scatter(sm[:, 0], sm[:, 1], s=1.6, color=to_rgba(INK, 0.22), zorder=5, linewidths=0)

glow_marker(ax, ox, oy, MINT, size=80)
ax.text(ox, oy, "  Groningen Centraal", color=INK, fontsize=6.5, va="center",
        ha="left", zorder=10, fontweight="bold",
        path_effects=[pe.withStroke(linewidth=2.2, foreground=BG)])

title_block(ax, "MULTIMODAL · WALK + TRANSIT", "Transit reach at 08:00,\nshaded by journey time",
            fig_tag="FIG 9A", w=0.60, tsize=10.5)
colorbar_inset(ax, 0.060, 0.090, 0.34, 0.022, RAMP, 6, VMAX,
               "TOTAL JOURNEY TIME (MIN)", ticks=[10, 20, 30],
               fmt=lambda v: f"{int(v)}")
# small stops note inside its own chip
panel(ax, 0.66, 0.055, 0.30, 0.050, z=10, alpha=0.86)
ax.scatter([0.69], [0.080], transform=ax.transAxes, s=10, color=to_rgba(INK, 0.5),
           zorder=12, linewidths=0)
ax.text(0.715, 0.080, "1,893 GTFS stops", transform=ax.transAxes, color=MUTE,
        fontsize=6.2, va="center", ha="left", zorder=12, family="DejaVu Sans Mono")
credit(ax)
finish(fig, "pitch_assets/maps/fig_transit.png")
