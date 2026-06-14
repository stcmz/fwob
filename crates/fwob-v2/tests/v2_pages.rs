use std::io::Cursor;

use fwob_core::{Field, FieldType, Key, Schema};
use fwob_v2::{
    Codec, CodecSelection, Encoding, EncodingSelection, Reader, Writer, WriterOptions,
    PAGE_HEADER_LEN,
};
use tempfile::tempdir;

fn tick_schema() -> Schema {
    Schema::new(
        "Tick",
        vec![
            Field::new("Time", FieldType::SignedInteger, 4, 0),
            Field::new("Value", FieldType::FloatingPoint, 8, 4),
            Field::new("Str", FieldType::Utf8String, 4, 12),
        ],
        0,
    )
    .unwrap()
}

fn tick(time: i32, value: f64, text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&time.to_le_bytes());
    out.extend_from_slice(&value.to_le_bytes());
    let mut str_bytes = [b' '; 4];
    str_bytes[..text.len()].copy_from_slice(text.as_bytes());
    out.extend_from_slice(&str_bytes);
    out
}

fn short_tick(time: i32, price: u32, size: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&time.to_le_bytes());
    out.extend_from_slice(&price.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out
}

#[test]
fn page_header_is_64_bytes() {
    assert_eq!(PAGE_HEADER_LEN, 64);
}

#[test]
fn page_headers_store_contiguous_first_frame_indexes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("indexes.fwob");
    let schema = tick_schema();
    let mut options = WriterOptions::new("indexes");
    options.page_size = 1024;
    options.codec = Codec::None;
    options.codec_selection = CodecSelection::Fixed(Codec::None);
    options.encoding = Encoding::RowRawV1;
    options.encoding_selection = EncodingSelection::Fixed(Encoding::RowRawV1);
    let mut writer = Writer::create(&path, schema, options).unwrap();
    for index in 0..300 {
        writer.append_frame(&tick(index, index as f64, "")).unwrap();
    }
    writer.finish().unwrap();

    let mut reader = Reader::open(&path).unwrap();
    let mut expected = 0u64;
    for page_index in 0..reader.header().page_count {
        let page = reader.read_page_header(page_index).unwrap();
        assert_eq!(page.first_frame_index, expected);
        expected += u64::from(page.frame_count);
    }
    assert_eq!(expected, 300);
}

#[test]
fn boundary_frames_and_keys_are_read_directly() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("boundaries.fwob");
    let mut options = WriterOptions::new("boundaries");
    options.page_size = 1024;
    let mut writer = Writer::create(&path, tick_schema(), options).unwrap();
    for index in 0..300 {
        writer.append_frame(&tick(index, index as f64, "")).unwrap();
    }
    writer.finish().unwrap();

    let mut reader = Reader::open(path).unwrap();
    assert_eq!(reader.first_key().unwrap(), Some(Key::I32(0)));
    assert_eq!(reader.last_key().unwrap(), Some(Key::I32(299)));
    assert_eq!(
        reader.first_frame().unwrap().unwrap().bytes(),
        tick(0, 0.0, "")
    );
    assert_eq!(
        reader.last_frame().unwrap().unwrap().bytes(),
        tick(299, 299.0, "")
    );
}

#[test]
fn writer_defaults_match_cli_parameter_spec() {
    let options = WriterOptions::new("Defaults");
    assert_eq!(options.page_size, fwob_v2::DEFAULT_PAGE_SIZE);
    assert_eq!(options.codec, fwob_v2::DEFAULT_CODEC);
    assert_eq!(options.zstd_level, fwob_v2::DEFAULT_ZSTD_LEVEL);
    assert_eq!(options.encoding, fwob_v2::DEFAULT_ENCODING);
    assert_eq!(options.page_packing, fwob_v2::DEFAULT_PAGE_PACKING);
    assert_eq!(fwob_v2::DEFAULT_PAGE_SIZE, 512 * 1024);
    assert_eq!(fwob_v2::DEFAULT_CODEC, Codec::Zstd);
    assert_eq!(fwob_v2::DEFAULT_ZSTD_LEVEL, 6);
    assert_eq!(fwob_v2::DEFAULT_ENCODING, Encoding::ColumnarBasicV1);
    assert_eq!(
        fwob_v2::DEFAULT_PAGE_PACKING,
        fwob_v2::PagePacking::EstimateShrink
    );
    assert!(!options.compress_partial_page);
}

#[test]
fn v2_writer_enforces_page_size_bounds_and_nonempty_title() {
    for page_size in [fwob_v2::MIN_PAGE_SIZE - 1, fwob_v2::MAX_PAGE_SIZE + 1] {
        let mut options = WriterOptions::new("bounds");
        options.page_size = page_size;
        assert!(Writer::new(Cursor::new(Vec::new()), tick_schema(), options).is_err());
    }

    let options = WriterOptions::new("");
    assert!(Writer::new(Cursor::new(Vec::new()), tick_schema(), options).is_err());
}

