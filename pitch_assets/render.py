"""Ultra-clean dark cartography for the netweevil pitch deck.

Renders netweevil analysis outputs over a neutral CartoDB DarkMatter basemap,
styled to match the deck's neon-on-navy brand system.
"""
import json
import math
import numpy as np
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.collections import LineCollection
from matplotlib.colors import LinearSegmentedColormap, to_rgba, Normalize
from matplotlib.cm import ScalarMappable
import matplotlib.patheffects as pe
import matplotlib.font_manager as fm
import contextily as cx

# ----------------------------------------------------------------------------
# Brand palette (pulled from the deck)
# ----------------------------------------------------------------------------
BG        = "#0B101A"   # deck background
PANEL     = "#162032"   # panel fill
PANEL_HI  = "#1B283E"
BORDER    = "#33415C"
INK       = "#ECF1F8"   # near-white text
MUTE      = "#93A2B8"   # muted text
DIM       = "#5A687C"
MINT      = "#2DE5A0"   # primary accent
SKY       = "#4FC3F7"
LAV       = "#B99AFF"
AMBER     = "#FFC14E"
CORAL     = "#FF6B6B"

R_EARTH = 6378137.0

# Sequential brand ramp (fast/near -> slow/far)
RAMP = LinearSegmentedColormap.from_list(
    "nw_ramp", ["#2DE5A0", "#36D9C0", "#4FC3F7", "#8FA8FF", "#B99AFF", "#E07AB0"]
)
# Congestion ramp (free flow -> jam)
JAM = LinearSegmentedColormap.from_list(
    "nw_jam", ["#2DE5A0", "#9BE26A", "#FFC14E", "#FF8A4C", "#FF6B6B"]
)


def merc(lon, lat):
    """lon/lat (deg) -> web mercator metres (EPSG:3857)."""
    x = R_EARTH * math.radians(lon)
    y = R_EARTH * math.log(math.tan(math.pi / 4 + math.radians(lat) / 2))
    return x, y


def merc_arr(coords):
    a = np.asarray(coords, dtype=float)
    x = R_EARTH * np.radians(a[:, 0])
    y = R_EARTH * np.log(np.tan(np.pi / 4 + np.radians(a[:, 1]) / 2))
    return np.column_stack([x, y])


def new_fig(w_in, h_in, dpi=300):
    fig = plt.figure(figsize=(w_in, h_in), dpi=dpi, facecolor=BG)
    ax = fig.add_axes([0, 0, 1, 1])
    ax.set_facecolor(BG)
    ax.set_xticks([]); ax.set_yticks([])
    for s in ax.spines.values():
        s.set_visible(False)
    return fig, ax


def set_extent(ax, xs, ys, pad=0.10, min_span=None, aspect_wh=None):
    """Fit extent to data with padding; optionally enforce a width/height ratio."""
    xmin, xmax = float(np.min(xs)), float(np.max(xs))
    ymin, ymax = float(np.min(ys)), float(np.max(ys))
    dx, dy = xmax - xmin, ymax - ymin
    cx_, cy_ = (xmin + xmax) / 2, (ymin + ymax) / 2
    dx = max(dx, 1.0); dy = max(dy, 1.0)
    dx *= (1 + 2 * pad); dy *= (1 + 2 * pad)
    if min_span:
        dx = max(dx, min_span); dy = max(dy, min_span)
    if aspect_wh:
        # force dx/dy == aspect_wh, expanding the smaller dimension
        if dx / dy < aspect_wh:
            dx = dy * aspect_wh
        else:
            dy = dx / aspect_wh
    ax.set_xlim(cx_ - dx / 2, cx_ + dx / 2)
    ax.set_ylim(cy_ - dy / 2, cy_ + dy / 2)


def basemap(ax, labels=True, zoom="auto"):
    cx.add_basemap(ax, source=cx.providers.CartoDB.DarkMatterNoLabels,
                   crs="EPSG:3857", zoom=zoom, attribution=False, zorder=0)
    if labels:
        cx.add_basemap(ax, source=cx.providers.CartoDB.DarkMatterOnlyLabels,
                       crs="EPSG:3857", zoom=zoom, attribution=False, zorder=6, alpha=0.55)


def glow_lines(ax, segments, color, lw=1.6, glow=(7, 0.05), base_z=3, alpha=1.0,
               cap="round"):
    """Draw neon line segments with a soft outer glow."""
    gw, ga = glow
    # outer glow (a couple of widening passes)
    for k, mult in enumerate([1.0, 0.6]):
        lc = LineCollection(segments, colors=[to_rgba(color, ga * (1 - 0.3 * k))],
                            linewidths=lw + gw * (1 - 0.35 * k), capstyle=cap,
                            joinstyle="round", zorder=base_z)
        ax.add_collection(lc)
    lc = LineCollection(segments, colors=[to_rgba(color, alpha)], linewidths=lw,
                        capstyle=cap, joinstyle="round", zorder=base_z + 1)
    ax.add_collection(lc)


