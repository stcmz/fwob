use fwob_core::{FieldType, Schema};

use crate::{Encoding, Result, V2Error};

pub fn encode_page_payload(
    schema: &Schema,
    encoding: Encoding,
    raw: &[u8],
    frame_count: usize,
) -> Result<Vec<u8>> {
    match encoding {
        Encoding::RowRawV1 => Ok(raw.to_vec()),
        Encoding::ColumnarBasicV1 => encode_columnar_basic(schema, raw, frame_count),
        Encoding::ColumnarDeltaV1 => encode_columnar_delta(schema, raw, frame_count),
    }
}

pub fn decode_page_payload(
    schema: &Schema,
    encoding: Encoding,
    encoded: &[u8],
    frame_count: usize,
) -> Result<Vec<u8>> {
    match encoding {
        Encoding::RowRawV1 => Ok(encoded.to_vec()),
        Encoding::ColumnarBasicV1 => decode_columnar_basic(schema, encoded, frame_count),
        Encoding::ColumnarDeltaV1 => decode_columnar_delta(schema, encoded, frame_count),
    }
}

fn encode_columnar_basic(schema: &Schema, raw: &[u8], frame_count: usize) -> Result<Vec<u8>> {
    let frame_len = schema.frame_len as usize;
    if raw.len() != frame_len * frame_count {
        return Err(V2Error::InvalidFileHeader);
    }

    let mut out = Vec::with_capacity(raw.len());
    for field in &schema.fields {
        let start = field.offset as usize;
        let end = start + field.length as usize;
        for frame_index in 0..frame_count {
            let frame_start = frame_index * frame_len;
            out.extend_from_slice(&raw[frame_start + start..frame_start + end]);
        }
    }
    Ok(out)
}

fn decode_columnar_basic(schema: &Schema, encoded: &[u8], frame_count: usize) -> Result<Vec<u8>> {
    let frame_len = schema.frame_len as usize;
    if encoded.len() != frame_len * frame_count {
        return Err(V2Error::InvalidPageHeader(0));
    }

    let mut raw = vec![0u8; encoded.len()];
    let mut cursor = 0usize;
    for field in &schema.fields {
        let start = field.offset as usize;
        let end = start + field.length as usize;
        let field_len = field.length as usize;
        for frame_index in 0..frame_count {
            let frame_start = frame_index * frame_len;
            raw[frame_start + start..frame_start + end]
                .copy_from_slice(&encoded[cursor..cursor + field_len]);
            cursor += field_len;
        }
    }
    Ok(raw)
}

fn encode_columnar_delta(schema: &Schema, raw: &[u8], frame_count: usize) -> Result<Vec<u8>> {
    let frame_len = schema.frame_len as usize;
    if raw.len() != frame_len * frame_count {
        return Err(V2Error::InvalidFileHeader);
    }

    let mut out = Vec::with_capacity(raw.len());
    for field in &schema.fields {
        let start = field.offset as usize;
        let end = start + field.length as usize;
        let field_len = field.length as usize;
        if is_delta_field(field.field_type, field_len) && frame_count > 0 {
            out.push(1);
            if field_len == 4 {
                encode_delta_i32_column(
                    field.field_type,
                    raw,
                    frame_len,
                    start,
                    frame_count,
                    &mut out,
                );
                continue;
            }
            let mut previous = 0i64;
            for frame_index in 0..frame_count {
                let frame_start = frame_index * frame_len;
                let value = read_int(
                    field.field_type,
                    &raw[frame_start + start..frame_start + end],
                );
                let stored = if frame_index == 0 {
                    value
                } else {
                    value.wrapping_sub(previous)
                };
                write_int(field.field_type, field_len, stored, &mut out);
                previous = value;
            }
        } else {
            out.push(0);
            for frame_index in 0..frame_count {
                let frame_start = frame_index * frame_len;
                out.extend_from_slice(&raw[frame_start + start..frame_start + end]);
            }
        }
    }
    Ok(out)
}

fn encode_delta_i32_column(
    field_type: FieldType,
    raw: &[u8],
    frame_len: usize,
    start: usize,
    frame_count: usize,
    out: &mut Vec<u8>,
) {
    let out_start = out.len();
    out.resize(out_start + frame_count * 4, 0);
    let column = &mut out[out_start..];
    if field_type == FieldType::SignedInteger {
        let mut previous = 0i32;
        for frame_index in 0..frame_count {
            let frame_start = frame_index * frame_len;
            let value = i32::from_le_bytes([
                raw[frame_start + start],
                raw[frame_start + start + 1],
                raw[frame_start + start + 2],
                raw[frame_start + start + 3],
            ]);
            let stored = if frame_index == 0 {
                value
            } else {
                value.wrapping_sub(previous)
            };
            column[frame_index * 4..frame_index * 4 + 4].copy_from_slice(&stored.to_le_bytes());
            previous = value;
        }
    } else {
        let mut previous = 0u32;
        for frame_index in 0..frame_count {
            let frame_start = frame_index * frame_len;
            let value = u32::from_le_bytes([
                raw[frame_start + start],
                raw[frame_start + start + 1],
                raw[frame_start + start + 2],
                raw[frame_start + start + 3],
            ]);
            let stored = if frame_index == 0 {
                value
            } else {
                value.wrapping_sub(previous)
            };
            column[frame_index * 4..frame_index * 4 + 4].copy_from_slice(&stored.to_le_bytes());
            previous = value;
        }
    }
}

