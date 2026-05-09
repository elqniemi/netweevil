#!/usr/bin/env python3

import json
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "examples" / "api" / "groningen_routes_every_10th_from_od.json"
OUT_DIR = ROOT / ".netweevil" / "runs" / "groningen_routes_every_10th_gpkg"
DATASET = "groningen_2026_03"
PROFILE = ROOT / "examples" / "profiles" / "car_research_v3.yml"


def main() -> None:
    payload = json.loads(MANIFEST.read_text())
    requests = payload["requests"]
    OUT_DIR.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="netweevil-groningen-routes-") as tmp_dir:
        tmp_dir_path = Path(tmp_dir)
        for index, entry in enumerate(requests, start=1):
            request = entry["request"]
            route_id = request["route_id"]
            request_path = tmp_dir_path / f"{route_id}.json"
            out_path = OUT_DIR / f"{index:03d}_{route_id}.gpkg"
            request_path.write_text(json.dumps(request, indent=2) + "\n")
            subprocess.run(
                [
                    "cargo",
                    "run",
                    "--release",
                    "-p",
                    "netweevil-cli",
                    "--",
                    "analyze",
                    "route",
                    "--dataset",
                    DATASET,
                    "--profile",
                    str(PROFILE),
                    "--request",
                    str(request_path),
                    "--out",
                    str(out_path),
                ],
                cwd=ROOT,
                check=True,
            )
            print(f"[{index:03d}/{len(requests)}] wrote {out_path}")


if __name__ == "__main__":
    main()