def glow_marker(ax, x, y, color, size=120, ring=True, z=8):
    if ring:
        ax.scatter([x], [y], s=size * 5.0, color=to_rgba(color, 0.10), zorder=z, linewidths=0)
        ax.scatter([x], [y], s=size * 2.4, color=to_rgba(color, 0.18), zorder=z, linewidths=0)
    ax.scatter([x], [y], s=size, color=color, zorder=z + 1,
               edgecolors=BG, linewidths=1.4)


def panel(ax, x, y, w, h, z=10, fc=PANEL, ec=BORDER, alpha=0.92, lw=1.0, rad=0.02):
    from matplotlib.patches import FancyBboxPatch
    p = FancyBboxPatch((x, y), w, h, transform=ax.transAxes,
                       boxstyle=f"round,pad=0,rounding_size={rad}",
                       linewidth=lw, edgecolor=ec, facecolor=to_rgba(fc, alpha),
                       zorder=z, mutation_aspect=1)
    ax.add_patch(p)


def title_block(ax, kicker, title, fig_tag=None, w=0.70, tsize=11.0):
    """Title-only bar: the title sits centered inside a box sized to fit it."""
    nlines = title.count("\n") + 1
    top = 0.965
    line_h = 0.046
    pad = 0.034
    h = pad * 2 + nlines * line_h
    y0 = top - h
    cy = y0 + h / 2
    panel(ax, 0.035, y0, w, h, z=10, alpha=0.90)
    ax.text(0.060, cy, title, transform=ax.transAxes, color=INK, fontsize=tsize,
            fontweight="bold", va="center", ha="left", multialignment="left",
            linespacing=1.18, zorder=11)
    if fig_tag:
        tag_h = 0.058
        panel(ax, 0.858, top - tag_h, 0.108, tag_h, z=10, fc=MINT, ec=MINT, alpha=0.10)
        ax.text(0.912, top - tag_h / 2, fig_tag, transform=ax.transAxes, color=MINT,
                fontsize=7.5, fontweight="bold", va="center", ha="center",
                family="DejaVu Sans Mono", zorder=11)


def credit(ax):
    ax.text(0.985, 0.018, "© OpenStreetMap  ·  CARTO  ·  netweevil",
            transform=ax.transAxes, color=DIM, fontsize=4.6, va="bottom",
            ha="right", zorder=11, family="DejaVu Sans Mono")


def legend_box(ax, x, y, rows, w=0.30, title=None, line=False):
    """rows = [(color, label), ...]. Draws a compact dark legend panel."""
    rh = 0.050
    n = len(rows)
    pad_top = 0.060 if title else 0.028
    h = pad_top + n * rh + 0.022
    panel(ax, x, y - h, w, h, z=10, alpha=0.90)
    yy = y - 0.040 if title else y - 0.030
    if title:
        ax.text(x + 0.022, y - 0.030, title, transform=ax.transAxes, color=MUTE,
                fontsize=6.0, fontweight="bold", va="center", ha="left",
                family="DejaVu Sans Mono", zorder=12)
        yy = y - 0.072
    for col, lab in rows:
        if line:
            ax.plot([x + 0.024, x + 0.064], [yy, yy], transform=ax.transAxes,
                    color=col, lw=2.6, solid_capstyle="round", zorder=12,
                    path_effects=[pe.Stroke(linewidth=6, foreground=to_rgba(col, 0.18)), pe.Normal()])
        else:
            ax.scatter([x + 0.044], [yy], transform=ax.transAxes, s=55, color=col,
                       edgecolors=BG, linewidths=0.8, zorder=12)
        ax.text(x + 0.082, yy, lab, transform=ax.transAxes, color=INK, fontsize=7.0,
                va="center", ha="left", zorder=12)
        yy -= rh


def colorbar_inset(ax, x, y, w, h, cmap, vmin, vmax, label, ticks, fmt=lambda v: f"{v:g}"):
    panel(ax, x - 0.018, y - 0.052, w + 0.036, h + 0.10, z=10, alpha=0.90)
    cax = ax.inset_axes([x, y, w, h], transform=ax.transAxes, zorder=12)
    grad = np.linspace(0, 1, 256).reshape(1, -1)
    cax.imshow(grad, aspect="auto", cmap=cmap, extent=[vmin, vmax, 0, 1])
    cax.set_yticks([])
    cax.set_xticks(ticks)
    cax.set_xticklabels([fmt(t) for t in ticks], color=INK, fontsize=6.0)
    cax.tick_params(axis="x", colors=BORDER, length=2, pad=2)
    for s in cax.spines.values():
        s.set_color(BORDER); s.set_linewidth(0.6)
    cax.text(0.0, 1.0, label, transform=cax.transAxes, color=MUTE, fontsize=6.0,
             fontweight="bold", va="bottom", ha="left", family="DejaVu Sans Mono",
             zorder=12, clip_on=False)


def finish(fig, path):
    fig.savefig(path, dpi=fig.dpi, facecolor=BG, pad_inches=0)
    plt.close(fig)
    print("wrote", path)
