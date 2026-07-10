use std::fs::{self};
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use super::common::*;

const EPSG_4326: i32 = 4326;

pub(super) struct GeoPackageWriter {
    connection: Connection,
}

impl GeoPackageWriter {
    pub(super) fn create(path: &Path) -> Result<Self> {
        ensure_parent_dir(path)?;
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("removing existing {}", path.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("creating GeoPackage {}", path.display()))?;
        let writer = Self { connection };
        writer.initialize()?;
        Ok(writer)
    }

    fn initialize(&self) -> Result<()> {
        self.connection.execute_batch(
            "
            CREATE TABLE gpkg_spatial_ref_sys (
                srs_name TEXT NOT NULL,
                srs_id INTEGER NOT NULL PRIMARY KEY,
                organization TEXT NOT NULL,
                organization_coordsys_id INTEGER NOT NULL,
                definition TEXT NOT NULL,
                description TEXT
            );
            CREATE TABLE gpkg_contents (
                table_name TEXT NOT NULL PRIMARY KEY,
                data_type TEXT NOT NULL,
                identifier TEXT UNIQUE,
                description TEXT DEFAULT '',
                last_change DATETIME NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                min_x DOUBLE,
                min_y DOUBLE,
                max_x DOUBLE,
                max_y DOUBLE,
                srs_id INTEGER,
                CONSTRAINT fk_gc_r_srs_id FOREIGN KEY (srs_id) REFERENCES gpkg_spatial_ref_sys(srs_id)
            );
            CREATE TABLE gpkg_geometry_columns (
                table_name TEXT NOT NULL,
                column_name TEXT NOT NULL,
                geometry_type_name TEXT NOT NULL,
                srs_id INTEGER NOT NULL,
                z TINYINT NOT NULL,
                m TINYINT NOT NULL,
                PRIMARY KEY (table_name, column_name),
                CONSTRAINT fk_ggc_tn FOREIGN KEY (table_name) REFERENCES gpkg_contents(table_name),
                CONSTRAINT fk_ggc_srs FOREIGN KEY (srs_id) REFERENCES gpkg_spatial_ref_sys(srs_id)
            );
            ",
        )?;
        self.connection.execute(
            "INSERT INTO gpkg_spatial_ref_sys (srs_name, srs_id, organization, organization_coordsys_id, definition, description)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "Undefined Cartesian",
                -1,
                "NONE",
                -1,
                "undefined",
                "undefined Cartesian coordinate reference system",
            ],
        )?;
        self.connection.execute(
            "INSERT INTO gpkg_spatial_ref_sys (srs_name, srs_id, organization, organization_coordsys_id, definition, description)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "Undefined Geographic",
                0,
                "NONE",
                0,
                "undefined",
                "undefined geographic coordinate reference system",
            ],
        )?;
        self.connection.execute(
            "INSERT INTO gpkg_spatial_ref_sys (srs_name, srs_id, organization, organization_coordsys_id, definition, description)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "WGS 84 geodetic",
                EPSG_4326,
                "EPSG",
                EPSG_4326,
                r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]]"#,
                "longitude/latitude coordinates in decimal degrees on the WGS 84 spheroid",
            ],
        )?;
        Ok(())
    }

    pub(super) fn create_feature_table(
        &mut self,
        table_name: &str,
        columns: &[(&str, &str)],
        geometry_type_name: &str,
        extent: Option<Extent>,
    ) -> Result<()> {
        self.create_feature_table_with_z(table_name, columns, geometry_type_name, extent, false)
    }

    pub(super) fn create_feature_table_3d(
        &mut self,
        table_name: &str,
        columns: &[(&str, &str)],
        geometry_type_name: &str,
        extent: Option<Extent>,
    ) -> Result<()> {
        self.create_feature_table_with_z(table_name, columns, geometry_type_name, extent, true)
    }

    fn create_feature_table_with_z(
        &mut self,
        table_name: &str,
        columns: &[(&str, &str)],
        geometry_type_name: &str,
        extent: Option<Extent>,
        has_z: bool,
    ) -> Result<()> {
        let mut sql = format!("CREATE TABLE {table_name} (id INTEGER PRIMARY KEY AUTOINCREMENT");
        for (name, definition) in columns {
            sql.push_str(&format!(", {name} {definition}"));
        }
        sql.push_str(", geom BLOB NOT NULL)");
        self.connection.execute_batch(&sql)?;
        self.connection.execute(
            "INSERT INTO gpkg_contents
             (table_name, data_type, identifier, description, min_x, min_y, max_x, max_y, srs_id)
             VALUES (?1, 'features', ?1, '', ?2, ?3, ?4, ?5, ?6)",
            params![
                table_name,
                extent.map(|value| value.min_x),
                extent.map(|value| value.min_y),
                extent.map(|value| value.max_x),
                extent.map(|value| value.max_y),
                EPSG_4326,
            ],
        )?;
        self.connection.execute(
            "INSERT INTO gpkg_geometry_columns
             (table_name, column_name, geometry_type_name, srs_id, z, m)
             VALUES (?1, 'geom', ?2, ?3, ?4, 0)",
            params![table_name, geometry_type_name, EPSG_4326, has_z as i32],
        )?;
        Ok(())
    }

    pub(super) fn create_attribute_table(
        &mut self,
        table_name: &str,
        columns: &[(&str, &str)],
    ) -> Result<()> {
        let mut sql = format!("CREATE TABLE {table_name} (id INTEGER PRIMARY KEY AUTOINCREMENT");
        for (name, definition) in columns {
            sql.push_str(&format!(", {name} {definition}"));
        }
        sql.push(')');
        self.connection.execute_batch(&sql)?;
        self.connection.execute(
            "INSERT INTO gpkg_contents
             (table_name, data_type, identifier, description, srs_id)
             VALUES (?1, 'attributes', ?1, '', NULL)",
            params![table_name],
        )?;
        Ok(())
    }

    pub(super) fn insert_feature_3d(
        &mut self,
        table_name: &str,
        fields: &[(&str, SqlValue)],
        coords: &[[f64; 3]],
    ) -> Result<()> {
        let mut field_names = fields.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        field_names.push("geom");
        let sql = format!(
            "INSERT INTO {table_name} ({}) VALUES ({})",
            field_names.join(", "),
            vec!["?"; field_names.len()].join(", ")
        );
        let geometry = gpkg_linestring_z(coords);
        let mut statement = self.connection.prepare(&sql)?;
        let mut values = fields
            .iter()
            .map(|(_, value)| value.as_param())
            .collect::<Vec<_>>();
        values.push(rusqlite::types::Value::Blob(geometry));
        statement.execute(rusqlite::params_from_iter(values))?;
        Ok(())
    }

    pub(super) fn insert_feature_wkb(
        &mut self,
        table_name: &str,
        fields: &[(&str, SqlValue)],
        wkb: &[u8],
    ) -> Result<()> {
        let mut field_names = fields.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        field_names.push("geom");
        let sql = format!(
            "INSERT INTO {table_name} ({}) VALUES ({})",
            field_names.join(", "),
            vec!["?"; field_names.len()].join(", ")
        );
        let geometry = gpkg_wkb(wkb);
        let mut statement = self.connection.prepare(&sql)?;
        let mut values = fields
            .iter()
            .map(|(_, value)| value.as_param())
            .collect::<Vec<_>>();
        values.push(rusqlite::types::Value::Blob(geometry));
        statement.execute(rusqlite::params_from_iter(values))?;
        Ok(())
    }

    pub(super) fn insert_row(
        &mut self,
        table_name: &str,
        fields: &[(&str, SqlValue)],
    ) -> Result<()> {
        let field_names = fields.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        let sql = format!(
            "INSERT INTO {table_name} ({}) VALUES ({})",
            field_names.join(", "),
            vec!["?"; field_names.len()].join(", ")
        );
        let mut statement = self.connection.prepare(&sql)?;
        let values = fields
            .iter()
            .map(|(_, value)| value.as_param())
            .collect::<Vec<_>>();
        statement.execute(rusqlite::params_from_iter(values))?;
        Ok(())
    }

    pub(super) fn begin_transaction(&mut self) -> Result<()> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE TRANSACTION")
            .context("starting GeoPackage transaction")
    }

    pub(super) fn commit_transaction(&mut self) -> Result<()> {
        self.connection
            .execute_batch("COMMIT")
            .context("committing GeoPackage transaction")
    }
}

