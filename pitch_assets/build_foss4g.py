#!/usr/bin/env python
"""Build the FOSS4G NL 2026 netweevil deck.

Declarative slide specs are rendered twice:
  * python-pptx  -> netweevil_foss4g.pptx (Arial / Courier New, 16:9)
  * PIL          -> pitch_assets/preview_foss4g/slide_NN.png for visual QA

Structure of the talk (three acts, no interleaving):
  Act I   the problem      (slides 1-2)
  Act II  the instrument   (slides 3-10)
  Act III how it got built (slides 11-16), then meaning + close.

Run with pitch_assets/.venv python:
  .venv_maps/bin/python pitch_assets/build_foss4g.py
"""

import os
from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(ROOT, "pitch_assets")
MAPS = os.path.join(ASSETS, "maps")
PREVIEW = os.path.join(ASSETS, "preview_foss4g")
OUT_PPTX = os.path.join(ROOT, "netweevil_foss4g.pptx")

SLIDE_W, SLIDE_H = 13.333, 7.5

BG = "#0B101A"
PANEL = "#162032"
PANEL_HI = "#1B283E"
PANEL_DK = "#101826"
CODE_BG = "#0A0F18"
BORDER = "#263246"
INK = "#ECF1F8"
MUTE = "#93A2B8"
DIM = "#5A687C"
MINT = "#2DE5A0"
SKY = "#4FC3F7"
LAV = "#B99AFF"
AMBER = "#FFC14E"
CORAL = "#FF6B6B"
CODE_TXT = "#C9D4E6"

WIN_FONTS = "/mnt/c/Windows/Fonts"
FONT_FILES = {
    ("sans", False, False): f"{WIN_FONTS}/arial.ttf",
    ("sans", True, False): f"{WIN_FONTS}/arialbd.ttf",
    ("sans", False, True): f"{WIN_FONTS}/ariali.ttf",
    ("sans", True, True): f"{WIN_FONTS}/arialbi.ttf",
    ("mono", False, False): f"{WIN_FONTS}/cour.ttf",
    ("mono", True, False): f"{WIN_FONTS}/courbd.ttf",
    ("mono", False, True): f"{WIN_FONTS}/cour.ttf",
    ("mono", True, True): f"{WIN_FONTS}/courbd.ttf",
}

SLIDES = []


def R(t, b=False, i=False, c=None, f="sans", s=None):
    """One styled run."""
    return {"t": t, "b": b, "i": i, "c": c, "f": f, "s": s}


def slide(notes=""):
    s = {"shapes": [], "notes": notes}
    SLIDES.append(s)
    return s


def rect(s, L, T, W, H, fill=None, line=None, lw=1.0, radius=0.0, text=None):
    s["shapes"].append({
        "kind": "rect", "L": L, "T": T, "W": W, "H": H, "fill": fill,
        "line": line, "lw": lw, "radius": radius, "text": text,
    })


def text(s, L, T, W, paras, size=13, color=INK, bold=False, italic=False,
         align="l", font="sans", spacing=1.12, para_gap=0.06):
    """paras: list of paragraphs; each is a string or a list of runs."""
    norm = []
    for p in paras:
        if isinstance(p, str):
            p = [R(p)]
        norm.append(p)
    s["shapes"].append({
        "kind": "text", "L": L, "T": T, "W": W, "paras": norm, "size": size,
        "color": color, "bold": bold, "italic": italic, "align": align,
        "font": font, "spacing": spacing, "para_gap": para_gap,
    })


def img(s, path, L, T, W, H, border=None, lw=1.25):
    s["shapes"].append({"kind": "img", "path": path, "L": L, "T": T,
                        "W": W, "H": H, "border": border, "lw": lw})


# ---------------------------------------------------------------- chrome ---

def base(s):
    rect(s, 0, 0, SLIDE_W, SLIDE_H, fill=BG)


def footer(s, idx, total):
    text(s, 0.72, 7.06, 4.5, ["netweevil · FOSS4G NL 2026"], size=11,
         color=MINT, bold=True)
    text(s, 11.2, 7.06, 1.4, [f"{idx:02d} / {total}"], size=11,
         color=DIM, align="r")


def chrome(s, kicker, title, accent=MINT, lead=None, lead_w=11.9):
    rect(s, 0.75, 0.72, 0.34, 0.07, fill=accent)
    text(s, 1.22, 0.575, 10.5, [kicker.upper()], size=13, color=accent,
         bold=True)
    text(s, 0.72, 0.95, 12.0, [title], size=33, color=INK, bold=True)
    if lead:
        text(s, 0.72, 1.62, lead_w, [lead] if isinstance(lead, str)
             else lead, size=13.5, color=MUTE, spacing=1.18)


def card(s, L, T, W, H, accent, title, body, fill=PANEL, title_c=INK,
         body_size=12.0):
    rect(s, L, T, W, H, fill=fill, line=BORDER, lw=1.0, radius=0.09)
    rect(s, L + 0.18, T + 0.18, 0.5, 0.06, fill=accent)
    text(s, L + 0.18, T + 0.36, W - 0.36, [title], size=14.5, color=title_c,
         bold=True)
    text(s, L + 0.18, T + 0.72, W - 0.36, [body], size=body_size, color=MUTE,
         spacing=1.16)


def quote_card(s, L, T, W, H, txt, accent=MINT):
    rect(s, L, T, W, H, fill=PANEL, line=BORDER, lw=1.0, radius=0.08,
         text={"runs": [R(txt, i=True, c=INK)], "size": 12.5, "align": "l",
               "ml": 0.36, "mr": 0.2})
    rect(s, L + 0.14, T + 0.14, 0.055, H - 0.28, fill=accent)


def quote_strip(s, L, T, W, H, quote, who, accent=MINT):
    """Full-width attributed quotation."""
    rect(s, L, T, W, H, fill=PANEL_DK, line=BORDER, lw=1.0, radius=0.09,
         text={"runs": [R('"' + quote + '"  ', i=True, c=INK),
                        R(who, c=accent, b=True)],
               "size": 13, "align": "l", "ml": 0.42, "mr": 0.28})
    rect(s, L + 0.16, T + 0.16, 0.055, H - 0.32, fill=accent)


def row(s, L, T, W, accent, lead_txt, tail, size=13.0):
    rect(s, L, T + 0.055, 0.16, 0.16, fill=accent, radius=0.04)
    text(s, L + 0.34, T, W - 0.34,
         [[R(lead_txt + "  ", b=True), R(tail, c=MUTE)]],
         size=size, color=INK, spacing=1.16)


