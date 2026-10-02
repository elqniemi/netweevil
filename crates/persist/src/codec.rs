//! Compact, lossless column encodings for bundle sections.
//!
//! Every encoded column starts with a one-byte tag and a LEB128 element
//! count, followed by a tag-specific payload. Writers try the applicable
//! encodings and keep the smallest; readers dispatch on the tag. All
//! encodings round-trip bit-exactly: a float column only uses fixed-point
//! when every value decodes back to the identical `f64`.

use anyhow::{Result, bail};
use bytemuck::Pod;

const TAG_RAW: u8 = 0;
const TAG_CONSTANT: u8 = 1;
const TAG_IDENTITY: u8 = 2;
const TAG_DELTA_VARINT: u8 = 3;
const TAG_VARINT: u8 = 4;
/// `f64` stored as `round(x * 1e7)` deltas, decoded as `k as f64 * 1e-7`
/// (the OSM PBF decimicro-degree convention, so OSM coordinates are exact).
const TAG_FIXED_E7_DELTA: u8 = 5;
/// `f64` values that are all exactly representable as `f32`.
const TAG_F32: u8 = 6;

const FIXED_E7_SCALE: f64 = 1e7;
const FIXED_E7_STEP: f64 = 1e-7;

// --- LEB128 primitives ----------------------------------------------------

pub(crate) fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn varint_len(mut value: u64) -> usize {
    let mut len = 1;
    while value >= 0x80 {
        value >>= 7;
        len += 1;
    }
    len
}

const fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

const fn unzigzag(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}

