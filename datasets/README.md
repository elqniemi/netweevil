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

## OpenOV GTFS feed (transit examples)

The Dutch national GTFS feed used by the transit examples:

```bash
curl -L -o datasets/gtfs-openov-nl.zip https://gtfs.openov.nl/gtfs-rt/gtfs-openov-nl.zip
```

Any other GTFS zip works with `transit import`; the examples assume the
OpenOV feed because it overlaps the Groningen street extract.

## Licensing

OpenStreetMap extracts are © OpenStreetMap contributors, available under the
[ODbL](https://www.openstreetmap.org/copyright). Check the license terms of
any GTFS feed you download; the OpenOV feed is published for reuse at
[gtfs.openov.nl](https://gtfs.openov.nl/).