def banner(s, L, T, W, H, lead_txt, tail, accent=MINT):
    rect(s, L, T, W, H, fill=PANEL_DK, line=accent, lw=1.25, radius=0.09)
    text(s, L + 0.28, T + 0.2, W - 0.56,
         [[R(lead_txt + "  ", b=True, c=INK), R(tail, c=MUTE)]],
         size=13.0, spacing=1.18)


def fig_panel(s, path, L=7.46, T=1.62, W=5.16, H=5.16):
    img(s, path, L, T, W, H, border=MINT, lw=1.25)


# ================================================================= slides ===

# -- 0 · title ---------------------------------------------------------------
s = slide(notes=(
    "Hi, I am Elmeri. For a good part of my career I was the person "
    "researchers and public agencies came to with routing questions. This "
    "talk is in three parts. First the questions I could never answer "
    "properly. Then the instrument I built to answer them. And then the "
    "part the title gives away: most of that instrument was written by AI "
    "coding agents, and I want to be honest about what that was like."))
base(s)
rect(s, 10.9, -0.9, 3.4, 2.6, line=BORDER, lw=1.5, radius=0.25)
rect(s, 11.55, -1.25, 3.4, 2.6, line=MINT, lw=1.0, radius=0.25)
text(s, 0.75, 1.28, 10.0, ["FOSS4G NL 2026"], size=15, color=MINT, bold=True)
text(s, 0.68, 1.52, 11.0, ["netweevil"], size=74, color=INK, bold=True)
text(s, 0.75, 2.95, 12.2,
     ["Prototyping a Reproducible Network Analysis Engine "
      "in the Age of AI Agents"],
     size=20, color=INK, bold=True)
text(s, 0.75, 3.52, 10.2,
     ["Years of answering other people's routing questions with duct tape. "
      "Then agents got good, and the tool I always wanted became a few "
      "days of work."],
     size=14, color=MUTE, spacing=1.2)
pills = [("Rust engine", MINT), ("HTTP API", SKY), ("QGIS plugin", AMBER),
         ("Reproducible runs", INK)]
px = 0.75
for label, accent in pills:
    w = 1.62 if len(label) < 12 else 2.1
    rect(s, px, 4.72, w, 0.5, fill=PANEL, line=accent, lw=1.25, radius=0.25,
         text={"runs": [R(label, b=True, c=accent)], "size": 12.5})
    px += w + 0.25
img(s, os.path.join(MAPS, "portrait_circle.png"), 0.75, 5.42, 1.32, 1.32)
text(s, 2.32, 5.56, 8.0, ["Elmeri Niemi"], size=15, color=INK, bold=True)
text(s, 2.32, 5.88, 8.0,
     [[R("Geo Consultant", c=MINT, b=True), R("  ·  Geodienst", c=MUTE)]],
     size=12.5)
text(s, 2.32, 6.18, 10.0,
     ["elmeri.niemi@gmail.com · o.e.niem@rug.nl · me@elmeriniemi.com"],
     size=11.5, color=DIM)
text(s, 2.32, 6.62, 10.0,
     ["Standing on OpenStreetMap, GTFS, twenty years of routing research, "
      "and the QGIS community."],
     size=11.5, color=DIM, italic=True)

# ============================== ACT I · THE PROBLEM =========================

# -- 1 · the questions -------------------------------------------------------
s = slide(notes=(
    "Act one, the problem. These are real questions, lightly anonymised. "
    "The ambulance one is real. The Lelylijn comes up constantly here in "
    "the Netherlands. And the last one is the quiet killer: "
    "reproducibility. For most of my career the honest answer to that one "
    "was no. Not because we were sloppy, but because the answers lived in "
    "one-off scripts glued around whatever tool was closest."))
base(s)
chrome(s, "Act one · the problem",
       "The questions that kept landing on my desk",
       lead=("A big part of my work used to be helping researchers and "
             "public agencies with network analysis. Different projects, "
             "same shape of question:"))
QUOTES = [
    "How much time do ambulances spend on motorways, now that the national "
    "speed limit changed?",
    "What would the Lelylijn do to accessibility in the north of the "
    "Netherlands?",
    "Which road types do trips actually use, and how does that shift "
    "between policies?",
    "Can you route vehicles along deliberately suboptimal paths, and tell "
    "me what that costs?",
    "Can you run the whole thing again next year and get exactly the same "
    "numbers?",
]
qt = 2.18
for q in QUOTES:
    quote_card(s, 0.72, qt, 6.9, 0.78, f'"{q}"')
    qt += 0.90
rect(s, 7.85, 2.18, 4.75, 4.4, fill=PANEL_DK, line=BORDER, lw=1.0,
     radius=0.09)
text(s, 8.13, 2.42, 4.2, ["THE UNCOMFORTABLE PART"], size=12.5, color=AMBER,
     bold=True)
text(s, 8.13, 2.82, 4.2,
     ["Every one of these is a fair question.",
      "Almost every answer was a one-off script, a hand-patched cost "
      "function, and a result I could not fully reconstruct six months "
      "later.",
      [R("This talk is about closing that gap, and about who actually "
         "wrote the code.", c=INK)]],
     size=13, color=MUTE, spacing=1.22, para_gap=0.22)

# -- 2 · the tooling ---------------------------------------------------------
s = slide(notes=(
    "To be clear, I used these tools for years and they are excellent. "
    "OSRM is absurdly fast. Valhalla runs real products. pgRouting gives "
    "you the whole database. QNEAT3 is right there in QGIS. But a custom "
    "cost model meant editing Lua or C++ and re-preprocessing, or writing "
    "SQL per question. And none of them wrote down how a result was "
    "produced in a way I could hand to a reviewer a year later. So last "
    "spring I did the unreasonable thing and built my own engine. Before "
    "I tell you how, let me show you around, because the tool has to earn "
    "your attention on its own."))
base(s)
chrome(s, "Act one · the problem", "Great tools, none of them shaped for "
       "this")
COLS = [
    ("OSRM", SKY, "Fastest car routing at continental scale. The reference "
     "for raw throughput."),
    ("Valhalla", CORAL, "Production multimodal routing, tiled and mature. "
     "Runs real services today."),
    ("pgRouting", AMBER, "Routing inside PostGIS. Arbitrary graph queries "
     "in SQL, deep database integration."),
    ("QNEAT3", LAV, "Zero-setup network analysis inside QGIS. Open the "
     "plugin and go."),
]
cx = 0.72
for name, accent, body in COLS:
    card(s, cx, 2.15, 2.25, 2.6, accent, name, body)
    cx += 2.41
