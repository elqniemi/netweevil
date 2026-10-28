#!/usr/bin/env python3
"""Combine Transport Department surface GTFS with Wheels rail and 3D platforms.

Source calendars, stop times, frequency windows and shapes are retained. Wheels
rail times are procedural estimates, not an official MTR working timetable.
"""
from __future__ import annotations

import argparse
from collections import Counter, defaultdict
import csv
import hashlib
import io
import json
import math
from pathlib import Path
import re
import zipfile

TABLES = ('agency', 'stops', 'routes', 'trips', 'stop_times', 'calendar',
          'calendar_dates', 'frequencies', 'shapes', 'transfers', 'levels', 'pathways')
ID_FIELDS = {'agency_id', 'stop_id', 'parent_station', 'route_id', 'trip_id',
             'service_id', 'shape_id', 'level_id', 'pathway_id', 'block_id', 'zone_id',
             'from_stop_id', 'to_stop_id', 'from_route_id', 'to_route_id',
             'from_trip_id', 'to_trip_id'}


def read_feed(path):
    with zipfile.ZipFile(path) as archive:
        return {table: list(csv.DictReader(io.TextIOWrapper(archive.open(table + '.txt'),
                 encoding='utf-8-sig'))) for table in TABLES if table + '.txt' in archive.namelist()}


def digest(path):
    with open(path, 'rb') as handle:
        return hashlib.file_digest(handle, 'sha256').hexdigest()


def rail_subset(feed):
    """Keep heavy/light rail and their referenced data, including station exits."""
    routes = {r['route_id'] for r in feed['routes'] if r.get('agency_id') in ('MTRR', 'LR')}
    trips = {r['trip_id'] for r in feed['trips'] if r['route_id'] in routes}
    services = {r['service_id'] for r in feed['trips'] if r['trip_id'] in trips}
    shapes = {r.get('shape_id') for r in feed['trips'] if r['trip_id'] in trips}
    agencies = {r['agency_id'] for r in feed['routes'] if r['route_id'] in routes}
    stops = {r['stop_id'] for r in feed['stop_times'] if r['trip_id'] in trips}
    stops |= {r['stop_id'] for r in feed['stops'] if r['stop_id'].startswith(('MTR-', 'LR'))}
    levels = {r.get('level_id') for r in feed['stops'] if r['stop_id'] in stops}
    predicates = {
        'routes': lambda r: r['route_id'] in routes,
        'trips': lambda r: r['trip_id'] in trips,
        'stop_times': lambda r: r['trip_id'] in trips,
        'frequencies': lambda r: r['trip_id'] in trips,
        'calendar': lambda r: r['service_id'] in services,
        'calendar_dates': lambda r: r['service_id'] in services,
        'shapes': lambda r: r['shape_id'] in shapes,
        'agency': lambda r: r['agency_id'] in agencies,
        'stops': lambda r: r['stop_id'] in stops,
        'levels': lambda r: r['level_id'] in levels,
        'transfers': lambda r: r.get('from_stop_id') in stops and r.get('to_stop_id') in stops
            and (not r.get('from_route_id') or r['from_route_id'] in routes)
            and (not r.get('to_route_id') or r['to_route_id'] in routes)
            and (not r.get('from_trip_id') or r['from_trip_id'] in trips)
            and (not r.get('to_trip_id') or r['to_trip_id'] in trips),
        'pathways': lambda r: r['from_stop_id'] in stops and r['to_stop_id'] in stops,
    }
    return {table: [r.copy() for r in rows if predicates[table](r)] for table, rows in feed.items()}


def namespace(feed, prefix):
    return {table: [{key: prefix + value if key in ID_FIELDS and value else value
                    for key, value in row.items()} for row in rows] for table, rows in feed.items()}


def distance_m(a, b):
    return math.hypot((a[0] - b[0]) * 111320 * math.cos(math.radians(a[1])),
                      (a[1] - b[1]) * 111320)


