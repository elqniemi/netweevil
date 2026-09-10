#!/usr/bin/env python3
"""Regression checks for source-preserving Hong Kong GTFS fusion."""
import unittest
from scripts.build_hong_kong_transit import namespace, platform_bindings, rail_subset, validate


class HongKongTransitTests(unittest.TestCase):
    def test_namespaces_foreign_keys_without_altering_times(self):
        source = {'stop_times': [{'trip_id': 'same', 'stop_id': 'same', 'arrival_time': '25:01:30'}],
                  'pathways': [{'pathway_id': 'p', 'from_stop_id': 'a', 'to_stop_id': 'b'}]}
        result = namespace(source, 'rail:')
        self.assertEqual(result['stop_times'][0], {'trip_id': 'rail:same', 'stop_id': 'rail:same', 'arrival_time': '25:01:30'})
        self.assertEqual(result['pathways'][0]['from_stop_id'], 'rail:a')
        self.assertEqual(source['stop_times'][0]['trip_id'], 'same')

    def test_selects_rail_without_duplicate_surface_routes(self):
        feed = {
            'routes': [{'route_id': 'MTR-X', 'agency_id': 'MTRR'}, {'route_id': 'BUS', 'agency_id': 'KMB'}],
            'trips': [{'trip_id': 'rail', 'route_id': 'MTR-X', 'service_id': 'shared', 'shape_id': 'r'},
                      {'trip_id': 'bus', 'route_id': 'BUS', 'service_id': 'shared', 'shape_id': 'b'}],
            'stops': [{'stop_id': 'MTR-PLATFORM-X-1'}, {'stop_id': 'MTR-X'}, {'stop_id': 'MTR-ENTRANCE-X-A'}, {'stop_id': 'BUS'}],
            'stop_times': [{'trip_id': 'rail', 'stop_id': 'MTR-PLATFORM-X-1'}, {'trip_id': 'bus', 'stop_id': 'BUS'}],
            'calendar': [{'service_id': 'shared', 'monday': '1'}],
            'frequencies': [{'trip_id': 'rail', 'headway_secs': '213'}, {'trip_id': 'bus', 'headway_secs': '300'}],
            'shapes': [{'shape_id': 'r'}, {'shape_id': 'b'}],
        }
        result = rail_subset(feed)
        self.assertEqual(len(result['routes']), 1)
        self.assertEqual(len(result['stops']), 3)
        self.assertEqual(result['calendar'], feed['calendar'])
        self.assertEqual(result['frequencies'], [{'trip_id': 'rail', 'headway_secs': '213'}])
        self.assertEqual(result['shapes'], [{'shape_id': 'r'}])

    def test_numbered_platform_retains_z_and_station_filter(self):
        rail = {'trips': [{'trip_id': 'MTR-X-UT', 'route_id': 'MTR-X'}],
                'stop_times': [{'trip_id': 'MTR-X-UT', 'stop_id': 'MTR-PLATFORM-ABC-1'}],
                'stops': [{'stop_id': 'MTR-PLATFORM-ABC-1', 'stop_lon': '114.1', 'stop_lat': '22.3'}]}
        join = [{'venue_id': 'v', 'station_code': 'ABC', 'line_code': 'X'}]
        catalog = {'platforms': {'p': {'venue_id': 'v', 'ref': '1', 'amenity_id': 'p',
                    'coordinates': {'lon': 114.1, 'lat': 22.3, 'z': -12.5}}}}
        bindings, audit, _ = platform_bindings(rail, join, catalog, 'hk')
        self.assertEqual(bindings['bindings'][0]['coordinate']['z'], -12.5)
        self.assertEqual(bindings['bindings'][0]['coordinate']['attribute_filter'],
                         {'mtr_station_code': 'ABC', 'mtr_platform_level': '1'})
        self.assertEqual(audit[0]['method'], 'csdi_numbered_platform')
        catalog['platforms']['p']['coordinates']['lon'] = 115
        with self.assertRaisesRegex(ValueError, 'differs from its GTFS'):
            platform_bindings(rail, join, catalog, 'hk')

    def test_rejects_missing_trip_calendar(self):
        feed = {'stops': [{'stop_id': 's'}], 'routes': [{'route_id': 'r'}],
                'trips': [{'trip_id': 't', 'route_id': 'r', 'service_id': 'missing'}], 'stop_times': []}
        with self.assertRaisesRegex(ValueError, 'missing route/calendar'):
            validate(feed)


if __name__ == '__main__':
    unittest.main()
