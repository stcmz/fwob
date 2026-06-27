use std::{
    io::{Cursor, Read, Seek, SeekFrom},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use fwob_core::{Field, FieldType, Key, Schema};
use fwob_v2::{
    Codec, CodecSelection, Encoding, EncodingSelection, Reader, Writer, WriterOptions,
    FILE_HEADER_LEN, PAGE_HEADER_LEN,
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

/// A `tick` whose value/string payload is incompressible, so a batch of them forces the writer
/// to emit multiple compressed pages (the key/Time field stays sequential for ordering).
fn noisy_tick(time: i32) -> Vec<u8> {
    let mut frame = tick(
        time,
        f64::from_bits((time as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
        "",
    );
    frame[12..16].copy_from_slice(&(time as u32).wrapping_mul(2_654_435_761).to_le_bytes());
    frame
}

fn short_tick(time: i32, price: u32, size: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&time.to_le_bytes());
    out.extend_from_slice(&price.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out
}

struct CountingCursor {
    inner: Cursor<Vec<u8>>,
    seeks: Arc<AtomicUsize>,
}

impl CountingCursor {
    fn new(bytes: Vec<u8>, seeks: Arc<AtomicUsize>) -> Self {
        Self {
            inner: Cursor::new(bytes),
            seeks,
        }
    }
}

impl Read for CountingCursor {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl Seek for CountingCursor {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.seeks.fetch_add(1, Ordering::Relaxed);
        self.inner.seek(position)
    }
}

#[test]
fn page_header_is_80_bytes() {
    assert_eq!(PAGE_HEADER_LEN, 80);
}

#[test]
fn owned_presorted_buffers_roundtrip_without_changing_writer_semantics() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("owned.fwob");
    let mut options = WriterOptions::new("owned");
    options.page_size = 1024;
    options.codec = Codec::None;
    let mut writer = Writer::create(&path, tick_schema(), options).unwrap();
    let mut frames = Vec::new();
    for time in 0..100 {
        frames.extend_from_slice(&tick(time, time as f64, "OWN"));
    }
    writer.append_presorted_raw_frames_owned(frames).unwrap();
    writer.finish().unwrap();

    let mut reader = Reader::open(&path).unwrap();
    assert_eq!(reader.header().frame_count, 100);
    assert_eq!(reader.first_key().unwrap(), Some(Key::I32(0)));
    assert_eq!(reader.last_key().unwrap(), Some(Key::I32(99)));
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
fn equal_range_handles_duplicates_spanning_multiple_pages() {
    let schema = tick_schema();
    let mut options = WriterOptions::new("DuplicateKeys");
    options.page_size = 1024;
    options.codec = Codec::None;
    options.codec_selection = CodecSelection::Fixed(Codec::None);
    options.encoding = Encoding::RowRawV1;
    options.encoding_selection = EncodingSelection::Fixed(Encoding::RowRawV1);

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema, options).unwrap();
        for index in 0..500 {
            let key = if index < 50 {
                1
            } else if index < 450 {
                2
            } else {
                3
            };
            writer.append_frame(&tick(key, index as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }

    cursor.set_position(0);
    let mut reader = Reader::new(cursor).unwrap();
    assert!(reader.header().page_count > 3);
    assert_eq!(reader.equal_range(Key::I32(0)).unwrap(), (0, 0));
    assert_eq!(reader.equal_range(Key::I32(1)).unwrap(), (0, 50));
    assert_eq!(reader.equal_range(Key::I32(2)).unwrap(), (50, 450));
    assert_eq!(reader.equal_range(Key::I32(3)).unwrap(), (450, 500));
    assert_eq!(reader.equal_range(Key::I32(4)).unwrap(), (500, 500));
}

#[test]
fn sequential_indexed_reads_advance_page_by_page() {
    let schema = tick_schema();
    let mut options = WriterOptions::new("Sequential");
    options.page_size = 1024;
    options.codec = Codec::None;
    options.codec_selection = CodecSelection::Fixed(Codec::None);
    options.encoding = Encoding::RowRawV1;
    options.encoding_selection = EncodingSelection::Fixed(Encoding::RowRawV1);

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema, options).unwrap();
        for index in 0..500 {
            writer.append_frame(&tick(index, index as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }

    let seeks = Arc::new(AtomicUsize::new(0));
    let mut reader = Reader::new(CountingCursor::new(cursor.into_inner(), seeks.clone())).unwrap();
    let page_count = reader.header().page_count as usize;
    for index in 0..500 {
        assert_eq!(
            reader.read_frame_at(index).unwrap().unwrap().bytes(),
            tick(index as i32, index as f64, "").as_slice()
        );
    }

    assert!(
        seeks.load(Ordering::Relaxed) <= page_count + 10,
        "sequential reads should seek approximately once per page"
    );
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
fn open_append_with_zstd_coalesces_raw_tail_on_tiny_append() {
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
    // The new frame coalesces into the reclaimed raw tail rather than adding a fresh page.
    assert_eq!(reader.header().page_count, initial_page_count);
    let frames = reader.frames_between(Key::I32(0), Key::I32(80)).unwrap();
    assert_eq!(frames.len(), 81);
}

#[test]
fn open_append_with_none_codec_coalesces_into_last_page() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("none_coalesce.fwob");
    let mut options = WriterOptions::new("NoneCoalesce");
    options.page_size = 1024; // capacity (1024 - 80) / 16 = 59 frames/page
    options.codec = Codec::None;
    options.codec_selection = CodecSelection::Fixed(Codec::None);
    options.encoding = Encoding::RowRawV1;
    options.encoding_selection = EncodingSelection::Fixed(Encoding::RowRawV1);

    {
        let mut writer = Writer::create(&path, tick_schema(), options.clone()).unwrap();
        for i in 0..30 {
            writer.append_frame(&tick(i, i as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }
    let base_pages = Reader::open(&path).unwrap().header().page_count;
    assert_eq!(base_pages, 1);

    const SESSIONS: i32 = 3;
    for s in 0..SESSIONS {
        let mut writer = Writer::open_append(&path, options.clone()).unwrap();
        let base = 30 + s * 2;
        writer.append_frame(&tick(base, base as f64, "")).unwrap();
        writer
            .append_frame(&tick(base + 1, (base + 1) as f64, ""))
            .unwrap();
        writer.finish().unwrap();
    }

    let mut reader = Reader::open(&path).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, 30 + SESSIONS as u64 * 2);
    // All appends coalesce into the single trailing page until it fills.
    assert_eq!(reader.header().page_count, base_pages);
    assert_eq!(
        reader.read_page_header(0).unwrap().frame_count,
        30 + SESSIONS as u32 * 2
    );
    let frames = reader
        .frames_between(Key::I32(0), Key::I32(29 + SESSIONS * 2))
        .unwrap();
    assert_eq!(frames.len() as u64, 30 + SESSIONS as u64 * 2);
}

#[test]
fn open_append_with_none_codec_starts_new_page_when_tail_full() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("none_full.fwob");
    let mut options = WriterOptions::new("NoneFull");
    options.page_size = 1024; // capacity 59 frames/page
    options.codec = Codec::None;
    options.codec_selection = CodecSelection::Fixed(Codec::None);
    options.encoding = Encoding::RowRawV1;
    options.encoding_selection = EncodingSelection::Fixed(Encoding::RowRawV1);

    {
        let mut writer = Writer::create(&path, tick_schema(), options.clone()).unwrap();
        for i in 0..59 {
            writer.append_frame(&tick(i, i as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }
    let base_pages = Reader::open(&path).unwrap().header().page_count;
    assert_eq!(base_pages, 1);

    {
        let mut writer = Writer::open_append(&path, options).unwrap();
        writer.append_frame(&tick(59, 59.0, "")).unwrap();
        writer.append_frame(&tick(60, 60.0, "")).unwrap();
        writer.finish().unwrap();
    }

    let mut reader = Reader::open(&path).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, 61);
    // The trailing page was full, so a fresh page is started instead of rewriting it.
    assert_eq!(reader.header().page_count, base_pages + 1);
    assert_eq!(reader.read_page_header(0).unwrap().frame_count, 59);
    assert_eq!(reader.read_page_header(1).unwrap().frame_count, 2);
    let frames = reader.frames_between(Key::I32(0), Key::I32(60)).unwrap();
    assert_eq!(frames.len(), 61);
}

#[test]
fn open_append_with_zstd_coalesces_raw_tail_pages() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("coalesce.fwob");
    let mut options = WriterOptions::new("Coalesce");
    options.page_size = 1024;
    options.codec = Codec::Zstd;
    options.codec_selection = CodecSelection::Fixed(Codec::Zstd);

    {
        let mut writer = Writer::create(&path, tick_schema(), options.clone()).unwrap();
        for i in 0..80 {
            writer.append_frame(&tick(i, i as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }
    let base_pages = Reader::open(&path).unwrap().header().page_count;

    const SESSIONS: i32 = 5;
    for s in 0..SESSIONS {
        let mut writer = Writer::open_append(&path, options.clone()).unwrap();
        let base = 80 + s * 2;
        writer.append_frame(&tick(base, base as f64, "")).unwrap();
        writer
            .append_frame(&tick(base + 1, (base + 1) as f64, ""))
            .unwrap();
        writer.finish().unwrap();
    }

    let mut reader = Reader::open(&path).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, 80 + SESSIONS as u64 * 2);
    // Each session repacks the raw tail instead of leaving a fresh under-filled page behind.
    assert!(
        reader.header().page_count < base_pages + SESSIONS as u64,
        "expected coalescing: {} pages, base {}",
        reader.header().page_count,
        base_pages
    );
    let frames = reader
        .frames_between(Key::I32(0), Key::I32(79 + SESSIONS * 2))
        .unwrap();
    assert_eq!(frames.len() as u64, 80 + SESSIONS as u64 * 2);
}

#[test]
fn open_append_reclaims_entire_raw_tail_for_recompression() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("limit.fwob");

    // 12 full raw pages via Codec::None (capacity (1024 - 80) / 16 = 59 frames/page).
    let mut none_opts = WriterOptions::new("Limit");
    none_opts.page_size = 1024;
    none_opts.codec = Codec::None;
    none_opts.codec_selection = CodecSelection::Fixed(Codec::None);
    none_opts.encoding = Encoding::RowRawV1;
    none_opts.encoding_selection = EncodingSelection::Fixed(Encoding::RowRawV1);
    {
        let mut writer = Writer::create(&path, tick_schema(), none_opts).unwrap();
        for i in 0..708i32 {
            writer.append_frame(&tick(i, i as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }
    let initial_pages = Reader::open(&path).unwrap().header().page_count;
    assert_eq!(initial_pages, 12);

    // Reopen with Zstd and append more data so compression fires. There is no reclaim cap: the
    // whole 12-page raw tail is loaded back into memory and recompacted with the new frames.
    let mut zstd_opts = WriterOptions::new("Limit");
    zstd_opts.page_size = 1024;
    zstd_opts.codec = Codec::Zstd;
    zstd_opts.codec_selection = CodecSelection::Fixed(Codec::Zstd);
    {
        let mut writer = Writer::open_append(&path, zstd_opts).unwrap();
        for i in 708..1708i32 {
            writer.append_frame(&noisy_tick(i)).unwrap();
        }
        writer.finish().unwrap();
    }

    let mut reader = Reader::open(&path).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, 1708);

    // The entire raw tail was reclaimed and recompacted: even page 0 is now a compressed page
    // (under the old 10-page cap, pages 0 and 1 would have stayed raw `None`). The recompacted file
    // has far fewer raw `None` pages than the 12 it started with — only the trailing incompressible
    // run stays raw.
    assert_eq!(reader.read_page_header(0).unwrap().codec, Codec::Zstd);
    let raw_pages = (0..reader.header().page_count)
        .filter(|&p| reader.read_page_header(p).unwrap().codec == Codec::None)
        .count() as u64;
    assert!(
        raw_pages < initial_pages,
        "whole-tail recompaction should leave fewer raw pages than the original {initial_pages}, found {raw_pages}"
    );

    // Round-trip across the original (recompressed) and newly appended regions.
    assert_eq!(
        reader.read_frame_at(0).unwrap().unwrap().bytes(),
        tick(0, 0.0, "").as_slice()
    );
    assert_eq!(
        reader.read_frame_at(117).unwrap().unwrap().bytes(),
        tick(117, 117.0, "").as_slice()
    );
    assert_eq!(
        reader.read_frame_at(708).unwrap().unwrap().bytes(),
        noisy_tick(708).as_slice()
    );
    assert_eq!(
        reader.read_frame_at(1707).unwrap().unwrap().bytes(),
        noisy_tick(1707).as_slice()
    );
}

#[test]
fn open_append_with_zstd_leaves_full_pages_untouched() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("untouched.fwob");
    let mut options = WriterOptions::new("Untouched");
    options.page_size = 1024; // capacity 59 frames/page
    options.codec = Codec::Zstd;
    options.codec_selection = CodecSelection::Fixed(Codec::Zstd);

    // 139 frames stay below the compress threshold, so finish writes 3 raw pages: [59][59][21].
    {
        let mut writer = Writer::create(&path, tick_schema(), options.clone()).unwrap();
        for i in 0..139 {
            writer.append_frame(&tick(i, i as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }
    assert_eq!(Reader::open(&path).unwrap().header().page_count, 3);

    // Snapshot the two dense leading pages.
    let page_bytes = |index: usize| -> Vec<u8> {
        let raw = std::fs::read(&path).unwrap();
        let start = FILE_HEADER_LEN as usize + index * 1024;
        raw[start..start + 1024].to_vec()
    };
    let page0_before = page_bytes(0);
    let page1_before = page_bytes(1);

    // Append a few frames that fit into the trailing under-filled page (no overflow).
    {
        let mut writer = Writer::open_append(&path, options).unwrap();
        for i in 139..149 {
            writer.append_frame(&tick(i, i as f64, "")).unwrap();
        }
        writer.finish().unwrap();
    }

    // The full leading pages must be byte-for-byte identical — never reclaimed or rewritten.
    assert_eq!(page_bytes(0), page0_before, "page 0 was rewritten");
    assert_eq!(page_bytes(1), page1_before, "page 1 was rewritten");

    let mut reader = Reader::open(&path).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, 149);
    assert_eq!(reader.header().page_count, 3); // the partial page absorbed the appends
    let frames = reader.frames_between(Key::I32(0), Key::I32(148)).unwrap();
    assert_eq!(frames.len(), 149);
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

#[test]
fn sync_at_arbitrary_points_does_not_change_the_file() {
    // A sync is a checkpoint: the eventual file must be byte-identical whether or not (and however
    // often) sync was called. Use an incompressible payload at a small page size so several Zstd
    // pages form and a raw residual remains, exercising the reclaim path on each sync.
    let mut options = WriterOptions::new("Title");
    options.page_size = 4096;
    options.codec = Codec::Zstd;
    options.codec_selection = CodecSelection::Fixed(Codec::Zstd);
    options.zstd_level = 9;

    let frame = |i: i32| {
        let mut frame = tick(
            i,
            f64::from_bits((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
            "",
        );
        frame[12..16].copy_from_slice(&(i as u32).wrapping_mul(2_654_435_761).to_le_bytes());
        frame
    };
    let total = 5000i32;

    let build = |sync_every: Option<i32>| -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = Writer::new(&mut cursor, tick_schema(), options.clone()).unwrap();
            for i in 0..total {
                writer.append_frame(&frame(i)).unwrap();
                if let Some(k) = sync_every {
                    if (i + 1) % k == 0 {
                        writer.sync().unwrap();
                    }
                }
            }
            writer.finish().unwrap();
        }
        cursor.into_inner()
    };

    let reference = build(None);
    for sync_every in [1, 13, 254, 1000, 5000] {
        assert_eq!(
            build(Some(sync_every)),
            reference,
            "sync every {sync_every} frames changed the resulting file",
        );
    }

    // Reference must itself be a valid multi-page Zstd file (so the test is meaningful).
    let mut reader = Reader::new(Cursor::new(reference)).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, total as u64);
    assert!(reader.header().page_count >= 2);
}

#[test]
fn sync_on_a_freshly_created_file_writer_reads_back() {
    // `Writer::create` must open the file read+write: `sync` reads pages back to re-derive and
    // reload the reclaimable tail. A write-only handle (the old `File::create`) fails this with
    // "Access is denied" on Windows. Use a real file (not an in-memory cursor, which is always
    // readable) so the regression is actually exercised.
    let dir = tempdir().unwrap();
    let path = dir.path().join("sync.fwob");

    let mut options = WriterOptions::new("Title");
    options.page_size = 4096;
    options.codec = Codec::Zstd;

    let mut writer = Writer::create(&path, tick_schema(), options).unwrap();
    writer.append_frame(&tick(10, 1.0, "")).unwrap();
    writer.sync().unwrap();
    writer.append_frame(&tick(11, 2.0, "")).unwrap();
    writer.sync().unwrap();
    writer.finish().unwrap();

    let mut reader = Reader::open(&path).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, 2);
}

#[test]
fn reopen_in_batches_does_not_change_the_file() {
    // Closing and reopening the file between batches must produce the same bytes as one continuous
    // open+finish: on reopen the raw tail is loaded back into memory and recompacted exactly as a
    // never-closed writer would, so the file depends only on the frames, not on how appends were
    // split across sessions.
    let mut options = WriterOptions::new("Title");
    options.page_size = 4096;
    options.codec = Codec::Zstd;
    options.codec_selection = CodecSelection::Fixed(Codec::Zstd);
    options.zstd_level = 9;

    let frame = |i: i32| {
        let mut frame = tick(
            i,
            f64::from_bits((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
            "",
        );
        frame[12..16].copy_from_slice(&(i as u32).wrapping_mul(2_654_435_761).to_le_bytes());
        frame
    };
    let total = 5000i32;

    let dir = tempdir().unwrap();

    // One continuous session.
    let continuous = dir.path().join("continuous.fwob");
    {
        let mut writer = Writer::create(&continuous, tick_schema(), options.clone()).unwrap();
        for i in 0..total {
            writer.append_frame(&frame(i)).unwrap();
        }
        writer.finish().unwrap();
    }
    let reference = std::fs::read(&continuous).unwrap();

    // Several non-page-aligned reopen cadences. (The per-frame in-memory case is covered by
    // `sync_at_arbitrary_points_does_not_change_the_file`; reopening 5000× would recompact a
    // growing tail O(n²) at zstd level 9 with no extra coverage.)
    for batch in [13, 254, 1000, 5000] {
        let path = dir.path().join(format!("batched_{batch}.fwob"));
        let mut i = 0i32;
        let mut first = true;
        while i < total {
            let end = (i + batch).min(total);
            if first {
                let mut writer = Writer::create(&path, tick_schema(), options.clone()).unwrap();
                for j in i..end {
                    writer.append_frame(&frame(j)).unwrap();
                }
                writer.finish().unwrap();
                first = false;
            } else {
                let mut writer = Writer::open_append(&path, options.clone()).unwrap();
                for j in i..end {
                    writer.append_frame(&frame(j)).unwrap();
                }
                writer.finish().unwrap();
            }
            i = end;
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            reference,
            "reopening every {batch} frames changed the resulting file",
        );
    }
}