#[test]
fn v2_reader_rejects_invalid_utf8_inside_bounded_header() {
    let mut cursor = Cursor::new(Vec::new());
    {
        let writer = Writer::new(&mut cursor, tick_schema(), WriterOptions::new("Title")).unwrap();
        writer.finish().unwrap();
    }
    cursor.get_mut()[29] = 0xff;
    cursor.set_position(0);
    assert!(Reader::new(cursor).is_err());
}

#[test]
fn writes_fixed_pages_and_reads_ranges() {
    let schema = tick_schema();
    let mut options = WriterOptions::new("HelloFwob");
    options.page_size = 1024;
    options.codec = Codec::None;
    options.codec_selection = CodecSelection::Fixed(Codec::None);
    options.encoding = Encoding::RowRawV1;
    options.string_table = vec!["mystr".to_string()];

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema, options).unwrap();
        for i in 0..100 {
            writer.append_frame(&tick(i, i as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }

    cursor.set_position(0);
    let mut reader = Reader::new(cursor).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, 100);
    assert!(reader.header().page_count > 1);
    let frames = reader.frames_between(Key::I32(10), Key::I32(20)).unwrap();
    assert_eq!(frames.len(), 11);
}

#[test]
fn columnar_basic_pages_roundtrip_and_read_ranges() {
    let schema = tick_schema();
    let mut options = WriterOptions::new("Columnar");
    options.page_size = 1024;
    options.codec = Codec::Zstd;
    options.codec_selection = CodecSelection::Fixed(Codec::Zstd);
    options.encoding = Encoding::ColumnarBasicV1;

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema, options).unwrap();
        for i in 0..100 {
            writer.append_frame(&tick(i, i as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }

    cursor.set_position(0);
    let mut reader = Reader::new(cursor).unwrap();
    reader.verify().unwrap();
    assert_eq!(
        reader.read_page_header(0).unwrap().encoding,
        Encoding::ColumnarBasicV1
    );
    let frames = reader.frames_between(Key::I32(10), Key::I32(20)).unwrap();
    assert_eq!(frames.len(), 11);
    assert_eq!(frames[0].bytes(), tick(10, 10.0, "").as_slice());
    assert_eq!(
        reader.read_frame_at(99).unwrap().unwrap().bytes(),
        tick(99, 99.0, "").as_slice()
    );
    assert_eq!(reader.equal_range(Key::I32(50)).unwrap(), (50, 51));
}

#[test]
fn columnar_delta_pages_roundtrip_and_read_ranges() {
    let schema = tick_schema();
    let mut options = WriterOptions::new("ColumnarDelta");
    options.page_size = 1024;
    options.codec = Codec::Zstd;
    options.codec_selection = CodecSelection::Fixed(Codec::Zstd);
    options.encoding = Encoding::ColumnarDeltaV1;

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema, options).unwrap();
        for i in 0..100 {
            writer.append_frame(&tick(i, (i * 2) as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }

    cursor.set_position(0);
    let mut reader = Reader::new(cursor).unwrap();
    reader.verify().unwrap();
    assert!(matches!(
        reader.read_page_header(0).unwrap().encoding,
        Encoding::ColumnarBasicV1 | Encoding::ColumnarDeltaV1
    ));
    let frames = reader.frames_between(Key::I32(10), Key::I32(20)).unwrap();
    assert_eq!(frames.len(), 11);
    assert_eq!(frames[0].bytes(), tick(10, 20.0, "").as_slice());
    assert_eq!(
        reader.read_frame_at(99).unwrap().unwrap().bytes(),
        tick(99, 198.0, "").as_slice()
    );
    assert_eq!(reader.lower_bound(Key::I32(50)).unwrap(), 50);
    assert_eq!(reader.upper_bound(Key::I32(50)).unwrap(), 51);
}

#[test]
fn smallest_encoding_selection_is_recorded_per_page() {
    let schema = Schema::new(
        "ShortTick",
        vec![
            Field::new("Time", FieldType::UnsignedInteger, 4, 0),
            Field::new("Price", FieldType::UnsignedInteger, 4, 4),
            Field::new("Size", FieldType::SignedInteger, 4, 8),
        ],
        0,
    )
    .unwrap();
    let mut options = WriterOptions::new("SmallestEncoding");
    options.page_size = 4096;
    options.codec = Codec::Zstd;
    options.codec_selection = CodecSelection::Fixed(Codec::Zstd);
    options.encoding_selection = EncodingSelection::Smallest;

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema, options).unwrap();
        for i in 0..5000 {
            writer
                .append_frame(&short_tick(i as i32, 10_000 + i, (i % 100) as i32))
                .unwrap();
        }
        writer.finish().unwrap();
    }

    cursor.set_position(0);
    let mut reader = Reader::new(cursor).unwrap();
    reader.verify().unwrap();
    assert!(matches!(
        reader.read_page_header(0).unwrap().encoding,
        Encoding::ColumnarBasicV1 | Encoding::ColumnarDeltaV1
    ));
}

