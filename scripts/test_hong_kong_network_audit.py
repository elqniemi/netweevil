#!/usr/bin/env python3
import struct
import unittest
from audit_hong_kong_network import Network, coordinate_key, lines
from connect_hong_kong_precision_gaps import connectors


def gpkg_line(points, endian='<'):
    flag = 1 if endian == '<' else 0
    return (b'GP\0' + bytes([flag]) + struct.pack(endian + 'i', 4326)
            + bytes([flag]) + struct.pack(endian + 'II', 1002, len(points))
            + b''.join(struct.pack(endian + 'ddd', *point) for point in points))


class AuditTests(unittest.TestCase):
    def test_xyz_survives_both_byte_orders(self):
        points = [(114.1, 22.3, -12.5), (114.2, 22.4, 5.0)]
        for endian in ('<', '>'):
            self.assertEqual(lines(gpkg_line(points, endian)), [points])

    def test_stacked_crossings_do_not_connect(self):
        network = Network()
        ground = network.node((114, 22, 0))
        platform = network.node((114, 22, -10))
        network.outdoor.add(ground)
        network.indoor.add(platform)
        network.stations['TEST'].add(platform)
        network.platforms['TEST'].add(platform)
        self.assertNotEqual(ground, platform)
        report = network.report()
        self.assertEqual(report['stations'][0]['platform_components_without_outdoor'], 1)
        network.union(ground, platform)
        self.assertEqual(network.report()['stations'][0]['platform_components_without_outdoor'], 0)

    def test_quantization_matches_rust_negative_half(self):
        self.assertEqual(coordinate_key((0, 0, -.005))[2], -1)
        self.assertEqual(coordinate_key((0, 0, .005))[2], 1)

    def test_precision_connectors_only_join_same_station_same_height(self):
        network = Network()
        a = network.node((114, 22, 0))
        b = network.node((114.00000001, 22, 0))
        stacked = network.node((114, 22, 3))
        other_station = network.node((114.00000002, 22, 0))
        distant = network.node((114.000001, 22, 0))
        network.stations['A'].update((a, b, stacked, distant))
        network.stations['B'].add(other_station)
        result = connectors(network)
        self.assertEqual(len(result), 1)
        self.assertEqual(network.root(a), network.root(b))
        for node in (stacked, other_station, distant):
            self.assertNotEqual(network.root(a), network.root(node))


if __name__ == '__main__':
    unittest.main()
