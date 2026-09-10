#!/usr/bin/env python3
"""Audit exact-XYZ indoor/outdoor connectivity and surveyed MTR exits.

Uses only Python's standard library. Coordinates remain lon/lat + HKPD metres.
Weak connectivity is a source-topology check, not a guarantee of directed,
accessible or time-dependent routing. No geometry is moved or fabricated.
"""
from __future__ import annotations

import argparse
from collections import Counter, defaultdict
import json
import math
from pathlib import Path
import sqlite3
import struct


def lines(blob):
    if blob[:2] != b'GP':
        raise ValueError('Not a GeoPackage geometry')
    offset = 8 + {0: 0, 1: 32, 2: 48, 3: 48, 4: 64}[(blob[3] >> 1) & 7]

    def geometry():
        nonlocal offset
        endian = '<' if blob[offset] == 1 else '>'
        kind = struct.unpack_from(endian + 'I', blob, offset + 1)[0]
        offset += 5
        srid = bool(kind & 0x20000000)
        z, m = bool(kind & 0x80000000), bool(kind & 0x40000000)
        kind &= 0x0fffffff
        dim, kind = divmod(kind, 1000)
        z, m = z or dim in (1, 3), m or dim in (2, 3)
        if srid:
            offset += 4
        count = struct.unpack_from(endian + 'I', blob, offset)[0]
        offset += 4
        if kind == 5:
            return [line for _ in range(count) for line in geometry()]
        if kind != 2 or not z:
            raise ValueError('Expected 3D LineString/MultiLineString')
        width = 3 + int(m)
        points = [struct.unpack_from(endian + 'd' * width, blob, offset + i * width * 8)[:3]
                  for i in range(count)]
        offset += count * width * 8
        return [points]

    return geometry()


def coordinate_key(point):
    # Match Rust f64::round: halfway values round away from zero.
    def rounded(value):
        return math.floor(value + .5) if value >= 0 else math.ceil(value - .5)
    return tuple(rounded(value / epsilon) for value, epsilon in zip(point, (1e-9, 1e-9, .01)))