pub(super) enum SqlValue {
    Text(String),
    NullableText(Option<String>),
    Integer(i64),
    NullableInteger(Option<i64>),
    Real(f64),
    NullableReal(Option<f64>),
}

impl SqlValue {
    fn as_param(&self) -> rusqlite::types::Value {
        use rusqlite::types::Value;

        match self {
            SqlValue::Text(value) => Value::Text(value.clone()),
            SqlValue::NullableText(value) => value
                .as_ref()
                .map_or(Value::Null, |value| Value::Text(value.clone())),
            SqlValue::Integer(value) => Value::Integer(*value),
            SqlValue::NullableInteger(value) => value.map_or(Value::Null, Value::Integer),
            SqlValue::Real(value) => Value::Real(*value),
            SqlValue::NullableReal(value) => value.map_or(Value::Null, Value::Real),
        }
    }
}

fn gpkg_linestring_z(coords: &[[f64; 3]]) -> Vec<u8> {
    gpkg_wkb(&wkb_linestring_z(&coerce_linestring_coords_z(coords)))
}

fn gpkg_wkb(wkb: &[u8]) -> Vec<u8> {
    let mut binary = Vec::new();
    binary.extend_from_slice(b"GP");
    binary.push(0);
    binary.push(1);
    binary.extend_from_slice(&EPSG_4326.to_le_bytes());
    binary.extend_from_slice(wkb);
    binary
}