/// Sequential reader over a byte slice.
pub(crate) struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    #[inline]
    pub(crate) fn varint(&mut self) -> Result<u64> {
        // Most column entries are single bytes.
        if let Some(&byte) = self.bytes.get(self.offset)
            && byte < 0x80
        {
            self.offset += 1;
            return Ok(u64::from(byte));
        }
        self.multi_byte_varint()
    }

    fn multi_byte_varint(&mut self) -> Result<u64> {
        let mut value = 0_u64;
        let mut shift = 0_u32;
        loop {
            let Some(&byte) = self.bytes.get(self.offset) else {
                bail!("truncated varint in bundle column");
            };
            self.offset += 1;
            if shift >= 64 {
                bail!("varint overflow in bundle column");
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    fn byte(&mut self) -> Result<u8> {
        let Some(&byte) = self.bytes.get(self.offset) else {
            bail!("truncated bundle column");
        };
        self.offset += 1;
        Ok(byte)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len());
        let Some(end) = end else {
            bail!("truncated bundle column payload");
        };
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn count(&mut self) -> Result<usize> {
        let count = self.varint()?;
        // Every encoding spends at least... zero bytes per element for
        // constants, so bound by a sane maximum instead of the payload.
        if count > u64::from(u32::MAX) * 16 {
            bail!("implausible element count in bundle column");
        }
        Ok(count as usize)
    }

    pub(crate) fn finish(&self) -> Result<()> {
        if self.offset != self.bytes.len() {
            bail!("trailing bytes in bundle column");
        }
        Ok(())
    }
}

fn header(tag: u8, count: usize, capacity: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(capacity + 10);
    out.push(tag);
    put_varint(&mut out, count as u64);
    out
}

fn raw_column<T: Pod>(values: &[T]) -> Vec<u8> {
    let bytes: &[u8] = bytemuck::cast_slice(values);
    let mut out = header(TAG_RAW, values.len(), bytes.len());
    out.extend_from_slice(bytes);
    out
}

fn read_raw<T: Pod>(cursor: &mut Cursor<'_>, count: usize) -> Result<Vec<T>> {
    let Some(len) = count.checked_mul(size_of::<T>()) else {
        bail!("bundle column length overflow");
    };
    let bytes = cursor.take(len)?;
    Ok(bytemuck::allocation::pod_collect_to_vec(bytes))
}

// --- Integer columns ------------------------------------------------------

/// Encodes signed 64-bit integers, choosing among constant, identity,
/// zigzag-delta varint, plain varint, and raw storage.
pub(crate) fn encode_i64s(values: &[i64]) -> Vec<u8> {
    if let Some(&first) = values.first()
        && values.iter().all(|value| *value == first)
    {
        let mut out = header(TAG_CONSTANT, values.len(), 10);
        put_varint(&mut out, zigzag(first));
        return out;
    }
    if values
        .iter()
        .enumerate()
        .all(|(index, value)| *value == index as i64)
    {
        return header(TAG_IDENTITY, values.len(), 0);
    }
    let mut delta_len = 0_usize;
    let mut plain_len = 0_usize;
    let mut previous = 0_i64;
    for &value in values {
        delta_len += varint_len(zigzag(value.wrapping_sub(previous)));
        plain_len += varint_len(zigzag(value));
        previous = value;
    }
    let raw_len = size_of_val(values);
    if raw_len <= delta_len.min(plain_len) {
        return raw_column(values);
    }
    if delta_len <= plain_len {
        let mut out = header(TAG_DELTA_VARINT, values.len(), delta_len);
        let mut previous = 0_i64;
        for &value in values {
            put_varint(&mut out, zigzag(value.wrapping_sub(previous)));
            previous = value;
        }
        out
    } else {
        let mut out = header(TAG_VARINT, values.len(), plain_len);
        for &value in values {
            put_varint(&mut out, zigzag(value));
        }
        out
    }
}

pub(crate) fn decode_i64s(bytes: &[u8]) -> Result<Vec<i64>> {
    let mut cursor = Cursor::new(bytes);
    let tag = cursor.byte()?;
    let count = cursor.count()?;
    let values = match tag {
        TAG_RAW => read_raw::<i64>(&mut cursor, count)?,
        TAG_CONSTANT => vec![unzigzag(cursor.varint()?); count],
        TAG_IDENTITY => (0..count as i64).collect(),
        TAG_DELTA_VARINT => {
            let mut values = Vec::with_capacity(count);
            let mut previous = 0_i64;
            for _ in 0..count {
                previous = previous.wrapping_add(unzigzag(cursor.varint()?));
                values.push(previous);
            }
            values
        }
        TAG_VARINT => {
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                values.push(unzigzag(cursor.varint()?));
            }
            values
        }
        other => bail!("unsupported i64 column encoding {other}"),
    };
    cursor.finish()?;
    Ok(values)
}

/// Encodes unsigned 32-bit integers. `u32::MAX` sentinels are common
/// (absent ids), so deltas are taken in `i64` and zigzagged.
pub(crate) fn encode_u32s(values: &[u32]) -> Vec<u8> {
    if let Some(&first) = values.first()
        && values.iter().all(|value| *value == first)
    {
        let mut out = header(TAG_CONSTANT, values.len(), 5);
        put_varint(&mut out, u64::from(first));
        return out;
    }
    if values
        .iter()
        .enumerate()
        .all(|(index, value)| *value as usize == index)
    {
        return header(TAG_IDENTITY, values.len(), 0);
    }
    let mut delta_len = 0_usize;
    let mut plain_len = 0_usize;
    let mut previous = 0_i64;
    for &value in values {
        let value = i64::from(value);
        delta_len += varint_len(zigzag(value - previous));
        plain_len += varint_len(value as u64);
        previous = value;
    }
    let raw_len = size_of_val(values);
    if raw_len <= delta_len.min(plain_len) {
        return raw_column(values);
    }
    if delta_len <= plain_len {
        let mut out = header(TAG_DELTA_VARINT, values.len(), delta_len);
        let mut previous = 0_i64;
        for &value in values {
            let value = i64::from(value);
            put_varint(&mut out, zigzag(value - previous));
            previous = value;
        }
        out
    } else {
        let mut out = header(TAG_VARINT, values.len(), plain_len);
        for &value in values {
            put_varint(&mut out, u64::from(value));
        }
        out
    }
}

pub(crate) fn decode_u32s(bytes: &[u8]) -> Result<Vec<u32>> {
    let mut cursor = Cursor::new(bytes);
    let tag = cursor.byte()?;
    let count = cursor.count()?;
    let narrow = |value: i64| -> Result<u32> {
        u32::try_from(value).map_err(|_| anyhow::anyhow!("u32 column value out of range"))
    };
    let values = match tag {
        TAG_RAW => read_raw::<u32>(&mut cursor, count)?,
        TAG_CONSTANT => vec![narrow(cursor.varint()? as i64)?; count],
        TAG_IDENTITY => {
            if count > u32::MAX as usize + 1 {
                bail!("identity u32 column too long");
            }
            (0..count).map(|index| index as u32).collect()
        }
        TAG_DELTA_VARINT => {
            let mut values = Vec::with_capacity(count);
            let mut previous = 0_i64;
            for _ in 0..count {
                previous += unzigzag(cursor.varint()?);
                values.push(narrow(previous)?);
            }
            values
        }
        TAG_VARINT => {
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                values.push(narrow(cursor.varint()? as i64)?);
            }
            values
        }
        other => bail!("unsupported u32 column encoding {other}"),
    };
    cursor.finish()?;
    Ok(values)
}

