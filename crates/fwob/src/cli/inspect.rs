use super::*;

pub(super) fn inspect_v1(args: V1FileArgs) -> Result<()> {
    let mut reader = fwob_v1::Reader::open(&args.path, args.key_field_index)?;
    let header = reader.header();
    let physical_bytes = std::fs::metadata(&args.path)?.len();
    let data_bytes = header.frame_count * u64::from(header.frame_length);

    toml_section("file");
    toml_kv_str("format", "fwob-v1");
    toml_kv_str("title", &header.title);
    toml_kv_str("frame_type", &header.frame_type);
    toml_kv_num("key_field_index", args.key_field_index);

    println!();
    toml_section("storage");
    toml_kv_num("physical_bytes", physical_bytes);
    toml_kv_num("frame_count", header.frame_count);
    toml_kv_num("frame_length", header.frame_length);
    toml_kv_num("data_bytes", data_bytes);

    println!();
    toml_section("strings");
    toml_kv_num("string_count", header.string_count);
    toml_kv_num("string_table_length", header.string_table_length);
    toml_kv_num(
        "string_table_preserved_length",
        header.string_table_preserved_length,
    );

    println!();
    toml_section("schema");
    toml_kv_num("field_count", reader.schema().fields.len());
    for field in &reader.schema().fields {
        println!();
        toml_array_section("schema.fields");
        toml_kv_str("name", &field.name);
        toml_kv_str("type", field_type_name(field.field_type));
        toml_kv_num("length", field.length);
        toml_kv_num("offset", field.offset);
        if field.semantic != fwob_core::FieldSemantic::None {
            toml_kv_str("semantic", field_semantic_name(field.semantic));
        }
    }
    let preview = frame_preview_v1_text(&mut reader)?;
    if !preview.is_empty() {
        println!();
        toml_section("frames");
        toml_kv_multiline("preview", &preview);
    }
    Ok(())
}

pub(super) fn verify_v1(args: V1FileArgs) -> Result<()> {
    let report = fwob_v1::verify_file(&args.path, args.key_field_index)?;
    toml_section("verify");
    toml_kv_str("status", "ok");
    toml_kv_num("frame_count", comma_u64(report.frame_count));
    toml_kv_num("string_count", comma_u32(report.string_count));
    toml_kv_num("file_length", comma_u64(report.file_length));
    Ok(())
}

pub(super) fn inspect_v2(args: V2FileArgs) -> Result<()> {
    let mut reader = fwob_v2::Reader::open(&args.path)?;
    let header = reader.header().clone();
    let metadata = collect_v2_metadata(&args.path, &mut reader)?;

    toml_section("file");
    toml_kv_str("format", "fwob-v2");
    toml_kv_str("title", &header.title);
    toml_kv_str("frame_type", &header.schema.frame_type);
    toml_kv_num("key_field_index", header.key_field_index);

    println!();
    toml_section("storage");
    toml_kv_num("physical_bytes", metadata.physical_bytes);
    toml_kv_num("expected_physical_bytes", metadata.expected_physical_bytes);
    if metadata.physical_bytes != metadata.expected_physical_bytes {
        toml_kv_str(
            "physical_size_warning",
            &format!(
                "file has {} trailing_or_missing bytes relative to header",
                metadata.physical_bytes as i128 - metadata.expected_physical_bytes as i128
            ),
        );
    }
    toml_kv_num("frame_count", header.frame_count);
    toml_kv_num("string_count", header.string_table.len());

    println!();
    toml_section("pages");
    toml_kv_num("page_size", header.page_size);
    toml_kv_num("page_count", header.page_count);
    toml_kv_num(
        "page_payload_capacity_bytes",
        metadata.payload_capacity_per_page,
    );
    if header.page_count > 0 {
        toml_kv_num("min_frames_per_page", metadata.min_frames);
        toml_kv_num("max_frames_per_page", metadata.max_frames);
    }
    if let Some(first_key) = metadata.first_key {
        toml_kv_key("first_key", first_key);
    }
    if let Some(last_key) = metadata.last_key {
        toml_kv_key("last_key", last_key);
    }

    println!();
    toml_section("compression");
    toml_kv_num("compressed_payload_bytes", metadata.compressed_total);
    toml_kv_num("uncompressed_payload_bytes", metadata.uncompressed_total);
    toml_kv_num("padding_bytes", metadata.padding_bytes);
    if metadata.uncompressed_total > 0 {
        toml_kv_num(
            "payload_ratio",
            format!(
                "{:.4}",
                metadata.compressed_total as f64 / metadata.uncompressed_total as f64
            ),
        );
        toml_kv_num(
            "physical_ratio",
            format!(
                "{:.4}",
                metadata.physical_bytes as f64 / metadata.uncompressed_total as f64
            ),
        );
    }
    if metadata.payload_capacity_total > 0 {
        toml_kv_num(
            "page_payload_utilization",
            format!(
                "{:.4}",
                metadata.compressed_total as f64 / metadata.payload_capacity_total as f64
            ),
        );
    }
    print_page_codec_encoding_stats_toml(&metadata);

    println!();
    toml_section("schema");
    toml_kv_num("field_count", header.schema.fields.len());
    for field in &header.schema.fields {
        println!();
        toml_array_section("schema.fields");
        toml_kv_str("name", &field.name);
        toml_kv_str("type", field_type_name(field.field_type));
        toml_kv_num("length", field.length);
        toml_kv_num("offset", field.offset);
        if field.semantic != fwob_core::FieldSemantic::None {
            toml_kv_str("semantic", field_semantic_name(field.semantic));
        }
    }
    let preview = frame_preview_v2_text(&mut reader)?;
    if !preview.is_empty() {
        println!();
        toml_section("frames");
        toml_kv_multiline("preview", &preview);
    }
    Ok(())
}