class Network:
    def __init__(self):
        self.ids = {}
        self.parent = []
        self.size = []
        self.points = []
        self.outdoor = set()
        self.indoor = set()
        self.stations = defaultdict(set)
        self.platforms = defaultdict(set)
        self.station_segments = defaultdict(list)
        self.features = Counter()

    def node(self, point):
        key = coordinate_key(point)
        if key not in self.ids:
            self.ids[key] = len(self.parent)
            self.parent.append(len(self.parent))
            self.size.append(1)
            self.points.append(point)
        return self.ids[key]

    def root(self, node):
        while node != self.parent[node]:
            self.parent[node] = self.parent[self.parent[node]]
            node = self.parent[node]
        return node

    def union(self, a, b):
        a, b = self.root(a), self.root(b)
        if a == b:
            return
        if self.size[a] < self.size[b]:
            a, b = b, a
        self.parent[b] = a
        self.size[a] += self.size[b]

    def read(self, path, table, indoor=False):
        with sqlite3.connect(f'file:{path}?mode=ro', uri=True) as db:
            fields = 'Shape, MTRStationCode, MTRPlatformLevel' if indoor else 'Shape, NULL, NULL'
            for blob, station, platform in db.execute(f'SELECT {fields} FROM {table}'):
                self.features[table] += 1
                for line in lines(blob):
                    nodes = [self.node(p) for p in line]
                    (self.indoor if indoor else self.outdoor).update(nodes)
                    if station:
                        self.stations[station].update(nodes)
                        if platform:
                            self.platforms[station].update(nodes)
                    for a, b in zip(nodes, nodes[1:]):
                        self.union(a, b)
                        if station:
                            self.station_segments[station].append((a, b))

    def report(self, catalogue=None):
        outdoor_roots = {self.root(n) for n in self.outdoor}
        shared = self.indoor & self.outdoor
        stations = []
        for station, nodes in sorted(self.stations.items()):
            roots = {self.root(n) for n in nodes}
            platform_roots = {self.root(n) for n in self.platforms[station]}
            stations.append(dict(station=station, nodes=len(nodes), components=len(roots),
                                 shared_outdoor_vertices=len(nodes & shared),
                                 platform_components=len(platform_roots),
                                 platform_components_without_outdoor=len(platform_roots - outdoor_roots),
                                 nodes_without_outdoor=sum(self.root(n) not in outdoor_roots for n in nodes)))
        report = dict(schema_version=1, check='weak_source_connectivity',
                      quantization=dict(xy_degrees=1e-9, z_m=.01),
                      features=dict(self.features), nodes=len(self.parent),
                      components=len({self.root(n) for n in range(len(self.parent))}),
                      shared_indoor_outdoor_vertices=len(shared), stations=stations,
                      limitations=['Weak connectivity ignores direction, access restrictions and opening hours.',
                                   'Exit proximity is a QA measurement, not an inserted connector.'])
        if catalogue:
            venues = {s['venue_id']: s['station_code'] for s in catalogue['stations'].values() if s.get('venue_id')}
            exits = []
            # Station-local segments preserve vertical separation and avoid a
            # nearby street being mistaken for the station's own indoor link.
            station_bins = {}
            for station, segments in self.station_segments.items():
                bins = defaultdict(list)
                for a, b in segments:
                    ax, ay, _ = self.points[a]
                    bx, by, _ = self.points[b]
                    for gx in range(math.floor(min(ax, bx) * 102900 / 50), math.floor(max(ax, bx) * 102900 / 50) + 1):
                        for gy in range(math.floor(min(ay, by) * 111200 / 50), math.floor(max(ay, by) * 111200 / 50) + 1):
                            bins[(gx, gy)].append((a, b))
                station_bins[station] = bins
            for exit_id, item in sorted(catalogue['exits'].items()):
                station = venues.get(item.get('venue_id'))
                coord = item['coordinates']
                x, y, z = coord['lon'], coord['lat'], coord.get('z')
                bx, by = math.floor(x * 102900 / 50), math.floor(y * 111200 / 50)
                bins = station_bins.get(station, {})
                candidates = [n for dx in range(-2, 3) for dy in range(-2, 3) for n in bins.get((bx + dx, by + dy), [])]
                def distance(segment):
                    a, b = (self.points[n] for n in segment)
                    scale = (111320 * math.cos(math.radians(y)), 111320, 1)
                    origin = (x, y, z or 0)
                    av = [(v - o) * s for v, o, s in zip(a, origin, scale)]
                    bv = [(v - o) * s for v, o, s in zip(b, origin, scale)]
                    if z is None:
                        av[2] = bv[2] = 0
                    delta = [v - u for u, v in zip(av, bv)]
                    length2 = sum(v * v for v in delta)
                    fraction = max(0, min(1, -sum(u * v for u, v in zip(av, delta)) / length2)) if length2 else 0
                    return math.sqrt(sum((u + fraction * v) ** 2 for u, v in zip(av, delta)))
                segment = min(candidates, key=distance) if candidates else None
                node = segment[0] if segment else None
                gap = distance(segment) if segment else None
                exits.append(dict(exit_id=exit_id, name=item.get('name_en'), station=station,
                                  nearest_station_segment_distance_m=round(gap, 3) if gap is not None else None,
                                  nearest_vertex_has_outdoor_path=node is not None and self.root(node) in outdoor_roots,
                                  within_10m=gap is not None and gap <= 10))
            report['exits'] = exits
            report['exit_summary'] = dict(total=len(exits), within_10m=sum(e['within_10m'] for e in exits),
                                         with_outdoor_path=sum(e['within_10m'] and e['nearest_vertex_has_outdoor_path'] for e in exits),
                                         requires_review=[e['exit_id'] for e in exits if not e['within_10m'] or not e['nearest_vertex_has_outdoor_path']])
        return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prepared-dir', type=Path, required=True)
    parser.add_argument('--catalogue', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    network = Network()
    network.read(args.prepared_dir / '3D_Pedestrian_Network.gpkg', 'pedestrian_route')
    network.read(args.prepared_dir / '3D_Indoor_Network.gpkg', 'indoor_pedestrian_route', indoor=True)
    connectors = args.prepared_dir / 'Station_Precision_Connectors.gpkg'
    if connectors.exists():
        network.read(connectors, 'station_precision_connectors', indoor=True)
    report = network.report(json.loads(args.catalogue.read_text()) if args.catalogue else None)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({k: v for k, v in report.items() if k not in ('stations', 'exits')}, indent=2))


if __name__ == '__main__':
    main()