/// Encodes CSR head ids whose rows are each sorted ascending. The first
/// head of a row is stored relative to the row's tail id and the rest as
/// gaps, which keeps most entries to one or two bytes on road networks
/// whose ids carry spatial locality. Falls back to [`encode_u32s`] when a
/// row is unsorted.
pub(crate) fn encode_sorted_rows(first_out: &[u32], heads: &[u32]) -> Vec<u8> {
    let sorted = first_out.windows(2).all(|range| {
        heads[range[0] as usize..range[1] as usize]
            .windows(2)
            .all(|pair| pair[0] <= pair[1])
    });
    if !sorted || first_out.last().copied().unwrap_or_default() as usize != heads.len() {
        let mut out = vec![0_u8];
        out.extend(encode_u32s(heads));
        return out;
    }
    let mut out = Vec::with_capacity(heads.len() * 2 + 16);
    out.push(1_u8);
    put_varint(&mut out, heads.len() as u64);
    for (tail, range) in first_out.windows(2).enumerate() {
        let mut previous = tail as i64;
        for (offset, &head) in heads[range[0] as usize..range[1] as usize]
            .iter()
            .enumerate()
        {
            let head = i64::from(head);
            if offset == 0 {
                put_varint(&mut out, zigzag(head - previous));
            } else {
                put_varint(&mut out, (head - previous) as u64);
            }
            previous = head;
        }
    }
    out
}

pub(crate) fn decode_sorted_rows(first_out: &[u32], bytes: &[u8]) -> Result<Vec<u32>> {
    let Some((&mode, rest)) = bytes.split_first() else {
        bail!("empty CSR head column");
    };
    let heads = match mode {
        0 => decode_u32s(rest)?,
        1 => {
            let mut cursor = Cursor::new(rest);
            let count = cursor.count()?;
            if first_out.last().copied().unwrap_or_default() as usize != count {
                bail!("CSR head column does not match its offsets");
            }
            let mut heads = Vec::with_capacity(count);
            for (tail, range) in first_out.windows(2).enumerate() {
                if range[1] < range[0] {
                    bail!("CSR offsets are not monotonic");
                }
                let mut previous = tail as i64;
                for offset in 0..(range[1] - range[0]) {
                    let raw = cursor.varint()?;
                    let head = if offset == 0 {
                        previous + unzigzag(raw)
                    } else {
                        previous + raw as i64
                    };
                    heads.push(
                        u32::try_from(head)
                            .map_err(|_| anyhow::anyhow!("CSR head out of range"))?,
                    );
                    previous = head;
                }
            }
            cursor.finish()?;
            heads
        }
        other => bail!("unsupported CSR head encoding {other}"),
    };
    if first_out.last().copied().unwrap_or_default() as usize != heads.len() {
        bail!("CSR head column does not match its offsets");
    }
    Ok(heads)
}

// --- Floating-point columns -----------------------------------------------

fn fixed_e7(value: f64) -> Option<i64> {
    if !value.is_finite() || value.abs() > 1e11 {
        return None;
    }
    let scaled = (value * FIXED_E7_SCALE).round() as i64;
    ((scaled as f64 * FIXED_E7_STEP).to_bits() == value.to_bits()).then_some(scaled)
}

