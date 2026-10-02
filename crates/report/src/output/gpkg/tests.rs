use rusqlite::Connection;

use super::{GeoPackageWriter, wkb_linestring_z};
use crate::test_support::{TempFile, first_wkb_geometry_type, geometry_z_flag};

#[test]
fn writes_iso_linestring_z_wkb() {
    let bytes = wkb_linestring_z(&[[114.1, 22.3, 12.5], [114.2, 22.4, 18.0]]);
    assert_eq!(bytes[0], 1);
    assert_eq!(u32::from_le_bytes(bytes[1..5].try_into().unwrap()), 1002);
    assert_eq!(u32::from_le_bytes(bytes[5..9].try_into().unwrap()), 2);
    assert_eq!(bytes.len(), 9 + 2 * 24);
}

#[test]
fn registers_three_dimensional_feature_tables() {
    let path = TempFile::new("gpkg-z.gpkg");
    let mut writer = GeoPackageWriter::create(path.as_ref()).expect("GeoPackage created");
    writer
        .create_feature_table_3d("routes", &[], "LINESTRING", None)
        .expect("3D table created");
    writer
        .insert_feature_3d("routes", &[], &[[114.1, 22.3, 4.0], [114.2, 22.4, 8.0]])
        .expect("3D feature inserted");
    drop(writer);

    let connection = Connection::open(&path).expect("GeoPackage opens");
    assert_eq!(geometry_z_flag(&connection, "routes"), 1);
    assert_eq!(first_wkb_geometry_type(&connection, "routes"), 1002);
}