card(s, cx, 2.15, 2.25, 2.6, MINT, "The gap",
     "Custom cost models meant Lua, C++ or SQL per question, plus "
     "re-preprocessing. And nothing recorded how a result was produced.",
     fill=PANEL_HI)
banner(s, 0.72, 5.25, 11.9, 1.2,
       "I used pgRouting and Valhalla for years, and they are excellent "
       "at their jobs.",
       "But every research question became a fight with someone else's "
       "schema. So last spring I did the unreasonable thing and built my "
       "own. Let me show you around first.")

# ============================ ACT II · THE INSTRUMENT =======================

# -- 3 · what it is ----------------------------------------------------------
s = slide(notes=(
    "Act two, the instrument. Netweevil is a Rust workspace, eleven "
    "crates, one binary. The CLI does import, profile compilation, "
    "analysis, simulation and reporting. The same engine serves an HTTP "
    "API that a QGIS plugin talks to. Everything lands in one local "
    "dot-netweevil directory. No database, no tile server, nothing to "
    "operate before you can ask a question."))
base(s)
chrome(s, "Act two · the instrument",
       "An analysis engine, not a production backend",
       lead=("A Rust workspace, 11 crates, one binary for import, profile "
             "compilation, analysis, simulation and reporting."))
CARDS5 = [
    (MINT, "CLI", "Scriptable, deterministic, CI friendly. Import, "
     "compile, analyze, simulate, report."),
    (SKY, "HTTP API", "Axum service, datasets and profiles held in memory. "
     "JSON or GeoJSON, ready for the plugin or a notebook."),
    (AMBER, "QGIS plugin", "QGIS 3.28 to 4.99, Qt 5 and Qt 6. Route, OD, "
     "matrix, service area, transit and live simulation as grouped "
     "layers."),
    (INK, "Reports", "Run manifests plus GeoJSON, CSV, GPKG, Parquet and "
     "GeoParquet you can defend later."),
]
cx = 0.72
for accent, name, body in CARDS5:
    card(s, cx, 2.3, 2.885, 2.3, accent, name, body)
    cx += 3.005
banner(s, 0.72, 5.05, 11.9, 1.15,
       "It reads OSM extracts and Overture Maps GeoParquet, imports GTFS, "
       "and keeps every derived artifact in one local directory.",
       "No database, no tile server, no services to babysit before you "
       "can ask a question.")

# -- 4 · profiles ------------------------------------------------------------
s = slide(notes=(
    "The ambulance question from act one. In netweevil, a cost model is a "
    "YAML file. Speeds by tag, surface factors, turn penalties, tolls, "
    "and how posted speed limits are treated. The national speed limit "
    "study becomes two profiles and a diff: same requests, compare the "
    "time spent per road class before and after. Compilation takes "
    "seconds, and the profile is version controlled next to the results, "
    "so the cost model can literally go in the paper's appendix."))
base(s)
chrome(s, "Act two · cost models",
       "The ambulance question is a YAML file now",
       lead=("Speeds, surface factors, turn penalties, ferries, tolls: the "
             "whole cost model is a plain YAML profile. Edit, recompile in "
             "seconds, route again."),
       lead_w=6.3)
ROWS6 = [
    (MINT, "A speed limit study becomes a diff.",
     "Two profiles, old and new motorway speeds, the same request batch, "
     "and a road-type breakdown on every route."),
    (SKY, "Any mode you can describe.",
     "Car, bicycle, pedestrian, ambulance, or something that exists only "
     "in one study."),
    (AMBER, "Readable by reviewers.",
     "The complete cost model fits in a paper appendix, version "
     "controlled next to the results."),
]
rt = 2.75
for accent, lead_txt, tail in ROWS6:
    row(s, 0.72, rt, 6.35, accent, lead_txt, tail)
    rt += 1.25
CODE = [
    "speed_rules:",
    "  - match: { highway: motorway }",
    "    speed_kph: 150",
    "factors:",
    "  - match: { surface: sand }",
    "    speed_factor: 0.25",
    "  - match: { smoothness: impassable }",
    "    speed_factor: 0.08",
    "turns:",
    "  uturn_penalty_s: 12",
    "preferences:",
    "  use_tolls: 1.0",
    "returns:",
    "  explain_cost_derivation: true",
]
rect(s, 7.5, 1.62, 5.1, 4.6, fill=CODE_BG, line=BORDER, lw=1.0, radius=0.09)
text(s, 7.82, 1.88, 4.6, ["ambulance_emergency_call_v1.yml"], size=11,
     color=DIM, font="mono")
code_paras = []
for ln in CODE:
    if ":" in ln:
        key, rest = ln.split(":", 1)
        val_c = AMBER if any(ch.isdigit() for ch in rest) else SKY
        code_paras.append([R(key, c=MINT, f="mono"),
                           R(":", c=CODE_TXT, f="mono"),
                           R(rest, c=val_c, f="mono")])
    else:
        code_paras.append([R(ln, c=CODE_TXT, f="mono")])
text(s, 7.82, 2.28, 4.6, code_paras, size=11.5, font="mono", spacing=1.18,
     para_gap=0.035)

# -- 5 · toolbox + service area ---------------------------------------------
s = slide(notes=(
    "The everyday requests are all native. Single routes with full "
    "geometry, batches of thousands in one deterministic run, "
    "origin-destination pairs, time and distance matrices, service areas, "
    "accessibility scoring. The map is a drive-time service area from the "
    "centre of Groningen, computed on the reachable network itself, not "
    "as a smoothed hull: about 107 thousand edges and 2,650 kilometres of "
    "road within fifteen minutes. This is the bread and butter behind "
    "every accessibility question."))
base(s)
chrome(s, "Act two · the toolbox", "The standard answers, one engine",
       lead=("The everyday request types are native, with no glue code "
             "between them:"), lead_w=6.4)
ROWS7 = [
    (MINT, "Route", "single path, ranked alternatives, full geometry."),
    (MINT, "Route batch", "thousands of routes in one deterministic run."),
    (MINT, "OD pairs", "origins to destinations from CSV or JSON."),
    (MINT, "Matrix", "many-to-many time and distance."),
    (MINT, "Service area", "isochrones and the reachable network itself."),
    (MINT, "Accessibility", "who can reach what, scored across the "
     "network."),
]
rt = 2.35
for accent, lead_txt, tail in ROWS7:
    row(s, 0.72, rt, 6.4, accent, lead_txt, tail, size=13.5)
    rt += 0.74
fig_panel(s, os.path.join(MAPS, "fig_service_area.png"))

