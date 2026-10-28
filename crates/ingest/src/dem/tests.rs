use super::*;
use crate::test_util::TempDir;

/// 4x3 geographic grid of 0.01 degree cells with its lower-left corner at
/// (6.00, 53.00). Row 0 is the top row; one cell is nodata.
const ASCII_GRID: &str = "ncols 4
nrows 3
xllcorner 6.0
yllcorner 53.0
cellsize 0.01
NODATA_value -9999
 30 31 32 33
 20 21 -9999 23
 10 11 12 13
";

fn write(dir: &TempDir, name: &str, contents: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

#[test]
fn ascii_grids_sample_bilinearly_and_respect_nodata() {
    let dir = TempDir::new("dem-ascii");
    let dem = DemRaster::read(&write(&dir, "dem.asc", ASCII_GRID.as_bytes()), None, None).unwrap();
    assert_eq!(dem.crs, RasterCrs::Geographic);
    assert_eq!(dem.format, DemFormat::AsciiGrid);
    assert_eq!((dem.grid.columns, dem.grid.rows), (4, 3));
    let near = |value: Option<f64>, expected: f64| (value.unwrap() - expected).abs() < 1e-6;
    // Cell centres.
    assert!(near(dem.sample(6.005, 53.025), 30.0));
    assert!(near(dem.sample(6.035, 53.005), 13.0));
    // Halfway between the centres of rows 0 and 1 in column 0.
    assert!((dem.sample(6.005, 53.02).unwrap() - 25.0).abs() < 1e-3);
    // Own cell is nodata.
    assert_eq!(dem.sample(6.025, 53.015), None);
    // Outside the grid.
    assert_eq!(dem.sample(5.99, 53.01), None);
    assert!(dem.cell_size_m(53.0) > 600.0);
}

#[test]
fn wrapped_rows_and_cropping_read_the_same_cells() {
    let dir = TempDir::new("dem-wrapped");
    let wrapped = ASCII_GRID.replace(" 20 21 -9999 23", " 20 21\n -9999 23");
    let full =
        DemRaster::read(&write(&dir, "full.asc", ASCII_GRID.as_bytes()), None, None).unwrap();
    let crop = LonLatBounds {
        min_lon: 6.031,
        min_lat: 53.001,
        max_lon: 6.039,
        max_lat: 53.009,
    };
    for path in [
        write(&dir, "wrapped.asc", wrapped.as_bytes()),
        write(&dir, "same.asc", ASCII_GRID.as_bytes()),
    ] {
        let cropped = DemRaster::read(&path, None, Some(crop)).unwrap();
        assert_eq!(cropped.source_grid, full.grid);
        assert!(cropped.grid.columns <= full.grid.columns);
        assert!((cropped.sample(6.035, 53.005).unwrap() - 13.0).abs() < 1e-6);
        let (cropped, full) = (cropped.sample(6.030, 53.007), full.sample(6.030, 53.007));
        assert!((cropped.unwrap() - full.unwrap()).abs() < 1e-9);
    }
    let far = LonLatBounds {
        min_lon: 7.0,
        min_lat: 54.0,
        max_lon: 7.1,
        max_lat: 54.1,
    };
    let error = DemRaster::read(&dir.join("full.asc"), None, Some(far))
        .err()
        .expect("a DEM that misses the network is rejected");
    assert!(error.to_string().contains("does not cover"), "{error}");
}

#[test]
fn projected_ascii_grids_need_a_crs() {
    let dir = TempDir::new("dem-projected");
    // A 100 m square of 5 m HK1980 Grid cells around the grid's false origin
    // (E 836694.05, N 819069.80), all at 40 m.
    let mut grid =
        "ncols 20\nnrows 20\nxllcorner 836644\nyllcorner 819020\ncellsize 5\n".to_string();
    for _ in 0..20 {
        grid.push_str(&" 40".repeat(20));
        grid.push('\n');
    }
    let path = write(&dir, "hk.asc", grid.as_bytes());
    let error = DemRaster::read(&path, None, None)
        .err()
        .expect("CRS is required");
    assert!(error.to_string().contains("--dem-crs"), "{error}");
    let dem = DemRaster::read(&path, Some("EPSG:2326"), None).unwrap();
    assert!(
        matches!(&dem.crs, RasterCrs::Proj4 { label: Some(label), .. } if label == "EPSG:2326")
    );
    // The origin's published HK80 latitude/longitude (22 18 43.68 N,
    // 114 10 42.80 E) shifted by the usual HK80-to-WGS84 offset of about
    // -5.5" and +8.8".
    let (lon, lat) = (114.181_000, 22.310_606);
    let (x, y) = CrsTransform::new(&dem.crs)
        .unwrap()
        .to_raster(lon, lat)
        .unwrap();
    assert!(
        (x - 836_694.05).abs() < 30.0 && (y - 819_069.80).abs() < 30.0,
        "{x} {y}"
    );
    assert_eq!(dem.sample(lon, lat), Some(40.0));
}

#[test]
fn crs_specs_resolve_to_known_definitions() {
    assert_eq!(resolve_crs("EPSG:4326").unwrap(), RasterCrs::Geographic);
    assert_eq!(resolve_crs("4258").unwrap(), RasterCrs::Geographic);
    let utm = resolve_crs("epsg:32631").unwrap();
    assert!(
        matches!(&utm, RasterCrs::Proj4 { definition, .. } if definition.contains("+zone=31 "))
    );
    let south = resolve_crs("EPSG:32733").unwrap();
    assert!(matches!(&south, RasterCrs::Proj4 { definition, .. } if definition.contains("+south")));
    let custom = resolve_crs("+proj=utm +zone=33 +datum=WGS84 +units=m +no_defs").unwrap();
    assert!(matches!(custom, RasterCrs::Proj4 { label: None, .. }));
    assert!(resolve_crs("EPSG:9999999").is_err());
    assert!(resolve_crs("+proj=nonsense").is_err());

    // RD New maps Amersfoort's reference point to its false origin.
    let rd = CrsTransform::new(&resolve_crs("EPSG:28992").unwrap()).unwrap();
    let (x, y) = rd.to_raster(5.387_206_21, 52.155_174_40).unwrap();
    assert!(
        (x - 155_000.0).abs() < 2.0 && (y - 463_000.0).abs() < 2.0,
        "{x} {y}"
    );
}

/// Single-band float GeoTIFF with GeoKeys and a GDAL nodata tag.
fn write_geotiff(path: &Path, pixel_is_point: bool) {
    use tiff::encoder::{TiffEncoder, colortype::Gray32Float};
    use tiff::tags::Tag;
    let file = File::create(path).unwrap();
    let mut encoder = TiffEncoder::new(file).unwrap();
    let mut image = encoder.new_image::<Gray32Float>(3, 2).unwrap();
    let raster_type = if pixel_is_point { 2_u16 } else { 1 };
    image
        .encoder()
        .write_tag(Tag::ModelPixelScaleTag, &[0.5_f64, 0.5, 0.0][..])
        .unwrap();
    image
        .encoder()
        .write_tag(
            Tag::ModelTiepointTag,
            &[0.0_f64, 0.0, 0.0, 10.0, 50.0, 0.0][..],
        )
        .unwrap();
    image
        .encoder()
        .write_tag(
            Tag::GeoKeyDirectoryTag,
            &[
                1_u16,
                1,
                0,
                3,
                1024,
                0,
                1,
                2,
                1025,
                0,
                1,
                raster_type,
                2048,
                0,
                1,
                4326,
            ][..],
        )
        .unwrap();
    image
        .encoder()
        .write_tag(Tag::GdalNodata, "-32768")
        .unwrap();
    image
        .write_data(&[1.0_f32, 2.0, 3.0, 4.0, -32768.0, 6.0])
        .unwrap();
}

#[test]
fn geotiffs_read_georeferencing_crs_and_nodata() {
    let dir = TempDir::new("dem-geotiff");
    let path = dir.join("dem.tif");
    write_geotiff(&path, false);
    let dem = DemRaster::read(&path, None, None).unwrap();
    assert_eq!(dem.format, DemFormat::GeoTiff);
    assert_eq!(dem.crs, RasterCrs::Geographic);
    assert_eq!(
        (dem.grid.min_x, dem.grid.max_y, dem.grid.cell_width),
        (10.0, 50.0, 0.5)
    );
    assert_eq!(dem.sample(10.25, 49.75), Some(1.0));
    assert_eq!(dem.sample(10.75, 49.25), None);
    assert_eq!(dem.sample(11.25, 49.25), Some(6.0));

    // PixelIsPoint shifts the grid by half a cell.
    write_geotiff(&path, true);
    let dem = DemRaster::read(&path, None, None).unwrap();
    assert_eq!((dem.grid.min_x, dem.grid.max_y), (9.75, 50.25));
    assert_eq!(dem.sample(10.0, 50.0), Some(1.0));
}

#[test]
fn stored_tiles_sample_like_the_in_memory_raster() {
    let dir = TempDir::new("dem-tiles");
    let grid = RasterGrid {
        columns: 7,
        rows: 5,
        min_x: 6.0,
        max_y: 53.05,
        cell_width: 0.01,
        cell_height: 0.01,
    };
    let values = (0..35)
        .map(|index| {
            if index == 9 {
                f32::NAN
            } else {
                (index * 7 % 11) as f32 + 0.04 * index as f32
            }
        })
        .collect::<Vec<_>>();
    let dem = DemRaster::from_values(grid, RasterCrs::Geographic, values).unwrap();
    let source = dem.source_info(Path::new("dem.asc"), Some("NAP".to_string()));
    let (header, tiles) = dem.to_tiles(source, 3);
    assert_eq!(tiles.len(), 6);
    let path = dir.join("raster.bin");
    netweevil_persist::write_elevation_raster(&path, &header, &tiles).unwrap();
    let sampler = ElevationSampler::open(&path).unwrap();
    assert_eq!(
        sampler.header().source.vertical_datum.as_deref(),
        Some("NAP")
    );
    for step in 0..60 {
        let lon = 6.0005 + step as f64 * 0.00115;
        let lat = 53.0005 + step as f64 * 0.0008;
        let stored = sampler.sample(lon, lat).unwrap();
        match (stored, dem.sample(lon, lat)) {
            (Some(stored), Some(memory)) => assert!((stored - memory).abs() <= 0.05 + 1e-6),
            (None, None) => {}
            other => panic!("coverage differs at {lon},{lat}: {other:?}"),
        }
    }
    assert_eq!(sampler.sample(5.0, 53.0).unwrap(), None);
}
