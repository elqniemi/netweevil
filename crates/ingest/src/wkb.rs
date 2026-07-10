use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GeoPackageGeometry {
    pub(crate) srs_id: i32,
    pub(crate) lines: Vec<Vec<[f64; 3]>>,
}

pub(crate) fn parse_geopackage_lines(blob: &[u8]) -> Result<GeoPackageGeometry> {
    if blob.len() < 8 || &blob[..2] != b"GP" {
        bail!("invalid GeoPackage geometry header");
    }
    if blob[2] != 0 {
        bail!("unsupported GeoPackage geometry version {}", blob[2]);
    }
    let flags = blob[3];
    let little_endian = flags & 1 != 0;
    let empty = flags & 0x10 != 0;
    let envelope_code = (flags >> 1) & 0x07;
    let envelope_bytes = match envelope_code {
        0 => 0,
        1 => 32,
        2 | 3 => 48,
        4 => 64,
        value => bail!("invalid GeoPackage envelope type {value}"),
    };
    let wkb_offset = 8 + envelope_bytes;
    if blob.len() < wkb_offset {
        bail!("truncated GeoPackage geometry envelope");
    }
    let srs_id = read_i32(&blob[4..8], little_endian);
    if empty {
        return Ok(GeoPackageGeometry {
            srs_id,
            lines: Vec::new(),
        });
    }

    let mut cursor = Cursor::new(&blob[wkb_offset..]);
    let lines = parse_wkb_geometry(&mut cursor)?;
    if cursor.remaining() != 0 {
        bail!("GeoPackage geometry contains trailing WKB bytes");
    }
    Ok(GeoPackageGeometry { srs_id, lines })
}

fn parse_wkb_geometry(cursor: &mut Cursor<'_>) -> Result<Vec<Vec<[f64; 3]>>> {
    let little_endian = match cursor.read_u8()? {
        0 => false,
        1 => true,
        value => bail!("invalid WKB byte order {value}"),
    };
    let raw_type = cursor.read_u32(little_endian)?;
    let (base_type, has_z, has_m, has_srid) = dimensions(raw_type);
    if has_srid {
        let _ = cursor.read_u32(little_endian)?;
    }
    match base_type {
        2 => Ok(vec![parse_line_string(
            cursor,
            little_endian,
            has_z,
            has_m,
        )?]),
        5 => {
            let count = cursor.read_u32(little_endian)? as usize;
            let mut lines = Vec::with_capacity(count);
            for _ in 0..count {
                lines.extend(parse_wkb_geometry(cursor)?);
            }
            Ok(lines)
        }
        value => {
            bail!("unsupported WKB geometry type {value}; expected LineString or MultiLineString")
        }
    }
}

fn dimensions(raw_type: u32) -> (u32, bool, bool, bool) {
    let ewkb_z = raw_type & 0x8000_0000 != 0;
    let ewkb_m = raw_type & 0x4000_0000 != 0;
    let has_srid = raw_type & 0x2000_0000 != 0;
    let mut geometry_type = raw_type & 0x0fff_ffff;
    let (iso_z, iso_m) = if geometry_type >= 3000 {
        geometry_type -= 3000;
        (true, true)
    } else if geometry_type >= 2000 {
        geometry_type -= 2000;
        (false, true)
    } else if geometry_type >= 1000 {
        geometry_type -= 1000;
        (true, false)
    } else {
        (false, false)
    };
    (geometry_type, ewkb_z || iso_z, ewkb_m || iso_m, has_srid)
}

fn parse_line_string(
    cursor: &mut Cursor<'_>,
    little_endian: bool,
    has_z: bool,
    has_m: bool,
) -> Result<Vec<[f64; 3]>> {
    let count = cursor.read_u32(little_endian)? as usize;
    let dimensions = 2 + usize::from(has_z) + usize::from(has_m);
    let required = count
        .checked_mul(dimensions)
        .and_then(|value| value.checked_mul(8))
        .ok_or_else(|| anyhow::anyhow!("WKB coordinate count overflows"))?;
    if required > cursor.remaining() {
        bail!("truncated WKB LineString coordinates");
    }
    let mut coordinates = Vec::with_capacity(count);
    for _ in 0..count {
        let x = cursor.read_f64(little_endian)?;
        let y = cursor.read_f64(little_endian)?;
        let z = if has_z {
            cursor.read_f64(little_endian)?
        } else {
            f64::NAN
        };
        if has_m {
            let _ = cursor.read_f64(little_endian)?;
        }
        coordinates.push([x, y, z]);
    }
    Ok(coordinates)
}

fn read_i32(bytes: &[u8], little_endian: bool) -> i32 {
    let bytes: [u8; 4] = bytes.try_into().expect("four-byte i32");
    if little_endian {
        i32::from_le_bytes(bytes)
    } else {
        i32::from_be_bytes(bytes)
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn read_u8(&mut self) -> Result<u8> {
        let Some(value) = self.bytes.get(self.offset).copied() else {
            bail!("truncated WKB");
        };
        self.offset += 1;
        Ok(value)
    }

    fn read_u32(&mut self, little_endian: bool) -> Result<u32> {
        let bytes = self.read_array::<4>()?;
        Ok(if little_endian {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    }

    fn read_f64(&mut self, little_endian: bool) -> Result<f64> {
        let bytes = self.read_array::<8>()?;
        Ok(if little_endian {
            f64::from_le_bytes(bytes)
        } else {
            f64::from_be_bytes(bytes)
        })
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self.offset.saturating_add(N);
        if end > self.bytes.len() {
            bail!("truncated WKB");
        }
        let bytes = self.bytes[self.offset..end]
            .try_into()
            .expect("checked length");
        self.offset = end;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_geopackage_linestring_z() {
        let mut blob = b"GP\0\x01".to_vec();
        blob.extend_from_slice(&4326_i32.to_le_bytes());
        blob.push(1);
        blob.extend_from_slice(&1002_u32.to_le_bytes());
        blob.extend_from_slice(&2_u32.to_le_bytes());
        for value in [114.0_f64, 22.0, 5.0, 114.1, 22.1, 9.0] {
            blob.extend_from_slice(&value.to_le_bytes());
        }

        let geometry = parse_geopackage_lines(&blob).expect("geometry");
        assert_eq!(geometry.srs_id, 4326);
        assert_eq!(
            geometry.lines,
            vec![vec![[114.0, 22.0, 5.0], [114.1, 22.1, 9.0]]]
        );
    }
}