# -- 6 · suboptimal paths ----------------------------------------------------
s = slide(notes=(
    "One of my favourite recurring questions: what happens when vehicles "
    "do not take the optimal path? People want this for exposure studies, "
    "for spreading fleets over corridors, for making simulated traffic "
    "believable, or just to know how expensive the second-best option is. "
    "Most routers treat alternatives as a UI feature. Here they are a "
    "first-class result: every alternative reports its extra minutes and "
    "metres against the optimum, so suboptimality has a price tag. On "
    "this map the fastest crossing of Groningen takes 14.4 minutes, two "
    "alternatives cost less than a minute extra, and the wide northern "
    "ring costs 3.7 minutes more. That is the whole analysis, as one "
    "request."))
base(s)
chrome(s, "Act two · suboptimal paths",
       "What does the second-best route cost?",
       lead=("What happens when vehicles do not take the optimal path? "
             "That question came up more often than almost any other."),
       lead_w=6.4)
ROWS8 = [
    (MINT, "Ranked alternatives with price tags.",
     "Each alternative reports its extra minutes and metres against the "
     "optimum, so suboptimality is measurable, not vague."),
    (SKY, "Useful far beyond navigation.",
     "Exposure studies, fleet dispersion, believable simulated traffic, "
     "sensitivity checks on a corridor before roadworks."),
    (LAV, "Same guarantees as the main route.",
     "Alternatives respect the same profile, restrictions and breakdowns "
     "as the optimal path."),
]
rt = 2.6
for accent, lead_txt, tail in ROWS8:
    row(s, 0.72, rt, 6.4, accent, lead_txt, tail)
    rt += 1.3
fig_panel(s, os.path.join(MAPS, "fig_alternatives.png"))

# -- 7 · transit / Lelylijn --------------------------------------------------
s = slide(notes=(
    "And the Lelylijn. GTFS feeds import next to the road network, and a "
    "journey stitches walking and scheduled services together. The map "
    "shows transit reach from Groningen central station at eight in the "
    "morning, on the real national feed, about nineteen hundred stops in "
    "this extract. The trick for a line that does not exist yet: describe "
    "it as ordinary GTFS rows, import the synthetic feed, run the same "
    "accessibility query, and difference the two surfaces. The before and "
    "after map is the answer to the question."))
base(s)
chrome(s, "Act two · multimodal",
       "What would the Lelylijn actually change?",
       lead=("GTFS feeds import next to the road network, and journeys "
             "stitch walking and scheduled services together."),
       lead_w=6.4)
ROWS9 = [
    (SKY, "Accessibility, before and after.",
     "Run transit reach on today's feed. Add the hypothetical line as "
     "ordinary GTFS rows, run it again, difference the surfaces."),
    (MINT, "Real schedules, real transfers.",
     "Service dates, transfer times and access walks are part of the "
     "query, not an afterthought."),
    (AMBER, "Time of day matters.",
     "Reach at 08:00 is not reach at 22:00. The query takes a clock "
     "time."),
]
rt = 2.6
for accent, lead_txt, tail in ROWS9:
    row(s, 0.72, rt, 6.4, accent, lead_txt, tail)
    rt += 1.3
fig_panel(s, os.path.join(MAPS, "fig_transit.png"))

# -- 8 · simulation ----------------------------------------------------------
s = slide(notes=(
    "Routing tells you what is optimal. Simulation tells you what happens "
    "when seven hundred drivers all believe that at once. The engine "
    "includes agent-based traffic simulation on the same network: "
    "spillback queues, live rerouting when drivers hit jams, slow zones "
    "and closures that can change mid-run. This snapshot is a morning "
    "peak over Groningen, edges coloured from free flow to jammed, with "
    "the city-centre slow zone outlined. Frames land as layers, so the "
    "whole run plays back inside QGIS."))
base(s)
chrome(s, "Act two · beyond routing",
       "Agent-based traffic, in the same tool",
       lead=("Dispatch a fleet over the real network and watch congestion "
             "emerge: queues, rerouting, zones, closures."),
       lead_w=6.4)
ROWS10 = [
    (AMBER, "From routes to traffic.",
     "Routing says what is optimal. Simulation says what happens when "
     "700 drivers all believe that at once."),
    (MINT, "Scenarios as YAML.",
     "Fleets, departure curves, zones and closures are files, so a "
     "scenario is reviewable and repeatable."),
    (SKY, "Playback in QGIS.",
     "Frame-by-frame agent positions land as layers you can animate."),
]
rt = 2.6
for accent, lead_txt, tail in ROWS10:
    row(s, 0.72, rt, 6.4, accent, lead_txt, tail)
    rt += 1.3
fig_panel(s, os.path.join(MAPS, "fig_simulation.png"))

# -- 9 · QGIS ----------------------------------------------------------------
s = slide(notes=(
    "Most of the people who asked me those questions live in QGIS, so "
    "answers have to land there. The plugin talks to the local API and "
    "brings results back as grouped, styled layers: routes, service "
    "areas, transit stops, simulation frames. It supports QGIS 3.28 "
    "through 4.99 on both Qt 5 and Qt 6, and it is packaged like any "
    "normal plugin. For a municipal analyst the engine is invisible; "
    "there is a panel, and maps come out."))
base(s)
chrome(s, "Act two · where analysts work", "QGIS native, end to end",
       lead=("Most of the people who ask these questions live in QGIS, so "
             "the answers should land there too."),
       lead_w=6.4)
ROWS12 = [
    (AMBER, "Click to run.",
     "Route, OD, matrix, service area, transit and simulation tabs, "
     "against a local API."),
    (MINT, "Grouped, styled layers.",
     "Results arrive organised and ready to map. No manual imports, no "
     "leftover scratch files."),
    (SKY, "QGIS 3.28 through 4.99.",
     "Qt 5 and Qt 6, packaged as a normal installable plugin."),
]
rt = 2.6
for accent, lead_txt, tail in ROWS12:
    row(s, 0.72, rt, 6.4, accent, lead_txt, tail)
    rt += 1.3
fig_panel(s, os.path.join(MAPS, "fig_layers.png"))

# -- 10 · reproducibility ----------------------------------------------------
s = slide(notes=(
    "The fifth question from act one: run it again next year, get the "
    "same numbers. That is answered structurally, not by discipline. "
    "Every run writes a manifest pinning the source data and its hashes, "
    "the exact compiled cost model, the software and algorithm version, "
    "and the full request. There is no hidden state in a UI somewhere. "
    "That is the difference between a demo and an instrument you can "
    "publish from. And that closes the tour. Now the part of the story I "
    "have been saving: who actually built all this."))
base(s)
chrome(s, "Act two · run it again next year", "Reproducible by construction",
       lead=("Every run writes a manifest that pins what produced the "
             "result. The fifth question from the start, answered "
             "structurally rather than by discipline."))
