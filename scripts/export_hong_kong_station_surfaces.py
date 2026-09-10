#!/usr/bin/env python3
"""Export LandsD platform polygons in WGS84 longitude/latitude + HKPD metres.

Uses actual polygons from the audited indoor-map join, including explicitly
labelled whole-floor footprints where LandsD supplied no platform unit. Numbered
CSDI platform points label a surface only when they fall inside its polygon and
match its surveyed height. Geometry is never generated from boarding points.
"""
from __future__ import annotations
import argparse
from collections import Counter, defaultdict
import hashlib
import json
import math
from pathlib import Path


def positions(geometry):
    if geometry['type'] == 'Polygon':
        polygons = [geometry['coordinates']]
    elif geometry['type'] == 'MultiPolygon':
        polygons = geometry['coordinates']
    else:
        raise ValueError(f"expected polygon surface, got {geometry['type']}")
    for polygon in polygons:
        if not polygon:
            raise ValueError('empty polygon')
        for ring in polygon:
            if len(ring) < 4 or ring[0] != ring[-1]:
                raise ValueError('polygon ring is short or not closed')
            for point in ring:
                if len(point) != 3 or not all(math.isfinite(v) for v in point):
                    raise ValueError('every surface vertex must have finite XYZ coordinates')
                if not (113 <= point[0] <= 115 and 22 <= point[1] <= 23):
                    raise ValueError('expected Hong Kong longitude/latitude, not projected XY')
                yield point


def on_segment(x, y, a, b):
    dx, dy = b[0] - a[0], b[1] - a[1]
    length = math.hypot(dx, dy)
    if length == 0:
        return math.hypot(x - a[0], y - a[1]) <= 1e-9
    return (abs((x - a[0]) * dy - (y - a[1]) * dx) <= 1e-9 * length
            and min(a[0], b[0]) - 1e-9 <= x <= max(a[0], b[0]) + 1e-9
            and min(a[1], b[1]) - 1e-9 <= y <= max(a[1], b[1]) + 1e-9)


def in_ring(x, y, ring):
    inside = False
    for a, b in zip(ring, ring[1:]):
        if on_segment(x, y, a, b):
            return True
        if (a[1] > y) != (b[1] > y):
            crossing = a[0] + (y - a[1]) * (b[0] - a[0]) / (b[1] - a[1])
            if x < crossing:
                inside = not inside
    return inside


def contains(geometry, x, y):
    polygons = [geometry['coordinates']] if geometry['type'] == 'Polygon' else geometry['coordinates']
    return any(in_ring(x, y, rings[0]) and not any(in_ring(x, y, hole) for hole in rings[1:])
               for rings in polygons)


