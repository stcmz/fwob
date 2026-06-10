use std::path::Path;

use fwob::{
    AnyAppender, AnyReader, AppendOptions, FormatVersion, FwobAppender, FwobFile, FwobReader,
};
use fwob_core::{Field, FieldType, Key, Schema};
use tempfile::tempdir;

fn schema() -> Schema {
    Schema::new(
        "Tick",
        vec![
            Field::new("Time", FieldType::SignedInteger, 4, 0),
            Field::new("Value", FieldType::UnsignedInteger, 4, 4),
        ],
        0,
    )
    .unwrap()
}

fn frame(time: i32, value: u32) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&time.to_le_bytes());
    bytes[4..].copy_from_slice(&value.to_le_bytes());
    bytes
}

fn create_v1(path: &Path) {
    let mut options = fwob_v1::WriterOptions::new("facade");
    options.string_table_preserved_length = 128;
    let mut writer = fwob_v1::Writer::create(path, schema(), options).unwrap();
    writer.append_string("alpha").unwrap();
    writer.append_frame(&frame(1, 10)).unwrap();
    writer.append_frame(&frame(2, 20)).unwrap();
}

fn create_v2(path: &Path) {
    let mut options = fwob_v2::WriterOptions::new("facade");
    options.codec = fwob_v2::Codec::None;
    options.codec_selection = fwob_v2::CodecSelection::Fixed(fwob_v2::Codec::None);
    options.string_table = vec!["alpha".into()];
    let mut writer = fwob_v2::Writer::create(path, schema(), options).unwrap();
    writer.append_frame(&frame(1, 10)).unwrap();
    writer.append_frame(&frame(2, 20)).unwrap();
    writer.finish().unwrap();
}

fn query_key(index: usize) -> i32 {
    match index {
        0..40 => 1,
        40..180 => 2,
        180..260 => 3,
        _ => 5,
    }
}

fn create_query_v1(path: &Path) {
    let mut writer =
        fwob_v1::Writer::create(path, schema(), fwob_v1::WriterOptions::new("query")).unwrap();
    for index in 0..300 {
        writer
            .append_frame(&frame(query_key(index), index as u32))
            .unwrap();
    }
}

fn create_query_v2(path: &Path) {
    let mut options = fwob_v2::WriterOptions::new("query");
    options.page_size = 1024;
    options.codec = fwob_v2::Codec::None;
    options.codec_selection = fwob_v2::CodecSelection::Fixed(fwob_v2::Codec::None);
    options.encoding = fwob_v2::Encoding::RowRawV1;
    options.encoding_selection = fwob_v2::EncodingSelection::Fixed(fwob_v2::Encoding::RowRawV1);
    let mut writer = fwob_v2::Writer::create(path, schema(), options).unwrap();
    for index in 0..300 {
        writer
            .append_frame(&frame(query_key(index), index as u32))
            .unwrap();
    }
    writer.finish().unwrap();
}

fn assert_reader_contract(path: &Path, expected_version: FormatVersion) {
    let mut reader = AnyReader::open(path).unwrap();
    assert_eq!(reader.format_version(), expected_version);
    assert_eq!(reader.schema(), &schema());
    assert_eq!(reader.title(), "facade");
    assert_eq!(reader.frame_count(), 2);
    assert_eq!(reader.string_table(), ["alpha"]);
    let frames = reader.read_all_frames().unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].bytes(), frame(1, 10));
    assert_eq!(frames[1].bytes(), frame(2, 20));
}

fn assert_appender_contract(path: &Path, expected_version: FormatVersion) {
    let mut appender = AnyAppender::open(path, AppendOptions::default()).unwrap();
    assert_eq!(appender.format_version(), expected_version);
    assert_eq!(appender.schema(), &schema());
    assert_eq!(appender.title(), "facade");
    assert_eq!(appender.frame_count(), 2);
    assert_eq!(appender.string_table(), ["alpha"]);
    appender.append_frame(&frame(3, 30)).unwrap();
    assert_eq!(appender.frame_count(), 3);
    appender.finish().unwrap();

    let mut reader = AnyReader::open(path).unwrap();
    let frames = reader.read_all_frames().unwrap();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[2].bytes(), frame(3, 30));
}

fn frame_key(frame: &fwob_core::OwnedFrame) -> i32 {
    i32::from_le_bytes(frame.bytes()[..4].try_into().unwrap())
}

fn assert_query_contract(path: &Path, expected_version: FormatVersion) {
    let mut reader = AnyReader::open(path).unwrap();
    assert_eq!(reader.format_version(), expected_version);
    assert_eq!(reader.frame_count(), 300);
    assert_eq!(reader.first_key().unwrap(), Some(Key::I32(1)));
    assert_eq!(reader.last_key().unwrap(), Some(Key::I32(5)));
    assert_eq!(frame_key(&reader.first_frame().unwrap().unwrap()), 1);
    assert_eq!(frame_key(&reader.last_frame().unwrap().unwrap()), 5);
    assert_eq!(reader.read_key(179).unwrap(), Some(Key::I32(2)));
    assert_eq!(reader.read_key(180).unwrap(), Some(Key::I32(3)));
    assert!(reader.read_frame(300).unwrap().is_none());

    assert_eq!(reader.lower_bound(Key::I32(2)).unwrap(), 40);
    assert_eq!(reader.upper_bound(Key::I32(2)).unwrap(), 180);
    assert_eq!(reader.equal_range(Key::I32(2)).unwrap(), 40..180);
    assert_eq!(reader.equal_range(Key::I32(4)).unwrap(), 260..260);

    let keys = reader
        .frames(38..42)
        .unwrap()
        .map(|frame| frame_key(&frame.unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(keys, [1, 1, 2, 2]);

    let frames = reader
        .frames_by_key(Key::I32(2)..=Key::I32(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(frames.len(), 220);
    assert!(frames.iter().all(|frame| matches!(frame_key(frame), 2 | 3)));
}

#[test]
fn reader_contract_is_identical_for_v1_and_v2() {
    let dir = tempdir().unwrap();
    let v1 = dir.path().join("v1.fwob");
    let v2 = dir.path().join("v2.fwob");
    create_v1(&v1);
    create_v2(&v2);

    assert_reader_contract(&v1, FormatVersion::V1);
    assert_reader_contract(&v2, FormatVersion::V2);
}

#[test]
fn appender_contract_is_identical_for_v1_and_v2() {
    let dir = tempdir().unwrap();
    let v1 = dir.path().join("v1.fwob");
    let v2 = dir.path().join("v2.fwob");
    create_v1(&v1);
    create_v2(&v2);

    assert_appender_contract(&v1, FormatVersion::V1);
    assert_appender_contract(&v2, FormatVersion::V2);
}

#[test]
fn indexed_key_and_streaming_queries_are_identical_for_v1_and_v2() {
    let dir = tempdir().unwrap();
    let v1 = dir.path().join("query-v1.fwob");
    let v2 = dir.path().join("query-v2.fwob");
    create_query_v1(&v1);
    create_query_v2(&v2);

    assert_query_contract(&v1, FormatVersion::V1);
    assert_query_contract(&v2, FormatVersion::V2);
}