/// ISO WKB `LineString Z` (type code 1002), as required by GeoPackage for
/// XYZ coordinates.
pub(super) fn wkb_linestring_z(coords: &[[f64; 3]]) -> Vec<u8> {
    let coords = coerce_linestring_coords_z(coords);
    let mut binary = Vec::with_capacity(1 + 4 + 4 + coords.len() * 24);
    binary.push(1);
    binary.extend_from_slice(&1002_u32.to_le_bytes());
    binary.extend_from_slice(&(coords.len() as u32).to_le_bytes());
    for [x, y, z] in coords {
        binary.extend_from_slice(&x.to_le_bytes());
        binary.extend_from_slice(&y.to_le_bytes());
        binary.extend_from_slice(&z.to_le_bytes());
    }
    binary
}

pub(super) fn wkb_multilinestring_z(lines: &[Vec<[f64; 3]>]) -> Vec<u8> {
    let mut binary = Vec::new();
    binary.push(1);
    binary.extend_from_slice(&1005_u32.to_le_bytes());
    binary.extend_from_slice(&(lines.len() as u32).to_le_bytes());
    for line in lines {
        binary.extend_from_slice(&wkb_linestring_z(line));
    }
    binary
}

fn wkb_polygon(rings: &[Vec<[f64; 2]>]) -> Vec<u8> {
    let mut binary = Vec::new();
    binary.push(1);
    binary.extend_from_slice(&3_u32.to_le_bytes());
    binary.extend_from_slice(&(rings.len() as u32).to_le_bytes());
    for ring in rings {
        binary.extend_from_slice(&(ring.len() as u32).to_le_bytes());
        for [x, y] in ring {
            binary.extend_from_slice(&x.to_le_bytes());
            binary.extend_from_slice(&y.to_le_bytes());
        }
    }
    binary
}

pub(super) fn wkb_multipolygon(polygons: &[Vec<Vec<[f64; 2]>>]) -> Vec<u8> {
    let mut binary = Vec::new();
    binary.push(1);
    binary.extend_from_slice(&6_u32.to_le_bytes());
    binary.extend_from_slice(&(polygons.len() as u32).to_le_bytes());
    for polygon in polygons {
        binary.extend_from_slice(&wkb_polygon(polygon));
    }
    binary
}

#[cfg(test)]
mod tests {
    use super::{GeoPackageWriter, wkb_linestring_z};
    use rusqlite::Connection;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

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
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("netweevil-gpkg-z-{unique}.gpkg"));
        let mut writer = GeoPackageWriter::create(&path).expect("GeoPackage created");
        writer
            .create_feature_table_3d("routes", &[], "LINESTRING", None)
            .expect("3D table created");
        writer
            .insert_feature_3d("routes", &[], &[[114.1, 22.3, 4.0], [114.2, 22.4, 8.0]])
            .expect("3D feature inserted");
        drop(writer);

        let connection = Connection::open(&path).expect("GeoPackage opens");
        let z: i64 = connection
            .query_row(
                "SELECT z FROM gpkg_geometry_columns WHERE table_name = 'routes'",
                [],
                |row| row.get(0),
            )
            .expect("z metadata present");
        assert_eq!(z, 1);
        let geometry: Vec<u8> = connection
            .query_row("SELECT geom FROM routes", [], |row| row.get(0))
            .expect("geometry present");
        assert_eq!(
            u32::from_le_bytes(geometry[9..13].try_into().unwrap()),
            1002
        );
        fs::remove_file(path).expect("temporary GeoPackage removed");
    }
}