CARDS13 = [
    (MINT, "Source provenance", "OSM extract path and hash, import "
     "timestamps, bundle IDs."),
    (SKY, "Profile hashes", "The exact compiled cost model behind the "
     "run."),
    (AMBER, "Software versions", "Engine and algorithm version pinned to "
     "every result."),
    (LAV, "Run metadata", "Request, parameters and timing, captured "
     "rather than implied."),
]
cx = 0.72
for accent, name, body in CARDS13:
    card(s, cx, 2.35, 2.885, 1.85, accent, name, body)
    cx += 3.005
banner(s, 0.72, 4.6, 11.9, 1.2,
       "No hidden state.",
       "Anything you can configure lives in files, payloads or manifests. "
       "That is what turns a demo into an instrument you can publish "
       "from.")

# ========================== ACT III · HOW IT GOT BUILT ======================

# -- 11 · the turn -----------------------------------------------------------
s = slide(notes=(
    "Act three. Everything you just saw was built by one analyst, mostly "
    "in the evenings, and mostly not typed by me. The engine I wanted was "
    "never going to be a funded project; nobody budgets a routing engine "
    "for one person. What changed this spring is that the cost side of "
    "that equation collapsed. I want to be precise here: I did not "
    "vibe-code a router. I wrote specs, argued with plans, reviewed every "
    "diff, and verified outputs against tools I trust. But the agent did "
    "most of the typing, and honestly, a lot of the engineering is better "
    "than what I would have produced alone."))
base(s)
chrome(s, "Act three · how it got built", "So who wrote all this?",
       lead=("Mostly not me. The engine I wanted was never going to be a "
             "funded project. It became real this spring as a side effect "
             "of AI coding agents getting good."))
ROWS3 = [
    (MINT, "Software for an audience of one.",
     "No predefined schemas, no upstream maintainers to convince. Request "
     "and result models shaped exactly like my analyses, because nobody "
     "else has to agree."),
    (SKY, "I stopped typing most of the code.",
     "I write the spec, argue with the plan, review the diff, check the "
     "output. 37 of the 75 commits in the repo carry a Co-Authored-By: "
     "Claude trailer."),
    (LAV, "The code is often better than what I would write.",
     "Not a comfortable sentence, but true. The test discipline and the "
     "algorithm work go past what I would have shipped on my own "
     "evenings."),
    (AMBER, "Days of work, not years.",
     "About a week of actual working days, spread over three and a half "
     "months of calendar. The next slide has the receipts."),
]
rt = 2.35
for accent, lead_txt, tail in ROWS3:
    row(s, 0.72, rt, 11.9, accent, lead_txt, tail)
    rt += 1.08

# -- 12 · timeline -----------------------------------------------------------
s = slide(notes=(
    "This is the actual commit history. First commit in March, and then "
    "long stretches of nothing, because this is an evenings and weekends "
    "project and I have a life. The work concentrates in a few bursts: "
    "the core engine and routing first, then the accelerator and profile "
    "system, then one long day in June that added GTFS transit and the "
    "traffic simulation, and one in July for Overture Maps import. "
    "Seventy five commits, eight days with commits on them, forty three "
    "thousand lines of Rust. That used to be a team and a year."))
base(s)
chrome(s, "Act three · the receipts",
       "Three and a half months of calendar, a week of work")
img(s, os.path.join(MAPS, "fig_timeline.png"), 0.79, 1.72, 11.75, 5.28,
    border=BORDER, lw=1.0)

# -- 13 · process ------------------------------------------------------------
s = slide(notes=(
    "How the work actually feels. Every feature starts as a written plan "
    "that we argue about before any code exists; the typing is the cheap "
    "part now. The single biggest lever is the quote at the bottom, from "
    "Boris Cherny, who created Claude Code: give the agent a way to "
    "verify its output, and it iterates until the result is great. He "
    "puts the gain at two to three times, and that matches my experience "
    "exactly. For this engine, verification meant tests, fixture "
    "corpora, and rendered maps the agent can look at. When the agent "
    "gets something wrong, the correction goes into the project "
    "instructions file, not the chat, so the mistake stops recurring. "
    "And the work runs in parallel: one session on GTFS import, another "
    "on the QGIS plugin. Boris says he runs dozens at once; I am not "
    "there yet, but even three changes what an evening is worth. None of "
    "this is my invention, and that is the point: the patterns are "
    "public, and they transfer straight to geospatial work."))
base(s)
chrome(s, "Act three · the process", "Working with agents, in practice")
CARDS14 = [
    (MINT, "Plan first, code later.",
     "Every feature starts as a written plan we argue about before any "
     "code exists. The typing is the cheap part now."),
    (SKY, "Give the agent a way to verify.",
     "The single biggest lever: tests, fixture corpora, a rendered map "
     "the agent can look at. Wrong routes glow on a dark basemap."),
    (AMBER, "Corrections become rules.",
     "When the agent gets something wrong, the fix goes into the project "
     "instructions file, not the chat. That class of mistake stops "
     "happening."),
    (LAV, "Parallel work, small reviews.",
     "One session builds GTFS import while another writes the QGIS "
     "plugin. My job is direction and review, in small pieces."),
]
positions = [(0.72, 1.8), (6.75, 1.8), (0.72, 3.75), (6.75, 3.75)]
for (accent, name, body), (cx, cy) in zip(CARDS14, positions):
    card(s, cx, cy, 5.86, 1.8, accent, name, body, body_size=12.5)
quote_strip(s, 0.72, 5.8, 11.9, 0.95,
            "Give Claude a way to verify its output. Once you do that, "
            "Claude will iterate until the result is great.",
            "Boris Cherny, creator of Claude Code")

# -- 14 · trust --------------------------------------------------------------
s = slide(notes=(
    "The obvious objection: can you trust code an AI wrote? My answer is "
    "that it is the same question as trusting any router, and the same "
    "answer applies: verification, not confidence. The fast path is a "
    "customizable contraction hierarchy, and it is differential tested "
    "route for route against a brute force engine that ships inside the "
    "same binary. Same answers, about 45 times faster. The cases that "
    "quietly break routers, via-way turn restrictions, are modelled and "
    "on this map: the red route is what you get if you ignore them. And "
    "this is not just my hobby standard. Simon Willison recently had an "
    "agent do a final review before a release and it found five release "
    "blockers for about 150 dollars. Machine verification is cheap now. "
    "Blind trust is the only expensive option."))
base(s)
chrome(s, "Act three · the obvious objection",
       "Can you trust code an AI wrote?",
       lead=("It is the same question as trusting any router, and it has "
             "the same answer: verification, built into the engine."),
       lead_w=6.4)