pub(super) fn verify_v2(args: V2FileArgs) -> Result<()> {
    let mut reader = fwob_v2::Reader::open(&args.path)?;
    reader.verify()?;
    toml_section("verify");
    toml_kv_str("status", "ok");
    toml_kv_num("page_count", comma_u64(reader.header().page_count));
    toml_kv_num("frame_count", comma_u64(reader.header().frame_count));
    Ok(())
}

fn field_type_name(field_type: FieldType) -> &'static str {
    match field_type {
        FieldType::SignedInteger => "signed-integer",
        FieldType::UnsignedInteger => "unsigned-integer",
        FieldType::FloatingPoint => "floating-point",
        FieldType::Utf8String => "utf8-string",
        FieldType::StringTableIndex => "string-table-index",
    }
}

pub(super) fn field_semantic_name(semantic: fwob_core::FieldSemantic) -> &'static str {
    match semantic {
        fwob_core::FieldSemantic::None => "none",
        fwob_core::FieldSemantic::UnixTimestamp(fwob_core::TimestampUnit::Seconds) => {
            "unix-seconds"
        }
        fwob_core::FieldSemantic::UnixTimestamp(fwob_core::TimestampUnit::Milliseconds) => {
            "unix-milliseconds"
        }
        fwob_core::FieldSemantic::UnixTimestamp(fwob_core::TimestampUnit::Microseconds) => {
            "unix-microseconds"
        }
        fwob_core::FieldSemantic::UnixTimestamp(fwob_core::TimestampUnit::Nanoseconds) => {
            "unix-nanoseconds"
        }
    }
}

fn frame_preview_v1_text(reader: &mut fwob_v1::Reader<std::fs::File>) -> Result<String> {
    let frame_count = reader.header().frame_count;
    let schema = reader.schema().clone();
    let indices = preview_indices(frame_count);
    if indices.is_empty() {
        return Ok(String::new());
    }
    let mut rows = Vec::new();
    for item in indices {
        match item {
            PreviewIndex::Frame(index) => {
                let raw = reader.read_raw_frames_chunk(index, 1)?;
                rows.push(PreviewRow::Frame(index, raw));
            }
            PreviewIndex::Ellipsis => rows.push(PreviewRow::Ellipsis),
        }
    }
    Ok(format_frame_preview_rows(&schema, &rows))
}

fn frame_preview_v2_text(reader: &mut fwob_v2::Reader<std::fs::File>) -> Result<String> {
    let frame_count = reader.header().frame_count;
    let schema = reader.header().schema.clone();
    let indices = preview_indices(frame_count);
    if indices.is_empty() {
        return Ok(String::new());
    }
    let mut rows = Vec::new();
    for item in indices {
        match item {
            PreviewIndex::Frame(index) => {
                let raw = read_v2_frame_at(reader, index)?;
                rows.push(PreviewRow::Frame(index, raw));
            }
            PreviewIndex::Ellipsis => rows.push(PreviewRow::Ellipsis),
        }
    }
    Ok(format_frame_preview_rows(&schema, &rows))
}

pub(super) enum PreviewIndex {
    Frame(u64),
    Ellipsis,
}

pub(super) enum PreviewRow {
    Frame(u64, Vec<u8>),
    Ellipsis,
}

