# Datasets

Everything in this directory except this file is git-ignored: OSM extracts
and GTFS feeds are large and license-encumbered, so you download them
yourself. The commands below produce the files the README quickstart and the
examples reference.

## Groningen routing extract (quickstart)

Download the Groningen province extract from Geofabrik and reduce it to the
routing-relevant objects with `osmium` (see "Preparing Routing-Only OSM
Extracts" in the repository README for what the filter keeps and why):

```bash
cd datasets

curl -L -o groningen-latest.osm.pbf \
  https://download.geofabrik.de/europe/netherlands/groningen-latest.osm.pbf

cat > netweevil-routing-filters.txt <<'EOF'
w/highway
w/route=ferry
w/ferry
r/type=restriction
n/highway=traffic_signals
EOF

osmium tags-filter \
  --expressions=netweevil-routing-filters.txt \
  --remove-tags \
  groningen-latest.osm.pbf \
  -o groningen-260508-routing.osm.pbf \
  -O
```

The full unfiltered extract also imports fine if you do not have `osmium`;
point `dataset import` at `groningen-latest.osm.pbf` instead. The
`260508`-style date suffix in the documented file names just records the
extract date — use whatever matches your download.

## Overture Maps transportation extract

Overture distributes its data as GeoParquet on S3/Azure. The easiest way to
get a bounding-box extract of the transportation segments is the official
Python CLI (`pip install overturemaps`):

```bash
overturemaps download \
  --bbox=6.4,53.1,6.7,53.3 \
  -f geoparquet --type=segment \
  -o datasets/overture-groningen-segments.parquet
```

Import it with `dataset import datasets/overture-groningen-segments.parquet
--name groningen_overture` (the parquet extension selects the Overture
importer; a directory of parquet files also works). DuckDB or Athena
queries against the release buckets are an alternative for larger
extracts — see the [Overture docs](https://docs.overturemaps.org/getting-data/).

## OpenOV GTFS feed (transit examples)

The Dutch national GTFS feed used by the transit examples:

```bash
curl -L -o datasets/gtfs-openov-nl.zip https://gtfs.openov.nl/gtfs-rt/gtfs-openov-nl.zip
```

Any other GTFS zip works with `transit import`; the examples assume the
OpenOV feed because it overlaps the Groningen street extract.

## Hong Kong full-data example

The [Hong Kong tutorial](../docs/hong-kong-tutorial.md) covers the OSM road
extract, full outdoor/indoor 3D pedestrian GeoPackages, separate numbered MTR
platforms and exits, and bus/tram/MTR/light-rail/ferry transit. Run
`python3 scripts/download_hong_kong_sources.py --list` from the repository root
for direct downloads and the official CSDI spatial-export pages.

`scripts/setup_hong_kong.sh` reuses local spatial files and writes preparation,
combined feeds, bindings and audits under `.netweevil/hong-kong/`. Rail timing
comes from community estimates, and the tutorial records coverage gaps.

## Licensing

OpenStreetMap extracts are © OpenStreetMap contributors, available under the
[ODbL](https://www.openstreetmap.org/copyright). Overture Maps data is
distributed under [ODbL and CDLA-Permissive-2.0](https://overturemaps.org/download/)
depending on theme; the transportation theme is ODbL. Check the license
terms of any GTFS feed you download; the OpenOV feed is published for reuse
at [gtfs.openov.nl](https://gtfs.openov.nl/).