/// Encodes `f64` values losslessly: constant (bitwise), fixed-point 1e-7
/// deltas when every value round-trips exactly, `f32` when every value is
/// exactly representable, otherwise raw.
pub(crate) fn encode_f64s(values: &[f64]) -> Vec<u8> {
    if let Some(&first) = values.first()
        && values
            .iter()
            .all(|value| value.to_bits() == first.to_bits())
    {
        let mut out = header(TAG_CONSTANT, values.len(), 8);
        out.extend_from_slice(&first.to_bits().to_le_bytes());
        return out;
    }
    let fixed: Option<Vec<i64>> = values.iter().map(|value| fixed_e7(*value)).collect();
    if let Some(fixed) = fixed {
        let mut out = header(TAG_FIXED_E7_DELTA, values.len(), values.len() * 2);
        let mut previous = 0_i64;
        for value in fixed {
            put_varint(&mut out, zigzag(value - previous));
            previous = value;
        }
        if out.len() < size_of_val(values) {
            return out;
        }
    }
    if values
        .iter()
        .all(|value| f64::from(*value as f32).to_bits() == value.to_bits())
    {
        let narrowed: Vec<f32> = values.iter().map(|value| *value as f32).collect();
        let bytes: &[u8] = bytemuck::cast_slice(&narrowed);
        let mut out = header(TAG_F32, values.len(), bytes.len());
        out.extend_from_slice(bytes);
        return out;
    }
    raw_column(values)
}

pub(crate) fn decode_f64s(bytes: &[u8]) -> Result<Vec<f64>> {
    let mut cursor = Cursor::new(bytes);
    let tag = cursor.byte()?;
    let count = cursor.count()?;
    let values = match tag {
        TAG_RAW => read_raw::<f64>(&mut cursor, count)?,
        TAG_CONSTANT => {
            let bits = u64::from_le_bytes(cursor.take(8)?.try_into().unwrap());
            vec![f64::from_bits(bits); count]
        }
        TAG_FIXED_E7_DELTA => {
            let mut values = Vec::with_capacity(count);
            let mut previous = 0_i64;
            for _ in 0..count {
                previous += unzigzag(cursor.varint()?);
                values.push(previous as f64 * FIXED_E7_STEP);
            }
            values
        }
        TAG_F32 => read_raw::<f32>(&mut cursor, count)?
            .into_iter()
            .map(f64::from)
            .collect(),
        other => bail!("unsupported f64 column encoding {other}"),
    };
    cursor.finish()?;
    Ok(values)
}

/// Encodes `f32` values: constant (bitwise) or raw.
pub(crate) fn encode_f32s(values: &[f32]) -> Vec<u8> {
    if let Some(&first) = values.first()
        && values
            .iter()
            .all(|value| value.to_bits() == first.to_bits())
    {
        let mut out = header(TAG_CONSTANT, values.len(), 4);
        out.extend_from_slice(&first.to_bits().to_le_bytes());
        return out;
    }
    raw_column(values)
}

pub(crate) fn decode_f32s(bytes: &[u8]) -> Result<Vec<f32>> {
    let mut cursor = Cursor::new(bytes);
    let tag = cursor.byte()?;
    let count = cursor.count()?;
    let values = match tag {
        TAG_RAW => read_raw::<f32>(&mut cursor, count)?,
        TAG_CONSTANT => {
            let bits = u32::from_le_bytes(cursor.take(4)?.try_into().unwrap());
            vec![f32::from_bits(bits); count]
        }
        other => bail!("unsupported f32 column encoding {other}"),
    };
    cursor.finish()?;
    Ok(values)
}

// --- String tables --------------------------------------------------------

/// Encodes strings with front coding: each entry stores the byte length of
/// the prefix it shares with the previous entry (cut back to a character
/// boundary) and its remaining suffix. Sorted tables such as street names
/// share long prefixes.
pub(crate) fn encode_strings(values: &[String]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.iter().map(String::len).sum::<usize>() + 16);
    put_varint(&mut out, values.len() as u64);
    let mut previous = "";
    for value in values {
        let mut shared = previous
            .bytes()
            .zip(value.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while !value.is_char_boundary(shared) {
            shared -= 1;
        }
        put_varint(&mut out, shared as u64);
        put_varint(&mut out, (value.len() - shared) as u64);
        out.extend_from_slice(&value.as_bytes()[shared..]);
        previous = value;
    }
    out
}