fn decode_columnar_delta(schema: &Schema, encoded: &[u8], frame_count: usize) -> Result<Vec<u8>> {
    let frame_len = schema.frame_len as usize;
    let mut raw = vec![0u8; frame_len * frame_count];
    let mut cursor = 0usize;
    for field in &schema.fields {
        if cursor >= encoded.len() {
            return Err(V2Error::InvalidPageHeader(0));
        }
        let mode = encoded[cursor];
        cursor += 1;
        let start = field.offset as usize;
        let end = start + field.length as usize;
        let field_len = field.length as usize;
        match mode {
            0 => {
                let required = field_len * frame_count;
                if cursor + required > encoded.len() {
                    return Err(V2Error::InvalidPageHeader(0));
                }
                for frame_index in 0..frame_count {
                    let frame_start = frame_index * frame_len;
                    raw[frame_start + start..frame_start + end]
                        .copy_from_slice(&encoded[cursor..cursor + field_len]);
                    cursor += field_len;
                }
            }
            1 if is_delta_field(field.field_type, field_len) => {
                let required = field_len * frame_count;
                if cursor + required > encoded.len() {
                    return Err(V2Error::InvalidPageHeader(0));
                }
                let mut previous = 0i64;
                for frame_index in 0..frame_count {
                    let stored = read_int(field.field_type, &encoded[cursor..cursor + field_len]);
                    cursor += field_len;
                    let value = if frame_index == 0 {
                        stored
                    } else {
                        previous.wrapping_add(stored)
                    };
                    let frame_start = frame_index * frame_len;
                    write_int_to_slice(
                        field.field_type,
                        field_len,
                        value,
                        &mut raw[frame_start + start..frame_start + end],
                    );
                    previous = value;
                }
            }
            _ => return Err(V2Error::InvalidPageHeader(0)),
        }
    }
    if cursor != encoded.len() {
        return Err(V2Error::InvalidPageHeader(0));
    }
    Ok(raw)
}

fn is_delta_field(field_type: FieldType, field_len: usize) -> bool {
    matches!(
        field_type,
        FieldType::SignedInteger | FieldType::UnsignedInteger | FieldType::StringTableIndex
    ) && matches!(field_len, 1 | 2 | 4 | 8)
}

fn read_int(field_type: FieldType, bytes: &[u8]) -> i64 {
    match (field_type, bytes.len()) {
        (FieldType::SignedInteger, 1) => bytes[0] as i8 as i64,
        (FieldType::SignedInteger, 2) => i16::from_le_bytes(bytes.try_into().unwrap()) as i64,
        (FieldType::SignedInteger, 4) => i32::from_le_bytes(bytes.try_into().unwrap()) as i64,
        (FieldType::SignedInteger, 8) => i64::from_le_bytes(bytes.try_into().unwrap()),
        (_, 1) => bytes[0] as i64,
        (_, 2) => u16::from_le_bytes(bytes.try_into().unwrap()) as i64,
        (_, 4) => u32::from_le_bytes(bytes.try_into().unwrap()) as i64,
        (_, 8) => i64::from_le_bytes(bytes.try_into().unwrap()),
        _ => unreachable!("invalid integer field length"),
    }
}

fn write_int(field_type: FieldType, field_len: usize, value: i64, out: &mut Vec<u8>) {
    let start = out.len();
    out.resize(start + field_len, 0);
    write_int_to_slice(
        field_type,
        field_len,
        value,
        &mut out[start..start + field_len],
    );
}

fn write_int_to_slice(field_type: FieldType, field_len: usize, value: i64, out: &mut [u8]) {
    match (field_type, field_len) {
        (FieldType::SignedInteger, 1) => out[0] = value as i8 as u8,
        (FieldType::SignedInteger, 2) => out.copy_from_slice(&(value as i16).to_le_bytes()),
        (FieldType::SignedInteger, 4) => out.copy_from_slice(&(value as i32).to_le_bytes()),
        (FieldType::SignedInteger, 8) => out.copy_from_slice(&value.to_le_bytes()),
        (_, 1) => out[0] = value as u8,
        (_, 2) => out.copy_from_slice(&(value as u16).to_le_bytes()),
        (_, 4) => out.copy_from_slice(&(value as u32).to_le_bytes()),
        (_, 8) => out.copy_from_slice(&(value as u64).to_le_bytes()),
        _ => unreachable!("invalid integer field length"),
    }
}
