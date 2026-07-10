"""Replace the figure placeholders in netweevil_pitch.pptx with rendered maps.

Each target slide has a right-hand figure panel: a big rounded rectangle, a
'QGIS MAP PLACEHOLDER' label, a 'FIG nA' tag and a descriptive caption. The
rendered PNG already bakes in its own title / legend / fig tag, so we remove the
placeholder shapes inside the panel region and drop the image into the same
bounds.
"""
from pptx import Presentation
from pptx.util import Emu
import copy

SRC = "netweevil_pitch.pptx"
OUT = "netweevil_pitch.pptx"

# slide index -> image path
FIGS = {
    6:  "pitch_assets/maps/fig_service_area.png",
    7:  "pitch_assets/maps/fig_restrictions.png",
    8:  "pitch_assets/maps/fig_transit.png",
    9:  "pitch_assets/maps/fig_simulation.png",
    11: "pitch_assets/maps/fig_layers.png",
}

EMU_IN = 914400


def in_panel(sh):
    """True if the shape sits inside the right-hand figure panel region."""
    try:
        l = Emu(sh.left).inches; t = Emu(sh.top).inches
        w = Emu(sh.width).inches; h = Emu(sh.height).inches
    except Exception:
        return False
    cx = l + w / 2; cy = t + h / 2
    return l >= 7.85 and 1.7 <= cy <= 6.4


def panel_bounds(slide):
    """Find the big rounded rectangle that defines the figure frame."""
    best = None
    for sh in slide.shapes:
        try:
            w = Emu(sh.width).inches; h = Emu(sh.height).inches
            l = Emu(sh.left).inches
        except Exception:
            continue
        if l >= 7.85 and w >= 4.0 and h >= 3.8:
            area = w * h
            if best is None or area > best[0]:
                best = (area, sh.left, sh.top, sh.width, sh.height)
    return best


def main():
    prs = Presentation(SRC)
    slides = list(prs.slides)
    for idx, img in FIGS.items():
        slide = slides[idx]
        pb = panel_bounds(slide)
        if pb is None:
            print(f"slide {idx}: NO panel found, skipping")
            continue
        _, L, T, Wd, Ht = pb
        # remove every placeholder shape inside the panel region
        to_remove = [sh for sh in slide.shapes if in_panel(sh)]
        for sh in to_remove:
            sh._element.getparent().remove(sh._element)
        pic = slide.shapes.add_picture(img, L, T, Wd, Ht)
        # send picture just above the slide background but it's fine on top
        print(f"slide {idx}: removed {len(to_remove)} shapes, placed {img} "
              f"at ({Emu(L).inches:.2f},{Emu(T).inches:.2f}) "
              f"{Emu(Wd).inches:.2f}x{Emu(Ht).inches:.2f}")
    prs.save(OUT)
    print("saved", OUT)


if __name__ == "__main__":
    main()