def build_surfaces(joined, catalog, z_tolerance=.5):
    grouped = defaultdict(list)
    for feature in joined['features']:
        p = feature['properties']
        grouped[(p['venue_id'], p['platform_source_id'])].append(feature)
    features = []
    by_venue = defaultdict(list)
    for (venue, source_id), rows in sorted(grouped.items()):
        first = rows[0]
        if any(row['geometry'] != first['geometry'] for row in rows):
            raise ValueError(f'conflicting geometry for source polygon {source_id}')
        p = first['properties']
        vertices = list(positions(first['geometry']))
        lo = [min(pt[d] for pt in vertices) for d in range(3)]
        hi = [max(pt[d] for pt in vertices) for d in range(3)]
        source_kind = p['platform_source_kind']
        props = {
            'kind': 'platform_surface',
            'station_code': p['station_code'], 'station_name_en': p['station_name_en'],
            'station_name_zh': p.get('station_name_zh'), 'venue_id': venue,
            'source_id': source_id, 'source_kind': source_kind,
            'surface_kind': 'platform' if source_kind == 'unit' else 'platform_floor',
            'level_id': p['level_id'], 'level_name': p['level_name_en'],
            'level_short_name': p.get('level_short_name', ''),
            'line_codes': sorted({r['properties']['line_code'] for r in rows}),
            'platform_numbers': [], 'numbered_platforms': [],
            'number_assignment_status': 'unassigned',
            'z_min_m': lo[2], 'z_max_m': hi[2],
            'source_binding_z_m': sorted({r['properties']['binding_z'] for r in rows}),
            'joined_stop_ids': sorted({r['properties']['stop_id'] for r in rows}),
            'source_geometry_preserved': True,
            'horizontal_crs': 'OGC:CRS84', 'vertical_crs': 'EPSG:5738',
            'geometry_source': 'Lands Department 3D Indoor Map',
            'number_source': 'Wheels CSDI-derived MTR platform catalogue',
        }
        feature = {'type': 'Feature', 'id': source_id, 'bbox': lo + hi,
                   'properties': props, 'geometry': first['geometry']}
        features.append(feature)
        by_venue[venue].append(feature)
    unmatched, ambiguous = [], []
    for point in catalog['platforms'].values():
        c = point['coordinates']
        candidates = []
        for feature in by_venue[point['venue_id']]:
            props = feature['properties']
            # Nonplanar source polygons cannot establish an exact local floor
            # height without interpolation. Preserve them but do not guess labels.
            if props['z_max_m'] - props['z_min_m'] > .01:
                continue
            if abs(c['z'] - props['z_min_m']) <= z_tolerance and contains(feature['geometry'], c['lon'], c['lat']):
                candidates.append(feature)
        units = [f for f in candidates if f['properties']['source_kind'] == 'unit']
        candidates = units or candidates
        if len(candidates) != 1:
            item = {'amenity_id': point['amenity_id'], 'venue_id': point['venue_id'],
                    'platform_number': point['ref'], 'candidate_source_ids': [f['id'] for f in candidates]}
            (ambiguous if candidates else unmatched).append(item)
            continue
        props = candidates[0]['properties']
        stop_id = f"rail:MTR-PLATFORM-{props['station_code']}-{point['ref']}"
        props['numbered_platforms'].append({
            'ref': point['ref'], 'stop_id': stop_id, 'amenity_id': point['amenity_id'],
            'name_en': point.get('name_en', ''), 'directions': point.get('lines_and_directions', []),
            'match_method': 'point_inside_polygon_at_same_height',
            'point_z_m': c['z'], 'height_difference_m': abs(c['z'] - props['z_min_m']),
        })
        props['platform_numbers'].append(point['ref'])
        props['number_assignment_status'] = 'verified_point_on_surface'
    for f in features:
        p = f['properties']
        p['platform_numbers'] = sorted(set(p['platform_numbers']), key=lambda v: (not v.isdigit(), int(v) if v.isdigit() else v))
        p['numbered_platforms'].sort(key=lambda v: v['ref'])
    report = {
        'schema_version': 1, 'input_join_features': len(joined['features']),
        'surface_count': len(features), 'station_count': len({f['properties']['station_code'] for f in features}),
        'surface_kinds': dict(Counter(f['properties']['surface_kind'] for f in features)),
        'numbered_point_count': len(catalog['platforms']),
        'matched_numbered_points': sum(len(f['properties']['numbered_platforms']) for f in features),
        'surfaces_with_verified_numbers': sum(bool(f['properties']['platform_numbers']) for f in features),
        'unmatched_numbered_points': unmatched, 'ambiguous_numbered_points': ambiguous,
        'number_match_z_tolerance_m': z_tolerance,
        'vertices': sum(len(list(positions(f['geometry']))) for f in features),
        'geometry_transform': 'none; source longitude/latitude and HKPD Z preserved exactly',
        'limitations': [
            'Platform-floor footprints cover a whole mapped level, not necessarily the platform edge.',
            'Some mapped polygons contain multiple numbered platforms; geometry does not separate their boarding sides.',
            'Unmatched or ambiguous numbered points do not label polygons.',
            'Legacy platform-join binding Z may differ from polygon Z; polygons retain their original source height.',
        ],
    }
    return {'type': 'FeatureCollection', 'features': features, 'metadata': {
        'name': 'Hong Kong MTR platform surfaces', 'horizontal_crs': 'OGC:CRS84',
        'vertical_crs': 'EPSG:5738', 'surface_count': len(features),
        'station_count': report['station_count'], 'surface_kinds': report['surface_kinds'],
        'matched_numbered_points': report['matched_numbered_points'],
        'attribution': 'Lands Department, HKSAR Government; Wheels CSDI platform catalogue',
        'source_pages': ['https://data.gov.hk/en-data/dataset/hk-landsd-openmap-3d-indoor-map',
                         'https://github.com/wheelstransit/mtr-platform-exits-crawler'],
        'limitations': report['limitations'],
    }}, report


def sha256(path):
    with path.open('rb') as handle:
        return hashlib.file_digest(handle, 'sha256').hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--platform-join-dir', type=Path, required=True)
    parser.add_argument('--platform-catalog', type=Path, required=True)
    parser.add_argument('--output', type=Path, default=Path('.netweevil/hong-kong/transit/mtr_platform_surfaces.geojson'))
    parser.add_argument('--gtfs', type=Path, help='Also write the API stations sidecar beside this GTFS ZIP')
    args = parser.parse_args()
    source = args.platform_join_dir / 'mtr_platforms_joined.geojson'
    surfaces, report = build_surfaces(json.loads(source.read_text()), json.loads(args.platform_catalog.read_text()))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    temporary = args.output.with_suffix('.geojson.part')
    temporary.write_text(json.dumps(surfaces, ensure_ascii=False, separators=(',', ':')) + '\n')
    temporary.replace(args.output)
    gtfs = args.gtfs or args.output.parent / 'hong_kong_multimodal.gtfs.zip'
    sidecar = gtfs.with_suffix('.stations.geojson')
    sidecar.parent.mkdir(parents=True, exist_ok=True)
    sidecar_temp = sidecar.with_suffix('.geojson.part')
    sidecar_temp.write_bytes(args.output.read_bytes())
    sidecar_temp.replace(sidecar)
    report['stations_sidecar'] = str(sidecar.resolve())
    report['sources'] = [{'path': str(p.resolve()), 'sha256': sha256(p)} for p in (source, args.platform_catalog)]
    report['output_sha256'] = sha256(args.output)
    report['output_bytes'] = args.output.stat().st_size
    report_path = args.output.with_suffix('.manifest.json')
    report_path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({k: v for k, v in report.items() if k not in ('unmatched_numbered_points', 'ambiguous_numbered_points')}, indent=2))
    print(f'Unmatched points: {len(report["unmatched_numbered_points"])}; ambiguous: {len(report["ambiguous_numbered_points"])}')
    print(f'Surfaces: {args.output}\nManifest: {report_path}')


if __name__ == '__main__':
    main()