ROWS11 = [
    (MINT, "An exact engine rides along as the oracle.",
     "The CCH accelerator is differential tested route for route against "
     "brute force. Same answers, 131 ms median down to 2.9 ms on 1.02M "
     "edge states."),
    (CORAL, "The hard cases have tests.",
     "Via-way turn restrictions, the kind that quietly break routers, "
     "are modelled and verified. The red route is what ignoring them "
     "looks like."),
    (SKY, "149 tests and counting.",
     "The agent writes tests more patiently than I ever did."),
]
rt = 2.6
for accent, lead_txt, tail in ROWS11:
    row(s, 0.72, rt, 6.4, accent, lead_txt, tail)
    rt += 1.3
fig_panel(s, os.path.join(MAPS, "fig_restrictions.png"))

# -- 15 · honest part --------------------------------------------------------
s = slide(notes=(
    "Now the honest part. The agent did not know what a via-way "
    "restriction is, or what a wrong isochrone looks like. I did. Agents "
    "amplify what you bring, and if you skip the understanding you are "
    "taking on debt in your own head, not just in the code. Second, the "
    "bottleneck moved: I read far more code than I write now. Theo "
    "Browne asked in July how much better the models have to get before "
    "we stop reading the code. Maybe he is right for product code. For "
    "an engine whose numbers end up in policy discussions, my answer is: "
    "not yet. Third, we are lucky in this field: our wrong answers are "
    "visible. A route through a canal does not need a stack trace. Every "
    "rendered map doubled as a regression test."))
base(s)
chrome(s, "Act three · what the agent did not do", "The honest part")
ROWS15 = [
    (MINT, "Domain knowledge was the input.",
     "I knew what a via-way restriction is and what a wrong isochrone "
     "looks like. Agents amplify what you bring. Skip the understanding "
     "and you take on debt in your own head, not just in the code."),
    (SKY, "Review became the bottleneck.",
     "I read far more code than I write now. The time did not disappear, "
     "it moved. For numbers that end up in policy discussions, that is "
     "the right trade."),
    (AMBER, "Geospatial got lucky.",
     "Our wrong answers are visible. A route through a canal does not "
     "need a stack trace. Every rendered map doubled as a regression "
     "test."),
]
rt = 2.1
for accent, lead_txt, tail in ROWS15:
    row(s, 0.72, rt, 11.9, accent, lead_txt, tail)
    rt += 1.18
quote_strip(s, 0.72, 5.75, 11.9, 0.95,
            "How much better do the models have to get before you'll stop "
            "reading the code?",
            "Theo Browne, July 2026. My answer, for this engine: not yet.",
            accent=SKY)

# -- 16 · the essence --------------------------------------------------------
s = slide(notes=(
    "If you remember one line from this talk, make it this one. It is "
    "from yacine, a builder on X, and Andrej Karpathy passed it around "
    "for a reason. Agents can produce, and produce, and produce. "
    "Understanding does not come included. If you let it erode, you and "
    "your project are done; you just do not know it yet. In geospatial "
    "work this is not philosophy. Our numbers steer ambulances, bus "
    "lines and policy debates. The engine is mine because I understand "
    "it, not because I typed it. That is the deal the agents offer, and "
    "it is a good deal, but understanding is the part you can never "
    "hand over."))
base(s)
rect(s, -0.8, 5.9, 4.2, 3.0, fill=PANEL_DK, radius=0.25)
rect(s, 10.9, -1.0, 4.0, 3.0, fill=PANEL_DK, radius=0.25)
text(s, 0.75, 1.55, 10.0, ["THE ESSENCE"], size=13, color=MINT, bold=True)
text(s, 0.72, 1.95, 12.1,
     ['"You can outsource your thinking,', 'but not your understanding."'],
     size=36, color=INK, bold=True, para_gap=0.1)
text(s, 0.75, 3.65, 10.5,
     [[R("yacine (@yacineMTB)", c=MINT, b=True),
       R(", amplified by Andrej Karpathy", c=DIM)]], size=13)
rect(s, 0.75, 4.25, 3.2, 0.02, fill=BORDER)
text(s, 0.75, 4.55, 11.0,
     ["Agents will produce, and produce, and produce. Understanding does "
      "not come included.",
      "In geospatial work our numbers steer ambulances, bus lines and "
      "policy debates. The engine is mine because I understand it, not "
      "because I typed it."],
     size=15, color=MUTE, spacing=1.25, para_gap=0.16)

# -- 17 · what it means ------------------------------------------------------
s = slide(notes=(
    "So what does this mean for this room. Bespoke analysis tools used "
    "to be a luxury for well-funded teams. That constraint is gone: a "
    "research group or a municipal team can now afford an instrument "
    "fitted to their questions instead of bending the question to fit "
    "the tool. But none of it works without the commons. The agents "
    "learned from open code, and this engine stands on OSM, GTFS, twenty "
    "years of routing papers and QGIS. Publishing our tools back is the "
    "least we owe. And the skill that matters has moved to exactly where "
    "this community is strong: knowing what to ask, and knowing how to "
    "check the answer."))
base(s)
chrome(s, "What it means for this room",
       "Personal instruments, open foundations")
ROWS16 = [
    (MINT, "Bespoke tools are suddenly affordable.",
     "A research group or a municipal team can afford an instrument "
     "fitted to their questions, instead of bending the question to fit "
     "the tool."),
    (SKY, "This only works on top of the commons.",
     "OpenStreetMap, GTFS, twenty years of routing papers, QGIS. The "
     "agents learned from open code. Publishing ours back is the least "
     "we owe."),
    (LAV, "The valuable skill moved.",
     "It was never typing speed. It is knowing what to ask for and how "
     "to check the answer. That is exactly what this community is good "
     "at."),
]
rt = 2.25
for accent, lead_txt, tail in ROWS16:
    row(s, 0.72, rt, 11.9, accent, lead_txt, tail, size=14)
    rt += 1.45

# -- 18 · closing ------------------------------------------------------------
s = slide(notes=(
    "So, in one breath. Netweevil is not a product and it is not trying "
    "to replace OSRM, Valhalla, pgRouting or QNEAT3. It is an instrument: "
    "a local engine for the research case, where every parameter has to "
    "be explainable, every run reproducible, and the output shaped to "
    "the question. It was built in days, on top of decades of open "
    "source, by one analyst who understood the problem and a lot of "
    "patient machines. Thank you, and I am happy to take questions."))