def platform_bindings(rail, join_rows, catalog, feed_id):
    venue_station = {r['venue_id']: r['station_code'] for r in join_rows}
    by_station = defaultdict(list)
    for row in join_rows:
        by_station[row['station_code']].append(row)
    catalog_platforms = {}
    for p in catalog['platforms'].values():
        station = venue_station.get(p['venue_id'])
        if station:
            catalog_platforms[f"MTR-PLATFORM-{station}-{p['ref']}"] = p
    used = {r['stop_id'] for r in rail['stop_times'] if r['trip_id'].startswith('MTR-')}
    trips = {r['trip_id']: r for r in rail['trips']}
    stop_lines = defaultdict(set)
    for row in rail['stop_times']:
        stop_lines[row['stop_id']].add(trips[row['trip_id']]['route_id'].removeprefix('MTR-'))
    bindings, audit, features = [], [], []
    for stop in rail['stops']:
        stop_id = stop['stop_id']
        if stop_id not in used:
            continue
        match = re.fullmatch(r'MTR-PLATFORM-([A-Z]+)-(.+)', stop_id)
        if not match:
            raise ValueError(f'unsupported MTR boarding stop ID: {stop_id}')
        station, platform = match.groups()
        candidates = [r for r in by_station[station] if r['line_code'] in stop_lines[stop_id]]
        if not candidates:
            raise ValueError(f'no LandsD platform floor for {stop_id}')
        p = catalog_platforms.get(stop_id)
        if p:
            c = p['coordinates']
            coordinates = [float(c['lon']), float(c['lat']), float(c['z'])]
            method = 'csdi_numbered_platform'
            source_id = p['amenity_id']
        else:
            # Two known catalog gaps (AWE and POA) have LandsD platform polygons.
            # Select within the correct line and station, never a nearby station.
            candidates.sort(key=lambda r: (distance_m([float(stop['stop_lon']), float(stop['stop_lat'])],
                         [float(r['binding_lon']), float(r['binding_lat'])]), r['stop_id']))
            r = candidates[0]
            coordinates = [float(r['binding_lon']), float(r['binding_lat']), float(r['binding_z'])]
            method = 'landsd_platform_polygon_fallback'
            source_id = r['platform_source_id']
        source_distance = distance_m(coordinates, [float(stop['stop_lon']), float(stop['stop_lat'])])
        if source_distance > 250:
            raise ValueError(f'{stop_id} differs from its GTFS location by {source_distance:.1f} m')
        stop['stop_lon'], stop['stop_lat'] = str(coordinates[0]), str(coordinates[1])
        stop['platform_code'] = platform
        coordinate = dict(zip(('lon', 'lat', 'z'), coordinates))
        # Retain adjacent same-level candidates despite survey/rounding noise.
        coordinate['z_window_m'] = 0.5
        coordinate['attribute_filter'] = {'mtr_station_code': station, 'mtr_platform_level': '1'}
        bindings.append({'stop_id': 'rail:' + stop_id, 'coordinate': coordinate})
        properties = {'stop_id': 'rail:' + stop_id, 'station_code': station,
                      'platform_code': platform, 'method': method, 'source_id': source_id,
                      'gtfs_coordinate_shift_m': source_distance,
                      'lines': sorted(stop_lines[stop_id]),
                      'directions': p.get('lines_and_directions', []) if p else [],
                      'direction_verified': bool(p and p.get('lines_and_directions'))}
        audit.append(properties)
        features.append({'type': 'Feature', 'properties': properties,
                         'geometry': {'type': 'Point', 'coordinates': coordinates}})
    if len(bindings) != len(used):
        raise ValueError(f'bound {len(bindings)} of {len(used)} used MTR platform stops')
    return {'schema_version': 1, 'feed_id': feed_id, 'bindings': bindings}, audit, features


def validate(feed):
    for table, key in [('stops', 'stop_id'), ('trips', 'trip_id'), ('routes', 'route_id')]:
        values = [r[key] for r in feed[table]]
        if len(values) != len(set(values)):
            raise ValueError(f'duplicate {key}')
    stops = {r['stop_id'] for r in feed['stops']}
    trips = {r['trip_id'] for r in feed['trips']}
    routes = {r['route_id'] for r in feed['routes']}
    services = {r['service_id'] for t in ('calendar', 'calendar_dates') for r in feed.get(t, [])}
    for r in feed['trips']:
        if r['route_id'] not in routes or r['service_id'] not in services:
            raise ValueError(f'trip has missing route/calendar: {r}')
    for r in feed['stop_times']:
        if r['stop_id'] not in stops or r['trip_id'] not in trips:
            raise ValueError(f'stop time has missing stop/trip: {r}')
    for r in feed['stops']:
        if r.get('parent_station') and r['parent_station'] not in stops:
            raise ValueError(f'missing parent station: {r}')
    for r in feed.get('frequencies', []):
        if r['trip_id'] not in trips or int(r['headway_secs']) <= 0:
            raise ValueError(f'invalid frequency: {r}')


