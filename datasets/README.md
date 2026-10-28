# Datasets

Git ignores all files in this folder except this README. OSM extracts and GTFS
feeds are large and have their own licenses. You must download them. The
procedures below make the files that the Quickstart and the examples use.

## Groningen routing extract (Quickstart)

This procedure downloads the Groningen province extract from Geofabrik. It
keeps only the objects that routing uses. You need
[osmium](https://osmcode.org/osmium-tool/).

1. Go to the `datasets` folder:

    ```bash
    cd datasets
    ```

2. Download the extract:

    ```bash
    curl -L -o groningen-latest.osm.pbf \
      https://download.geofabrik.de/europe/netherlands/groningen-latest.osm.pbf
    ```

3. Write the filter file:

    ```bash
    cat > netweevil-routing-filters.txt <<'EOF'
    w/highway
    w/route=ferry
    w/ferry
    r/type=restriction
    n/highway=traffic_signals
    EOF
    ```

4. Filter the extract:

    ```bash
    osmium tags-filter \
      --expressions=netweevil-routing-filters.txt \
      --remove-tags \
      groningen-latest.osm.pbf \
      -o groningen-260508-routing.osm.pbf \
      -O
    ```

If you do not have osmium, import the full extract
`groningen-latest.osm.pbf`. The import takes more time. The number `260508` in the file name is the extract date (YYMMDD). Use
the date of your download. The [datasets guide](../docs/guides/datasets.md)
tells which objects the filter keeps and why.

## Overture Maps transportation extract

Overture publishes its data as GeoParquet. Use the official Python CLI to get
the transportation segments in a bounding box.

1. Install the CLI:

    ```bash
    pip install overturemaps
    ```

2. Download the segments:

    ```bash
    overturemaps download \
      --bbox=6.4,53.1,6.7,53.3 \
      -f geoparquet --type=segment \
      -o datasets/overture-groningen-segments.parquet
    ```

3. Import the file:

    ```bash
    netweevil dataset import datasets/overture-groningen-segments.parquet \
      --name groningen_overture
    ```

The `.parquet` extension selects the Overture importer. A folder of parquet
files is also correct. For large extracts, you can also query the release
buckets with DuckDB or Athena. See the
[Overture documentation](https://docs.overturemaps.org/getting-data/).

## OpenOV GTFS feed (transit examples)

The transit examples use the Dutch national GTFS feed:

```bash
curl -L -o datasets/gtfs-openov-nl.zip https://gtfs.openov.nl/gtfs-rt/gtfs-openov-nl.zip
```

`transit import` accepts all GTFS zip files. The examples use the OpenOV feed
because it covers the Groningen street extract.

## Hong Kong full-data example

The [Hong Kong tutorial](../docs/guides/hong-kong-tutorial.md) uses these
sources:

- the OSM road extract,
- the outdoor and indoor 3D pedestrian GeoPackages,
- the numbered MTR platforms and exits,
- bus, tram, MTR, light rail and ferry transit.

To list the download links and the CSDI spatial export pages, run this command
from the repository root:

```bash
python3 scripts/download_hong_kong_sources.py --list
```

`scripts/setup_hong_kong.sh` uses the local spatial files. It writes the
prepared data, combined feeds, bindings and audits to `.netweevil/hong-kong/`.
Rail times come from community estimates. The tutorial lists the coverage
gaps.

## Licenses

- OpenStreetMap extracts are © OpenStreetMap contributors, available under the
  [ODbL](https://www.openstreetmap.org/copyright).
- Overture Maps data is available under
  [ODbL and CDLA-Permissive-2.0](https://overturemaps.org/download/), as
  applicable to each theme. The transportation theme uses the ODbL.
- Read the license of each GTFS feed that you download. The OpenOV feed is
  published for reuse at [gtfs.openov.nl](https://gtfs.openov.nl/).
