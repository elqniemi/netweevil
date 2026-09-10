#!/usr/bin/env python3
"""Regression checks for truthful 3D station platform exports."""
import copy
import unittest
from scripts.export_hong_kong_station_surfaces import build_surfaces, contains, positions


def polygon(z=-5):
    return {'type': 'Polygon', 'coordinates': [[[114, 22.3, z], [114.01, 22.3, z],
            [114.01, 22.31, z], [114, 22.31, z], [114, 22.3, z]]]}


def surface(source='unit', z=-5):
    return {'type': 'Feature', 'geometry': polygon(z), 'properties': {
        'venue_id': 'v', 'platform_source_id': 's', 'station_code': 'ABC',
        'station_name_en': 'Example', 'station_name_zh': '', 'platform_source_kind': source,
        'level_id': 'l', 'level_name_en': 'Platform level', 'level_short_name': 'L1',
        'line_code': 'X', 'binding_z': -1, 'stop_id': 'legacy',
    }}


def catalog(z=-5):
    return {'platforms': {'p': {'venue_id': 'v', 'amenity_id': 'p', 'ref': '1',
        'coordinates': {'lon': 114.005, 'lat': 22.305, 'z': z},
        'lines_and_directions': [{'line_code': 'X', 'direction': 'UT'}]}}}


class StationSurfaceTests(unittest.TestCase):
    def test_deduplicates_shared_lines_and_preserves_original_xyz(self):
        a = surface()
        b = copy.deepcopy(a)
        b['properties']['line_code'] = 'Y'
        original = copy.deepcopy(a['geometry'])
        output, report = build_surfaces({'features': [a, b]}, catalog())
        self.assertEqual(report['surface_count'], 1)
        f = output['features'][0]
        self.assertEqual(f['geometry'], original)
        self.assertEqual(f['properties']['line_codes'], ['X', 'Y'])
        self.assertEqual(f['properties']['platform_numbers'], ['1'])
        self.assertEqual(f['properties']['source_binding_z_m'], [-1])
        self.assertEqual(f['properties']['z_min_m'], -5)
        self.assertEqual(f['properties']['kind'], 'platform_surface')

    def test_stacked_polygon_uses_height_and_holes_exclude_points(self):
        low, high = surface(z=-5), surface(z=5)
        high['properties']['platform_source_id'] = 'upper'
        output, report = build_surfaces({'features': [low, high]}, catalog())
        self.assertEqual(report['matched_numbered_points'], 1)
        self.assertEqual(output['features'][1]['properties']['platform_numbers'], [])
        geometry = polygon()
        geometry['coordinates'].append([[114.004, 22.304, -5], [114.006, 22.304, -5],
            [114.006, 22.306, -5], [114.004, 22.306, -5], [114.004, 22.304, -5]])
        self.assertFalse(contains(geometry, 114.005, 22.305))
        self.assertTrue(contains(geometry, 114.001, 22.301))

    def test_preserves_floor_fallback_without_inventing_number(self):
        output, report = build_surfaces({'features': [surface('platform_level_fallback')]}, catalog(z=9))
        props = output['features'][0]['properties']
        self.assertEqual(props['surface_kind'], 'platform_floor')
        self.assertEqual(props['platform_numbers'], [])
        self.assertEqual(len(report['unmatched_numbered_points']), 1)

    def test_rejects_conflicting_source_geometry_and_non_xyz(self):
        first, other = surface(), surface(z=9)
        with self.assertRaisesRegex(ValueError, 'conflicting geometry'):
            build_surfaces({'features': [first, other]}, catalog())
        geom = polygon()
        geom['coordinates'][0] = [p[:2] for p in geom['coordinates'][0]]
        with self.assertRaisesRegex(ValueError, 'finite XYZ'):
            list(positions(geom))

    def test_ambiguous_overlaps_do_not_get_verified_labels(self):
        first, other = surface(), surface()
        other['properties']['platform_source_id'] = 'overlap'
        output, report = build_surfaces({'features': [first, other]}, catalog())
        self.assertEqual(report['matched_numbered_points'], 0)
        self.assertEqual(len(report['ambiguous_numbered_points']), 1)
        self.assertTrue(all(not f['properties']['platform_numbers'] for f in output['features']))


if __name__ == '__main__':
    unittest.main()