base(s)
rect(s, -0.8, 5.9, 4.2, 3.0, fill=PANEL_DK, radius=0.25)
rect(s, 10.9, -1.0, 4.0, 3.0, fill=PANEL_DK, radius=0.25)
text(s, 0.75, 1.7, 10.0, ["IN ONE BREATH"], size=13, color=MINT, bold=True)
text(s, 0.72, 2.1, 12.0, ["Not a product.", "An instrument."], size=38,
     color=INK, bold=True, para_gap=0.1)
text(s, 0.75, 3.75, 10.8,
     ["A local engine for the research case: every parameter explainable, "
      "every run reproducible, output shaped to the question. Built in "
      "days, on top of decades of open source."],
     size=16.5, color=MUTE, spacing=1.25)
rect(s, 0.75, 4.95, 3.2, 0.02, fill=BORDER)
text(s, 0.75, 5.25, 11.0,
     [[R("Thank you.  ", b=True, c=INK),
       R("Questions are the whole point.", c=MUTE)]], size=14)
text(s, 0.75, 6.15, 8.0, ["netweevil"], size=16, color=MINT, bold=True)
text(s, 0.75, 6.48, 10.0,
     ["Elmeri Niemi · Geo Consultant, Geodienst · FOSS4G NL 2026",
      "elmeri.niemi@gmail.com · o.e.niem@rug.nl · me@elmeriniemi.com"],
     size=11.5, color=DIM)

# Footers on all content slides (skip title and closing).
for _i in range(1, len(SLIDES) - 1):
    footer(SLIDES[_i], _i + 1, len(SLIDES))

# ------------------------------------------------------------- renderers ---


def build_pptx(path):
    from pptx import Presentation
    from pptx.util import Inches, Pt, Emu
    from pptx.dml.color import RGBColor
    from pptx.enum.shapes import MSO_SHAPE
    from pptx.enum.text import PP_ALIGN, MSO_ANCHOR

    def rgb(hexc):
        return RGBColor.from_string(hexc.lstrip("#"))

    prs = Presentation()
    prs.slide_width = Emu(int(SLIDE_W * 914400))
    prs.slide_height = Emu(int(SLIDE_H * 914400))
    blank = prs.slide_layouts[6]

    for spec in SLIDES:
        sl = prs.slides.add_slide(blank)
        for sh in spec["shapes"]:
            if sh["kind"] == "rect":
                shape_type = (MSO_SHAPE.ROUNDED_RECTANGLE if sh["radius"] > 0
                              else MSO_SHAPE.RECTANGLE)
                sp = sl.shapes.add_shape(
                    shape_type, Inches(sh["L"]), Inches(sh["T"]),
                    Inches(sh["W"]), Inches(sh["H"]))
                if sh["radius"] > 0:
                    try:
                        sp.adjustments[0] = min(
                            0.5, sh["radius"] / max(0.01, min(sh["W"],
                                                              sh["H"])))
                    except Exception:
                        pass
                if sh["fill"]:
                    sp.fill.solid()
                    sp.fill.fore_color.rgb = rgb(sh["fill"])
                else:
                    sp.fill.background()
                if sh["line"]:
                    sp.line.color.rgb = rgb(sh["line"])
                    sp.line.width = Pt(sh["lw"])
                else:
                    sp.line.fill.background()
                sp.shadow.inherit = False
                if sh["text"]:
                    tf = sp.text_frame
                    tf.word_wrap = True
                    tf.vertical_anchor = MSO_ANCHOR.MIDDLE
                    tf.margin_left = Inches(sh["text"].get("ml", 0.05))
                    tf.margin_right = Inches(sh["text"].get("mr", 0.05))
                    tf.margin_top = tf.margin_bottom = Inches(0.02)
                    p = tf.paragraphs[0]
                    p.alignment = (PP_ALIGN.LEFT
                                   if sh["text"].get("align") == "l"
                                   else PP_ALIGN.CENTER)
                    p.line_spacing = 1.14
                    for run in sh["text"]["runs"]:
                        r = p.add_run()
                        r.text = run["t"]
                        r.font.size = Pt(sh["text"].get("size", 12))
                        r.font.bold = run["b"]
                        r.font.italic = run["i"]
                        r.font.name = ("Courier New" if run["f"] == "mono"
                                       else "Arial")
                        r.font.color.rgb = rgb(run["c"] or INK)
            elif sh["kind"] == "text":
                tb = sl.shapes.add_textbox(
                    Inches(sh["L"]), Inches(sh["T"]), Inches(sh["W"]),
                    Inches(0.4))
                tf = tb.text_frame
                tf.word_wrap = True
                tf.margin_left = tf.margin_right = 0
                tf.margin_top = tf.margin_bottom = 0
                first = True
                for para in sh["paras"]:
                    p = tf.paragraphs[0] if first else tf.add_paragraph()
                    first = False
                    p.alignment = {"l": PP_ALIGN.LEFT, "c": PP_ALIGN.CENTER,
                                   "r": PP_ALIGN.RIGHT}[sh["align"]]
                    p.line_spacing = sh["spacing"]
                    p.space_after = Pt(sh["para_gap"] * 72)
                    for run in para:
                        r = p.add_run()
                        r.text = run["t"]
                        r.font.size = Pt(run["s"] or sh["size"])
                        r.font.bold = run["b"] or sh["bold"]
                        r.font.italic = run["i"] or sh["italic"]
                        r.font.name = ("Courier New"
                                       if (run["f"] == "mono"
                                           or sh["font"] == "mono")
                                       else "Arial")
                        r.font.color.rgb = rgb(run["c"] or sh["color"])
            elif sh["kind"] == "img":
                im = Image.open(sh["path"])
                iw, ih = im.size
                box_ar = sh["W"] / sh["H"]
                img_ar = iw / ih
                if img_ar >= box_ar:
                    w = sh["W"]
                    h = w / img_ar
                else:
                    h = sh["H"]
                    w = h * img_ar
                L = sh["L"] + (sh["W"] - w) / 2
                T = sh["T"] + (sh["H"] - h) / 2
                sl.shapes.add_picture(sh["path"], Inches(L), Inches(T),
                                      Inches(w), Inches(h))
                if sh["border"]:
                    bord = sl.shapes.add_shape(
                        MSO_SHAPE.ROUNDED_RECTANGLE, Inches(L), Inches(T),
                        Inches(w), Inches(h))
                    bord.adjustments[0] = 0.015
                    bord.fill.background()
                    bord.line.color.rgb = rgb(sh["border"])
                    bord.line.width = Pt(sh["lw"])
                    bord.shadow.inherit = False
        if spec["notes"]:
            sl.notes_slide.notes_text_frame.text = spec["notes"]
    prs.save(path)
    print(f"wrote {path}")