def write_zip(path, feed):
    temporary = path.with_suffix('.zip.part')
    with zipfile.ZipFile(temporary, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
        for table, rows in sorted(feed.items()):
            if not rows:
                continue
            columns = list(dict.fromkeys(k for r in rows for k in r))
            buffer = io.StringIO(newline='')
            writer = csv.DictWriter(buffer, fieldnames=columns, lineterminator='\n')
            writer.writeheader()
            writer.writerows(rows)
            info = zipfile.ZipInfo(table + '.txt', date_time=(2026, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, buffer.getvalue())
    temporary.replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--surface-gtfs', type=Path, required=True)
    parser.add_argument('--rail-gtfs', type=Path, required=True)
    parser.add_argument('--platform-join-dir', type=Path, required=True)
    parser.add_argument('--platform-catalog', type=Path, required=True)
    parser.add_argument('--output-dir', type=Path, required=True)
    parser.add_argument('--feed-id', default='hong_kong_multimodal')
    args = parser.parse_args()
    surface = read_feed(args.surface_gtfs)
    rail = rail_subset(read_feed(args.rail_gtfs))
    with (args.platform_join_dir / 'mtr_platform_join.csv').open(encoding='utf-8-sig') as handle:
        rows = list(csv.DictReader(handle))
    catalog = json.loads(args.platform_catalog.read_text())
    bindings, audit, features = platform_bindings(rail, rows, catalog, args.feed_id)
    feeds = [namespace(surface, 'td:'), namespace(rail, 'rail:')]
    merged = {table: [row for feed in feeds for row in feed.get(table, [])] for table in TABLES}
    validate(merged)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    output = args.output_dir / 'hong_kong_multimodal.gtfs.zip'
    write_zip(output, merged)
    def write_json(name, value):
        (args.output_dir / name).write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
    write_json('stop_bindings.json', bindings)
    write_json('mtr_platforms.geojson', {'type': 'FeatureCollection', 'features': features})
    venue_station = {r['venue_id']: r['station_code'] for r in rows}
    catalog_platforms = [{'type': 'Feature',
        'properties': {k: v for k, v in p.items() if k != 'coordinates'} |
            {'station_code': venue_station.get(p['venue_id'])},
        'geometry': {'type': 'Point', 'coordinates': [p['coordinates'][k] for k in ('lon', 'lat', 'z')]}}
        for p in catalog['platforms'].values()]
    write_json('mtr_catalog_platforms.geojson', {'type': 'FeatureCollection', 'features': catalog_platforms})
    exits = [{'type': 'Feature', 'properties': {k: v for k, v in e.items() if k != 'coordinates'} |
              {'station_code': venue_station.get(e['venue_id'])},
              'geometry': {'type': 'Point', 'coordinates': [e['coordinates'][k] for k in ('lon', 'lat', 'z')]}}
             for e in catalog['exits'].values()]
    write_json('mtr_exits.geojson', {'type': 'FeatureCollection', 'features': exits})
    manifest = {
        'schema_version': 1, 'feed_id': args.feed_id,
        'sources': [{'path': str(p.resolve()), 'sha256': digest(p)} for p in
                    (args.surface_gtfs, args.rail_gtfs, args.platform_catalog,
                     args.platform_join_dir / 'mtr_platform_join.csv')],
        'output_sha256': digest(output), 'table_counts': {t: len(r) for t, r in merged.items()},
        'route_types': dict(Counter(r['route_type'] for r in merged['routes'])),
        'platform_binding_count': len(bindings['bindings']), 'exit_count': len(exits),
        'catalog_platform_count': len(catalog_platforms),
        'stations_without_rail_service': sorted({r['station_code'] for r in rows} -
                                              {r['station_code'] for r in audit}),
        'binding_methods': dict(Counter(r['method'] for r in audit)),
        'rail_timing': 'Wheels procedural estimates; source calendars, times and headways preserved',
        'official_mtr_timetable': False,
        'surface_timing': 'Transport Department aggregate headway feed; source times preserved',
        'source_pages': ['https://static.data.gov.hk/td/pt-headway-en/gtfs.zip',
                         'https://github.com/wheelstransit/hongkong-community-gtfs',
                         'https://github.com/wheelstransit/mtr-platform-exits-crawler'],
        'limitations': ['MTR/LR times are estimates, not an official timetable.',
                       'Fallback platform polygons do not prove the numbered platform or direction.',
                       'Platform coordinates require graph binding and exit connectivity audit.',
                       'Racecourse indoor geometry is included; this rail source has no race-day service.'],
        'platforms': audit,
    }
    write_json('transit_manifest.json', manifest)
    print(json.dumps({k: v for k, v in manifest.items() if k != 'platforms'}, indent=2))


if __name__ == '__main__':
    main()
