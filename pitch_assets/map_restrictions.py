import sys, json, numpy as np
sys.path.insert(0, "pitch_assets")
from render import *
import os
FORCE_K = os.environ.get("FORCE_K", "12")

W_IN, H_IN = 4.55, 4.25
d = json.load(open("pitch_assets/out/route_restrict_batch.json"))

items = {it["route_id"]: it for it in d["items"]}

def geom(rid):
    it = items.get(rid)
    if not it or it["status"] != "succeeded" or not it.get("route"):
        return None
    return it["route"]

def frechet_like(a, b):
    """cheap divergence: mean nearest-point distance between polylines (mercator)."""
    A = merc_arr(a); B = merc_arr(b)
    from scipy.spatial import cKDTree
    tb = cKDTree(B)
    da, _ = tb.query(A)
    ta = cKDTree(A)
    db, _ = ta.query(B)
    return (da.mean() + db.mean()) / 2

# choose the pair with the most interesting legal detour
best = None
for k in range(16):
    s = geom(f"strict_{k}"); n = geom(f"naive_{k}")
    if not s or not n or not s.get("geometry") or not n.get("geometry"):
        continue
    sd = s["summary"]["total_distance_m"]; nd = n["summary"]["total_distance_m"]
    viol = n["summary"].get("violation_count", 0)
    div = frechet_like(s["geometry"], n["geometry"])
    # want: naive shorter, real divergence, and naive uses violations
    score = div * (1 + viol) + max(0, sd - nd) * 0.5
    if sd > nd - 5 and div > 25 and (best is None or score > best[0]):
        best = (score, k, s, n, sd, nd, viol, div)

if best is None:
    # relax: just max divergence
    for k in range(16):
        s = geom(f"strict_{k}"); n = geom(f"naive_{k}")
        if not s or not n or not s.get("geometry") or not n.get("geometry"):
            continue
        div = frechet_like(s["geometry"], n["geometry"])
        if best is None or div > best[0]:
            best = (div, k, s, n, s["summary"]["total_distance_m"],
                    n["summary"]["total_distance_m"], n["summary"].get("violation_count", 0), div)

if FORCE_K is not None:
    kk=int(FORCE_K)
    s=geom(f"strict_{kk}"); n=geom(f"naive_{kk}")
    sd=s["summary"]["total_distance_m"]; nd=n["summary"]["total_distance_m"]
    viol=n["summary"].get("violation_count",0); div=frechet_like(s["geometry"],n["geometry"]); k=kk
    best=(0,k,s,n,sd,nd,viol,div)
_, k, s, n, sd, nd, viol, div = best
print(f"chosen pair k={k} strict_dist={sd} naive_dist={nd} naive_violations={viol} divergence={div:.0f}m")

sg = merc_arr(s["geometry"]); ng = merc_arr(n["geometry"])
o = s["origin"]; dest = s["destination"]
ox, oy = merc(o["snapped_lon"], o["snapped_lat"])
dx, dy = merc(dest["snapped_lon"], dest["snapped_lat"])

fig, ax = new_fig(W_IN, H_IN)
allxy = np.vstack([sg, ng])
set_extent(ax, allxy[:, 0], allxy[:, 1], pad=0.22, aspect_wh=W_IN / H_IN)
basemap(ax, labels=True)

def to_segs(m):
    return [m[i:i + 2] for i in range(len(m) - 1)]

# legal route underneath (mint), naive route on top (coral) so the red path
# reads as one consistent, uninterrupted line even where the two cross.
glow_lines(ax, to_segs(sg), MINT, lw=2.4, glow=(7, 0.05), base_z=3, alpha=0.98)
glow_lines(ax, to_segs(ng), CORAL, lw=2.4, glow=(7, 0.05), base_z=5, alpha=0.98)

glow_marker(ax, ox, oy, INK, size=70)
glow_marker(ax, dx, dy, AMBER, size=70)
ax.text(ox, oy, "  A", color=INK, fontsize=8, va="center", ha="left", zorder=11,
        fontweight="bold", path_effects=[pe.withStroke(linewidth=2.4, foreground=BG)])
ax.text(dx, dy, "  B", color=AMBER, fontsize=8, va="center", ha="left", zorder=11,
        fontweight="bold", path_effects=[pe.withStroke(linewidth=2.4, foreground=BG)])

title_block(ax, "TURN & VIA-WAY RESTRICTIONS", "Legal route vs. the\nnaive shortcut", fig_tag="FIG 8A",
            w=0.52, tsize=11.0)
legend_box(ax, 0.035, 0.235, [(MINT, "netweevil: legal route"), (CORAL, "naive: ignores rules")],
           w=0.42, title="SAME ORIGIN → DESTINATION", line=True)
credit(ax)
finish(fig, "pitch_assets/maps/fig_restrictions.png")
