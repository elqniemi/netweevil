#!/usr/bin/env python3
"""Write audited centimetre-scale station connectors without changing source files.

Only nodes in the same MTR station, in distinct weak components, within 1 cm
horizontally and 1 mm vertically qualify. These limits address digitization
rounding; they cannot bridge roads, floors or missing station corridors.
"""
import argparse
from collections import defaultdict
from datetime import datetime, timezone
import json
import math
from pathlib import Path
import sqlite3
import struct

from audit_hong_kong_network import Network


def connectors(network):
    result = []
    for station, nodes in sorted(network.stations.items()):
        bins = defaultdict(list)
        for node in sorted(nodes):
            lon, lat, z = network.points[node]
            x, y = lon * 102900, lat * 111200
            key = (math.floor(x / .01), math.floor(y / .01), math.floor(z / .001))
            for dx in (-1, 0, 1):
                for dy in (-1, 0, 1):
                    for dz in (-1, 0, 1):
                        for other in bins.get((key[0] + dx, key[1] + dy, key[2] + dz), ()):
                            if network.root(node) == network.root(other):
                                continue
                            ox, oy, oz = network.points[other]
                            horizontal = math.hypot((lon - ox) * 111320 * math.cos(math.radians(lat)), (lat - oy) * 111320)
                            if horizontal > .01 or abs(z - oz) > .001:
                                continue
                            result.append(dict(station=station, coordinates=[network.points[other], network.points[node]],
                                               horizontal_gap_m=horizontal, vertical_gap_m=abs(z - oz)))
                            network.union(node, other)
            bins[key].append(node)
    return result


def write_gpkg(path, rows):
    temporary = path.with_suffix('.tmp')
    temporary.unlink(missing_ok=True)
    with sqlite3.connect(temporary) as db:
        db.executescript('''
            PRAGMA application_id=1196444487;
            PRAGMA user_version=10300;
            CREATE TABLE gpkg_spatial_ref_sys(srs_name TEXT NOT NULL,srs_id INTEGER PRIMARY KEY,organization TEXT NOT NULL,organization_coordsys_id INTEGER NOT NULL,definition TEXT NOT NULL,description TEXT);
            CREATE TABLE gpkg_contents(table_name TEXT PRIMARY KEY,data_type TEXT NOT NULL,identifier TEXT UNIQUE,description TEXT DEFAULT '',last_change DATETIME NOT NULL,min_x DOUBLE,min_y DOUBLE,max_x DOUBLE,max_y DOUBLE,srs_id INTEGER);
            CREATE TABLE gpkg_geometry_columns(table_name TEXT NOT NULL,column_name TEXT NOT NULL,geometry_type_name TEXT NOT NULL,srs_id INTEGER NOT NULL,z TINYINT NOT NULL,m TINYINT NOT NULL,PRIMARY KEY(table_name,column_name));
            CREATE TABLE station_precision_connectors(OBJECTID INTEGER PRIMARY KEY,Shape BLOB NOT NULL,PedestrianRouteID INTEGER NOT NULL,MTRStationCode TEXT,MTRPlatformLevel INTEGER,FeatureType TEXT,provenance TEXT,horizontal_gap_m REAL,vertical_gap_m REAL);
        ''')
        db.executemany('INSERT INTO gpkg_spatial_ref_sys VALUES(?,?,?,?,?,?)', [
            ('Undefined Cartesian', -1, 'NONE', -1, 'undefined', ''),
            ('Undefined geographic', 0, 'NONE', 0, 'undefined', ''),
            ('WGS 84', 4326, 'EPSG', 4326, 'GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433],AUTHORITY["EPSG","4326"]]', 'Z retains HKPD metres')])
        coords = [p for row in rows for p in row['coordinates']]
        extent = [min(p[0] for p in coords), min(p[1] for p in coords), max(p[0] for p in coords), max(p[1] for p in coords)] if coords else [None] * 4
        db.execute('INSERT INTO gpkg_contents VALUES(?,?,?,?,?,?,?,?,?,?)', ('station_precision_connectors', 'features', 'station_precision_connectors', 'Same-station precision connectors; source coordinates unchanged', datetime.now(timezone.utc).isoformat(), *extent, 4326))
        db.execute('INSERT INTO gpkg_geometry_columns VALUES(?,?,?,?,?,?)', ('station_precision_connectors', 'Shape', 'LINESTRING', 4326, 1, 0))
        for i, row in enumerate(rows, 1):
            geometry = b'GP\0\1' + struct.pack('<i', 4326) + b'\1' + struct.pack('<II', 1002, 2) + b''.join(struct.pack('<ddd', *p) for p in row['coordinates'])
            db.execute('INSERT INTO station_precision_connectors VALUES(?,?,?,?,?,?,?,?,?)', (i, geometry, 9000000000 + i, row['station'], 0, 'precision_connector', 'same station; XY <= 0.01 m; Z <= 0.001 m', row['horizontal_gap_m'], row['vertical_gap_m']))
    temporary.replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prepared-dir', required=True, type=Path)
    args = parser.parse_args()
    network = Network()
    network.read(args.prepared_dir / '3D_Pedestrian_Network.gpkg', 'pedestrian_route')
    network.read(args.prepared_dir / '3D_Indoor_Network.gpkg', 'indoor_pedestrian_route', True)
    rows = connectors(network)
    output = args.prepared_dir / 'Station_Precision_Connectors.gpkg'
    write_gpkg(output, rows)
    report = dict(schema_version=1, connector_count=len(rows), max_horizontal_m=.01, max_vertical_m=.001, connectors=rows)
    output.with_suffix('.json').write_text(json.dumps(report, indent=2) + '\n')
    print(f'Wrote {len(rows)} audited precision connectors to {output}')


if __name__ == '__main__':
    main()