pub(crate) fn decode_strings(bytes: &[u8]) -> Result<Vec<String>> {
    let mut cursor = Cursor::new(bytes);
    let count = cursor.count()?;
    let mut values: Vec<String> = Vec::with_capacity(count.min(bytes.len()));
    for _ in 0..count {
        let shared = cursor.varint()? as usize;
        let suffix_len = cursor.varint()? as usize;
        let previous = values.last().map_or("", String::as_str);
        if shared > previous.len() || !previous.is_char_boundary(shared) {
            bail!("string table prefix is out of range");
        }
        let suffix = std::str::from_utf8(cursor.take(suffix_len)?)
            .map_err(|_| anyhow::anyhow!("string table entry is not UTF-8"))?;
        let mut value = String::with_capacity(shared + suffix.len());
        value.push_str(&previous[..shared]);
        value.push_str(suffix);
        values.push(value);
    }
    cursor.finish()?;
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_columns_round_trip_through_every_encoding() {
        let cases: Vec<Vec<u32>> = vec![
            vec![],
            vec![7; 5],
            (0..100).collect(),
            vec![5, 4, 3, u32::MAX, 0, 9],
            (0..1000).map(|value| value * 3 + 1_000_000).collect(),
            (0_u32..64)
                .map(|value| value.wrapping_mul(2_654_435_761))
                .collect(),
        ];
        for values in cases {
            assert_eq!(decode_u32s(&encode_u32s(&values)).unwrap(), values);
        }
        for values in [
            vec![-5_i64, i64::MAX, i64::MIN, 0],
            vec![10_000_000_000, 10_000_000_001, 9_999_999_999],
            vec![3; 4],
        ] {
            assert_eq!(decode_i64s(&encode_i64s(&values)).unwrap(), values);
        }
    }

    #[test]
    fn sorted_rows_round_trip_and_reject_mismatched_offsets() {
        let first_out = vec![0_u32, 2, 2, 5];
        let heads = vec![3_u32, 9, 0, 1, 4_000_000_000];
        let encoded = encode_sorted_rows(&first_out, &heads);
        assert_eq!(decode_sorted_rows(&first_out, &encoded).unwrap(), heads);
        assert!(decode_sorted_rows(&[0, 1], &encoded).is_err());
        let unsorted = vec![9_u32, 3, 0, 1, 2];
        let encoded = encode_sorted_rows(&first_out, &unsorted);
        assert_eq!(decode_sorted_rows(&first_out, &encoded).unwrap(), unsorted);
    }

    #[test]
    fn string_tables_round_trip_with_shared_multibyte_prefixes() {
        let cases: Vec<Vec<String>> = vec![
            vec![],
            vec![String::new(), String::new()],
            ["Hereweg", "Herestraat", "Herestraat", "Zuiderdiep", ""]
                .map(String::from)
                .to_vec(),
            // "é" and "è" share their first UTF-8 byte; the shared prefix
            // must stop before it.
            ["Rue é", "Rue è", "彌敦道", "彌敦道北", "A7;A28"]
                .map(String::from)
                .to_vec(),
        ];
        for values in cases {
            assert_eq!(decode_strings(&encode_strings(&values)).unwrap(), values);
        }
        let mut corrupt = encode_strings(&["ab".into(), "ac".into()]);
        corrupt[5] = 9; // shared prefix longer than "ab"
        assert!(decode_strings(&corrupt).is_err());
    }

    #[test]
    fn float_columns_round_trip_bit_exactly() {
        let osm: Vec<f64> = [65_665_123_i64, 65_665_200, -1_234_567_890]
            .iter()
            .map(|value| *value as f64 * 1e-7)
            .collect();
        let encoded = encode_f64s(&osm);
        assert_eq!(encoded[0], TAG_FIXED_E7_DELTA);
        let cases = vec![
            osm,
            vec![f64::NAN; 3],
            vec![0.1, 0.2, f64::NAN, f64::INFINITY],
            vec![1.5, 2.25, -0.0],
            vec![114.123_456_789_123, 22.1],
        ];
        for values in cases {
            let decoded = decode_f64s(&encode_f64s(&values)).unwrap();
            assert_eq!(
                decoded.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                values.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
        }
        let floats = vec![0.0_f32, 1.5, f32::NAN];
        let decoded = decode_f32s(&encode_f32s(&floats)).unwrap();
        assert_eq!(decoded[1], 1.5);
        assert!(decoded[2].is_nan());
    }
}