#[test]
fn v2_supports_utf8_metadata_and_string_table() {
    let schema = Schema::new(
        "取引",
        vec![
            Field::new("時刻", FieldType::SignedInteger, 4, 0),
            Field::new("価格", FieldType::UnsignedInteger, 4, 4),
            Field::new("数量", FieldType::SignedInteger, 4, 8),
        ],
        0,
    )
    .unwrap();
    let mut options = WriterOptions::new("銘柄");
    options.page_size = 4096;
    options.codec = Codec::None;
    options.codec_selection = CodecSelection::Fixed(Codec::None);
    options.string_table = vec![
        "東京".to_string(),
        "München".to_string(),
        "emoji-ok".to_string(),
    ];

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema.clone(), options).unwrap();
        writer.append_frame(&short_tick(1, 10, 100)).unwrap();
        writer.finish().unwrap();
    }

    cursor.set_position(0);
    let reader = Reader::new(cursor).unwrap();
    assert_eq!(reader.header().title, "銘柄");
    assert_eq!(reader.header().schema, schema);
    assert_eq!(
        reader.header().string_table,
        vec!["東京", "München", "emoji-ok"]
    );
}

#[test]
fn finish_leaves_residual_tail_uncompressed() {
    let schema = tick_schema();
    let mut options = WriterOptions::new("Tail");
    options.page_size = 1024;
    options.codec = Codec::Zstd;
    options.codec_selection = CodecSelection::Fixed(Codec::Zstd);

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema, options).unwrap();
        for i in 0..100 {
            writer.append_frame(&tick(i, i as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }

    cursor.set_position(0);
    let mut reader = Reader::new(cursor).unwrap();
    reader.verify().unwrap();
    let last = reader
        .read_page_header(reader.header().page_count - 1)
        .unwrap();
    assert_eq!(last.codec, Codec::None);
}

#[test]
fn open_append_repacks_existing_raw_tail_into_full_pages_and_appends() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("append.fwob");
    let schema = tick_schema();
    let mut options = WriterOptions::new("Append");
    options.page_size = 1024;
    options.codec = Codec::Zstd;
    options.codec_selection = CodecSelection::Fixed(Codec::Zstd);

    {
        let mut writer = Writer::create(&path, schema, options.clone()).unwrap();
        for i in 0..80 {
            writer.append_frame(&tick(i, i as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }

    {
        let mut writer = Writer::open_append(&path, options).unwrap();
        for i in 80..140 {
            writer.append_frame(&tick(i, i as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }

    let mut reader = Reader::open(&path).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, 140);
    let frames = reader.frames_between(Key::I32(0), Key::I32(139)).unwrap();
    assert_eq!(frames.len(), 140);
    let last = reader
        .read_page_header(reader.header().page_count - 1)
        .unwrap();
    assert_eq!(last.codec, Codec::None);
}

#[test]
fn open_append_does_not_rewrite_raw_tail_for_tiny_append() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("append_tiny.fwob");
    let schema = tick_schema();
    let mut options = WriterOptions::new("AppendTiny");
    options.page_size = 1024;
    options.codec = Codec::Zstd;
    options.codec_selection = CodecSelection::Fixed(Codec::Zstd);

    {
        let mut writer = Writer::create(&path, schema, options.clone()).unwrap();
        for i in 0..80 {
            writer.append_frame(&tick(i, i as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }

    let initial_page_count = Reader::open(&path).unwrap().header().page_count;

    {
        let mut writer = Writer::open_append(&path, options).unwrap();
        writer.append_frame(&tick(80, 80.0, "")).unwrap();
        writer.finish().unwrap();
    }

    let mut reader = Reader::open(&path).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, 81);
    assert_eq!(reader.header().page_count, initial_page_count + 1);
    let frames = reader.frames_between(Key::I32(0), Key::I32(80)).unwrap();
    assert_eq!(frames.len(), 81);
}

#[test]
fn smallest_codec_selection_is_recorded_per_page() {
    let schema = tick_schema();
    let mut options = WriterOptions::new("Smallest");
    options.page_size = 4096;
    options.codec_selection = CodecSelection::Smallest;

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema, options).unwrap();
        for i in 0..128 {
            writer.append_frame(&tick(i, 0.0, "")).unwrap();
        }
        writer.finish().unwrap();
    }

    cursor.set_position(0);
    let mut reader = Reader::new(cursor).unwrap();
    let page = reader.read_page_header(0).unwrap();
    assert!(matches!(page.codec, Codec::None | Codec::Lz4 | Codec::Zstd));
    reader.verify().unwrap();
}

#[test]
fn zstd_level_option_is_supported() {
    let schema = tick_schema();
    let mut options = WriterOptions::new("Level");
    options.page_size = 4096;
    options.codec = Codec::Zstd;
    options.codec_selection = CodecSelection::Fixed(Codec::Zstd);
    options.zstd_level = 9;

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema, options).unwrap();
        for i in 0..5000 {
            let mut frame = tick(
                i,
                f64::from_bits((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
                "",
            );
            frame[12..16].copy_from_slice(&(i as u32).wrapping_mul(2_654_435_761).to_le_bytes());
            writer.append_frame(&frame).unwrap();
        }
        writer.finish().unwrap();
    }

    cursor.set_position(0);
    let mut reader = Reader::new(cursor).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.read_page_header(0).unwrap().codec, Codec::Zstd);
}
