#!/usr/bin/env python3
"""Download public Hong Kong road, transit and numbered-platform snapshots.

Full LandsD pedestrian/indoor networks and the indoor map index are separate
CSDI exports. --list also prints their official download pages. Existing local
GeoPackages can be used directly by prepare_hong_kong_pedestrian.sh.
"""
from __future__ import annotations
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import shutil
import time
import urllib.request
import zipfile

SOURCES = {
    'roads': ('hong-kong-latest.osm.pbf', 'https://download.geofabrik.de/asia/china/hong-kong-latest.osm.pbf', 'OpenStreetMap contributors / Geofabrik; ODbL'),
    'surface-gtfs': ('hk-surface.gtfs.zip', 'https://static.data.gov.hk/td/pt-headway-en/gtfs.zip', 'HKSAR Transport Department; aggregate headways'),
    'rail-gtfs': ('hk-community.gtfs.zip', 'https://feed.justusewheels.com/hk.gtfs.zip', 'Wheels Hong Kong Community GTFS; ODbL; procedural timing estimates'),
    'platforms': ('mtr_data_complete.json', 'https://raw.githubusercontent.com/wheelstransit/mtr-platform-exits-crawler/main/data/output/mtr_data_complete.json', 'Wheels CSDI-derived numbered platforms and exits; see source terms'),
    'mtr-lines': ('mtr_lines_and_stations.csv', 'https://opendata.mtr.com.hk/data/mtr_lines_and_stations.csv', 'MTR Corporation open data'),
}
SPATIAL_PAGES = {
    '3D_Pedestrian_Network.gpkg': 'https://data.gov.hk/en-data/dataset/hk-landsd-openmap-3d-pedestrian-network',
    '3D_Indoor_Network.gpkg': 'https://data.gov.hk/en-data/dataset/hk-landsd-openmap-3d-indoor-network',
    'indoor-map-index': 'https://data.gov.hk/en-data/dataset/hk-landsd-openmap-3d-indoor-map',
}


def validate(path, key):
    if path.stat().st_size == 0:
        raise ValueError(f'empty download: {path}')
    if key.endswith('gtfs'):
        with zipfile.ZipFile(path) as archive:
            required = {'stops.txt', 'routes.txt', 'trips.txt', 'stop_times.txt', 'agency.txt'}
            if not required <= set(archive.namelist()) or archive.testzip():
                raise ValueError(f'invalid GTFS ZIP: {path}')
    elif key == 'platforms':
        data = json.loads(path.read_text())
        if not all(data.get(k) for k in ('platforms', 'stations', 'exits')):
            raise ValueError(f'incomplete platform catalog: {path}')
    elif key == 'roads':
        with path.open('rb') as handle:
            if b'OSMHeader' not in handle.read(64):
                raise ValueError(f'download is not an OSM PBF: {path}')
    elif key == 'mtr-lines' and 'Station Code' not in path.read_text(encoding='utf-8-sig').splitlines()[0]:
        raise ValueError(f'invalid MTR station CSV: {path}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output-dir', type=Path, default=Path('.netweevil/hong-kong/raw'))
    parser.add_argument('--only', nargs='+', choices=tuple(SOURCES), default=list(SOURCES))
    parser.add_argument('--refresh', action='store_true', help='Replace cached snapshots after successful validation')
    parser.add_argument('--list', action='store_true', help='Print URLs without downloading')
    args = parser.parse_args()
    if args.list:
        print(json.dumps({'downloads': SOURCES, 'spatial_exports': SPATIAL_PAGES}, indent=2))
        return
    args.output_dir.mkdir(parents=True, exist_ok=True)
    manifest_path = args.output_dir / 'download_manifest.json'
    manifest = json.loads(manifest_path.read_text()) if manifest_path.exists() else {'schema_version': 1, 'sources': {}}
    for key in args.only:
        filename, url, attribution = SOURCES[key]
        target = args.output_dir / filename
        headers = {}
        downloaded = args.refresh or not target.exists()
        if downloaded:
            temporary = target.with_suffix(target.suffix + '.part')
            for attempt in range(3):
                try:
                    request = urllib.request.Request(url, headers={'User-Agent': 'NetWeevil-HK/1.0'})
                    with urllib.request.urlopen(request, timeout=120) as response, temporary.open('wb') as output:
                        headers = {name: response.headers[name] for name in ('Last-Modified', 'ETag') if name in response.headers}
                        shutil.copyfileobj(response, output, length=1024 * 1024)
                    validate(temporary, key)
                    temporary.replace(target)
                    break
                except Exception:
                    temporary.unlink(missing_ok=True)
                    if attempt == 2:
                        raise
                    time.sleep(2 ** attempt)
        validate(target, key)
        with target.open('rb') as handle:
            checksum = hashlib.file_digest(handle, 'sha256').hexdigest()
        previous = manifest['sources'].get(key, {})
        manifest['sources'][key] = {
            'path': str(target.resolve()), 'url': url, 'sha256': checksum,
            'bytes': target.stat().st_size, 'attribution': attribution,
            'downloaded_at': datetime.now(timezone.utc).isoformat() if downloaded else previous.get('downloaded_at'),
            'headers': headers if downloaded else previous.get('headers', {}),
        }
        manifest['spatial_export_pages'] = SPATIAL_PAGES
        temporary_manifest = manifest_path.with_suffix('.json.part')
        temporary_manifest.write_text(json.dumps(manifest, indent=2) + '\n')
        temporary_manifest.replace(manifest_path)
        print(f'{"Downloaded" if downloaded else "Verified cached"}: {target} ({target.stat().st_size} bytes)', flush=True)
    print(f'Manifest: {manifest_path}')


if __name__ == '__main__':
    main()
