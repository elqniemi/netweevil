#!/usr/bin/env python3
import struct
import argparse
from pathlib import Path
import tempfile
import unittest
import zlib

import numpy as np
from osgeo import gdal

from prepare_hong_kong_terrain import fill_display_nodata, png_bytes, prepare, terrarium_encode


class TerrainTests(unittest.TestCase):
    def test_negative_and_channel_carry_roundtrip(self):
        source = np.array([[-33.63, -0.01, 0, 255.996, 256.004, 958.321]])
        rgb = terrarium_encode(source).astype(float)
        decoded = rgb[..., 0] * 256 + rgb[..., 1] + rgb[..., 2] / 256 - 32768
        np.testing.assert_allclose(decoded, source, atol=1 / 512)

    def test_invalid_heights_rejected(self):
        for value in [float("nan"), float("inf"), -40000, 40000]:
            with self.assertRaises(ValueError):
                terrarium_encode(np.array([[value]]))

    def test_nodata_fill_preserves_samples_and_mask(self):
        source = np.array([[-9999., -9999., -9999., -9999.], [-9999., -10., 20., -9999.], [4., -9999., -9999., 8.]])
        valid = source != -9999
        original, mask = source.copy(), valid.copy()
        filled = fill_display_nodata(source, valid)
        np.testing.assert_array_equal(filled, [[-10, -10, 20, 20], [-10, -10, 20, 20], [4, 4, 8, 8]])
        np.testing.assert_array_equal(source, original)
        np.testing.assert_array_equal(valid, mask)
        np.testing.assert_array_equal(filled[valid], source[valid])
        with self.assertRaises(ValueError):
            fill_display_nodata(source, np.zeros(source.shape, dtype=bool))

    def test_png_stores_exact_rgb_without_resampling(self):
        rgb = terrarium_encode(np.array([[-33.63, 0], [255.996, 256.004]]))
        png = png_bytes(rgb)
        self.assertEqual(png[:8], b"\x89PNG\r\n\x1a\n")
        offset, payload = 8, b""
        while offset < len(png):
            size = struct.unpack(">I", png[offset:offset + 4])[0]
            kind, data = png[offset + 4:offset + 8], png[offset + 8:offset + 8 + size]
            self.assertEqual(zlib.crc32(kind + data) & 0xffffffff,
                             struct.unpack(">I", png[offset + 8 + size:offset + 12 + size])[0])
            if kind == b"IDAT":
                payload += data
            offset += 12 + size
        self.assertEqual(zlib.decompress(payload), b"".join(b"\0" + row.tobytes() for row in rgb))

    def test_small_hk_raster_warp_tiles_and_completed_cache(self):
        gdal.UseExceptions()
        with tempfile.TemporaryDirectory() as folder:
            source, output = Path(folder) / "fixture.tif", Path(folder) / "terrain"
            ds = gdal.GetDriverByName("GTiff").Create(str(source), 16, 16, 1, gdal.GDT_Float32)
            ds.SetGeoTransform([838000, 5, 0, 819000, 0, -5])
            ds.GetRasterBand(1).SetNoDataValue(-9999)
            values = np.full((16, 16), -20.25, dtype=np.float32)
            values[:2] = -9999
            ds.GetRasterBand(1).WriteArray(values)
            ds = None
            args = argparse.Namespace(input=str(source), output_dir=str(output), minzoom=15, maxzoom=15)
            manifest = prepare(args)
            self.assertEqual(manifest["vertical_datum"], "HKPD")
            self.assertTrue((output / "manifest.json").exists())
            tiles = list((output / manifest["version"] / "tiles").glob("*/*/*.png"))
            self.assertEqual(len(tiles), manifest["tile_count"])
            self.assertGreater(len(tiles), 0)
            rgb = gdal.Open(str(tiles[0])).ReadAsArray().astype(float)
            decoded = rgb[0] * 256 + rgb[1] + rgb[2] / 256 - 32768
            np.testing.assert_allclose(decoded, -20.25, atol=1 / 256)
            self.assertGreater(manifest["coverage_by_zoom"]["15"]["display_filled_samples"], 0)
            self.assertEqual(prepare(args), manifest)


if __name__ == "__main__":
    unittest.main()