# ----------------------------------------------------------- PIL preview ---

SCALE = 150  # px per inch
_font_cache = {}


def get_font(family, bold, italic, size_pt):
    key = (family, bold, italic, round(size_pt * 2))
    if key not in _font_cache:
        path = FONT_FILES[(family, bold, italic)]
        _font_cache[key] = ImageFont.truetype(
            path, int(round(size_pt * SCALE / 72.0)))
    return _font_cache[key]


def wrap_runs(runs, max_w, size, d):
    """Greedy run-aware word wrap. Returns list of lines (list of runs)."""
    words = []
    for run in runs:
        parts = run["t"].split(" ")
        for j, w in enumerate(parts):
            words.append((w + (" " if j < len(parts) - 1 else ""), run))
    lines, cur, cur_w = [], [], 0.0
    for word, run in words:
        f = get_font(run["f"], run["b"], run["i"], run["s"] or size)
        ww = d.textlength(word, font=f)
        if cur and cur_w + ww > max_w:
            lines.append(cur)
            cur, cur_w = [], 0.0
        cur.append((word, run))
        cur_w += ww
    if cur:
        lines.append(cur)
    return lines


def build_previews():
    os.makedirs(PREVIEW, exist_ok=True)
    for i, spec in enumerate(SLIDES):
        im = Image.new("RGB", (int(SLIDE_W * SCALE), int(SLIDE_H * SCALE)),
                       BG)
        d = ImageDraw.Draw(im)
        for sh in spec["shapes"]:
            L, T = sh["L"] * SCALE, sh["T"] * SCALE
            if sh["kind"] == "rect":
                W, H = sh["W"] * SCALE, sh["H"] * SCALE
                rad = sh["radius"] * SCALE
                kw = {}
                if sh["fill"]:
                    kw["fill"] = sh["fill"]
                if sh["line"]:
                    kw["outline"] = sh["line"]
                    kw["width"] = max(1, int(sh["lw"] * SCALE / 72))
                d.rounded_rectangle([L, T, L + W, T + H],
                                    radius=max(0, rad), **kw)
                if sh["text"]:
                    runs = [dict(r, c=r["c"] or INK)
                            for r in sh["text"]["runs"]]
                    size = sh["text"].get("size", 12)
                    ml = sh["text"].get("ml", 0.05) * SCALE
                    mr = sh["text"].get("mr", 0.05) * SCALE
                    left_al = sh["text"].get("align") == "l"
                    max_w = W - ml - mr
                    lines = wrap_runs(runs, max_w, size, d)
                    lh = size * SCALE / 72 * 1.18 * 1.14
                    y = T + (H - lh * len(lines)) / 2 + lh * 0.05
                    for line in lines:
                        lw_px = sum(d.textlength(w, font=get_font(
                            r["f"], r["b"], r["i"], r["s"] or size))
                            for w, r in line)
                        x = (L + ml) if left_al else L + ml + (max_w
                                                               - lw_px) / 2
                        for word, r in line:
                            f = get_font(r["f"], r["b"], r["i"],
                                         r["s"] or size)
                            d.text((x, y), word, font=f, fill=r["c"])
                            x += d.textlength(word, font=f)
                        y += lh
            elif sh["kind"] == "text":
                max_w = sh["W"] * SCALE
                y = T
                for para in sh["paras"]:
                    runs = [dict(r, b=r["b"] or sh["bold"],
                                 i=r["i"] or sh["italic"],
                                 f=("mono" if sh["font"] == "mono"
                                    and r["f"] == "sans" else r["f"]),
                                 c=r["c"] or sh["color"]) for r in para]
                    lines = wrap_runs(runs, max_w, sh["size"], d)
                    lh = sh["size"] * SCALE / 72 * 1.18 * sh["spacing"]
                    for line in lines:
                        lw_px = sum(d.textlength(w, font=get_font(
                            r["f"], r["b"], r["i"], r["s"] or sh["size"]))
                            for w, r in line)
                        if sh["align"] == "c":
                            x = L + (max_w - lw_px) / 2
                        elif sh["align"] == "r":
                            x = L + max_w - lw_px
                        else:
                            x = L
                        for word, r in line:
                            f = get_font(r["f"], r["b"], r["i"],
                                         r["s"] or sh["size"])
                            d.text((x, y), word, font=f, fill=r["c"])
                            x += d.textlength(word, font=f)
                        y += lh
                    y += sh["para_gap"] * SCALE
            elif sh["kind"] == "img":
                if not os.path.exists(sh["path"]):
                    d.rounded_rectangle(
                        [L, T, L + sh["W"] * SCALE, T + sh["H"] * SCALE],
                        radius=10, outline=CORAL, width=3)
                    d.text((L + 20, T + 20),
                           f"MISSING {os.path.basename(sh['path'])}",
                           font=get_font("mono", True, False, 14),
                           fill=CORAL)
                    continue
                pic = Image.open(sh["path"]).convert("RGBA")
                iw, ih = pic.size
                box_w, box_h = sh["W"] * SCALE, sh["H"] * SCALE
                sc = min(box_w / iw, box_h / ih)
                nw, nh = int(iw * sc), int(ih * sc)
                pic = pic.resize((nw, nh), Image.LANCZOS)
                px = int(L + (box_w - nw) / 2)
                py = int(T + (box_h - nh) / 2)
                im.paste(pic, (px, py), pic)
                if sh["border"]:
                    d.rounded_rectangle([px, py, px + nw, py + nh],
                                        radius=8, outline=sh["border"],
                                        width=max(1, int(sh["lw"] * SCALE
                                                         / 72)))
        im.save(os.path.join(PREVIEW, f"slide_{i:02d}.png"))
    print(f"wrote {len(SLIDES)} previews to {PREVIEW}")


def check_no_emdash():
    bad = []
    for i, spec in enumerate(SLIDES):
        for sh in spec["shapes"]:
            if sh["kind"] == "text":
                for para in sh["paras"]:
                    for r in para:
                        if "—" in r["t"] or "–" in r["t"]:
                            bad.append((i, r["t"]))
            if sh["kind"] == "rect" and sh.get("text"):
                for r in sh["text"]["runs"]:
                    if "—" in r["t"] or "–" in r["t"]:
                        bad.append((i, r["t"]))
        if "—" in spec["notes"] or "–" in spec["notes"]:
            bad.append((i, "NOTES"))
    if bad:
        raise SystemExit(f"emdash/endash found: {bad}")
    print("dash check: clean")


if __name__ == "__main__":
    check_no_emdash()
    build_previews()
    build_pptx(OUT_PPTX)