pub(super) fn preview_indices(frame_count: u64) -> Vec<PreviewIndex> {
    let preview = FRAME_PREVIEW_COUNT as u64;
    if frame_count == 0 {
        return Vec::new();
    }
    if frame_count <= preview * 2 {
        return (0..frame_count).map(PreviewIndex::Frame).collect();
    }
    let mut out = Vec::with_capacity(FRAME_PREVIEW_COUNT * 2 + 1);
    for index in 0..preview {
        out.push(PreviewIndex::Frame(index));
    }
    out.push(PreviewIndex::Ellipsis);
    for index in frame_count - preview..frame_count {
        out.push(PreviewIndex::Frame(index));
    }
    out
}

fn read_v2_frame_at(reader: &mut fwob_v2::Reader<std::fs::File>, index: u64) -> Result<Vec<u8>> {
    let mut base = 0u64;
    for page_index in 0..reader.header().page_count {
        let page = reader.read_page_header(page_index)?;
        let page_frames = u64::from(page.frame_count);
        if index < base + page_frames {
            let raw = reader.read_page_raw_frames(page_index)?;
            let frame_len = reader.header().schema.frame_len as usize;
            let offset = (index - base) as usize * frame_len;
            return Ok(raw[offset..offset + frame_len].to_vec());
        }
        base += page_frames;
    }
    bail!("frame index {} is out of range", index);
}

pub(super) fn format_frame_preview_rows(schema: &Schema, rows: &[PreviewRow]) -> String {
    let mut table = Vec::with_capacity(rows.len() + 1);
    let mut header = Vec::with_capacity(schema.fields.len() + 1);
    header.push("index".to_string());
    header.extend(schema.fields.iter().map(|field| field.name.clone()));
    table.push(header);

    let mut right_align = Vec::with_capacity(schema.fields.len() + 1);
    right_align.push(true);
    right_align.extend(
        schema
            .fields
            .iter()
            .map(|field| field.field_type != FieldType::Utf8String),
    );

    for row in rows {
        match row {
            PreviewRow::Ellipsis => {
                table.push(vec!["...".to_string(); schema.fields.len() + 1]);
            }
            PreviewRow::Frame(index, bytes) => {
                let mut values = Vec::with_capacity(schema.fields.len() + 1);
                values.push(comma_u64(*index));
                for field in &schema.fields {
                    values.push(format_field_value(field, bytes));
                }
                table.push(values);
            }
        }
    }

    let mut widths = vec![0usize; schema.fields.len() + 1];
    for row in &table {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.chars().count());
        }
    }

    let mut out = String::new();
    for (row_index, row) in table.iter().enumerate() {
        for (column_index, value) in row.iter().enumerate() {
            if column_index > 0 {
                out.push_str("  ");
            }
            let align_right = row_index > 0 && right_align[column_index];
            if align_right {
                out.push_str(&format!("{value:>width$}", width = widths[column_index]));
            } else {
                out.push_str(&format!("{value:<width$}", width = widths[column_index]));
            }
        }
        out.push('\n');
    }
    out
}

fn format_field_value(field: &fwob_core::Field, frame: &[u8]) -> String {
    let start = field.offset as usize;
    let end = start + field.length as usize;
    let bytes = &frame[start..end];
    match field.field_type {
        FieldType::SignedInteger => format_signed(bytes),
        FieldType::UnsignedInteger | FieldType::StringTableIndex => format_unsigned(bytes),
        FieldType::FloatingPoint => match bytes.len() {
            4 => format!("{:.6}", f32::from_le_bytes(bytes.try_into().unwrap())),
            8 => format!("{:.6}", f64::from_le_bytes(bytes.try_into().unwrap())),
            _ => format_hex(bytes),
        },
        FieldType::Utf8String => String::from_utf8_lossy(bytes)
            .trim_end_matches('\0')
            .trim_end()
            .to_string(),
    }
}

fn format_signed(bytes: &[u8]) -> String {
    let value = match bytes.len() {
        1 => bytes[0] as i8 as i128,
        2 => i16::from_le_bytes(bytes.try_into().unwrap()) as i128,
        4 => i32::from_le_bytes(bytes.try_into().unwrap()) as i128,
        8 => i64::from_le_bytes(bytes.try_into().unwrap()) as i128,
        _ => return format_hex(bytes),
    };
    comma_i128(value)
}

fn format_unsigned(bytes: &[u8]) -> String {
    let value = match bytes.len() {
        1 => bytes[0] as u128,
        2 => u16::from_le_bytes(bytes.try_into().unwrap()) as u128,
        4 => u32::from_le_bytes(bytes.try_into().unwrap()) as u128,
        8 => u64::from_le_bytes(bytes.try_into().unwrap()) as u128,
        _ => return format_hex(bytes),
    };
    comma_u128(value)
}

fn format_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2 + 2);
    out.push_str("0x");
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
