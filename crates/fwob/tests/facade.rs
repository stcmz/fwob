use std::{fs::OpenOptions, io::Write, path::Path};

use fwob::{
    concat_files, light_verify_file, repair_file, split_by_keys, verify_file, AnyAppender,
    AnyEditor, AnyReader, AppendOptions, FormatVersion, SplitOptions, VerificationOptions,
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

fn assert_remaining_keys(path: &Path, expected: &[i32]) {
    let mut reader = AnyReader::open(path).unwrap();
    let count = reader.frame_count();
    let keys = reader
        .frames(0..count)
        .unwrap()
        .map(|frame| frame_key(&frame.unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(keys, expected);
    assert_eq!(reader.title(), "query");
    assert_eq!(reader.schema(), &schema());
}

fn create_query_file(path: &Path, version: FormatVersion) {
    match version {
        FormatVersion::V1 => create_query_v1(path),
        FormatVersion::V2 => create_query_v2(path),
    }
}

fn assert_editor_contract(version: FormatVersion) {
    let dir = tempdir().unwrap();

    let single = dir.path().join("single.fwob");
    create_query_file(&single, version);
    let mut editor = AnyEditor::open(&single).unwrap();
    assert!(editor.delete_frame(40).unwrap());
    assert!(!editor.delete_frame(999).unwrap());
    assert_eq!(editor.frame_count(), 299);
    let mut expected = (0..300).map(query_key).collect::<Vec<_>>();
    expected.remove(40);
    assert_remaining_keys(&single, &expected);

    let indexes = dir.path().join("indexes.fwob");
    create_query_file(&indexes, version);
    let mut editor = AnyEditor::open(&indexes).unwrap();
    assert_eq!(editor.delete_frames(40..180).unwrap(), 140);
    let expected = (0..40).chain(180..300).map(query_key).collect::<Vec<_>>();
    assert_remaining_keys(&indexes, &expected);

    let key = dir.path().join("key.fwob");
    create_query_file(&key, version);
    let mut editor = AnyEditor::open(&key).unwrap();
    assert_eq!(editor.delete_key(Key::I32(2)).unwrap(), 140);
    let expected = (0..40).chain(180..300).map(query_key).collect::<Vec<_>>();
    assert_remaining_keys(&key, &expected);

    let key_range = dir.path().join("key-range.fwob");
    create_query_file(&key_range, version);
    let mut editor = AnyEditor::open(&key_range).unwrap();
    assert_eq!(
        editor.delete_key_range(Key::I32(2)..=Key::I32(3)).unwrap(),
        220
    );
    let expected = (0..40).chain(260..300).map(query_key).collect::<Vec<_>>();
    assert_remaining_keys(&key_range, &expected);

    let all = dir.path().join("all.fwob");
    create_query_file(&all, version);
    let mut editor = AnyEditor::open(&all).unwrap();
    assert_eq!(editor.delete_all_frames().unwrap(), 300);
    assert_eq!(editor.frame_count(), 0);
    assert_remaining_keys(&all, &[]);
}

fn assert_metadata_contract(version: FormatVersion) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.fwob");
    match version {
        FormatVersion::V1 => create_v1(&path),
        FormatVersion::V2 => create_v2(&path),
    }

    let mut editor = AnyEditor::open(&path).unwrap();
    editor.set_title("renamed").unwrap();
    assert_eq!(editor.append_string("beta").unwrap(), 1);
    editor
        .replace_string_table(&["one".into(), "two".into(), "three".into()])
        .unwrap();

    let reader = AnyReader::open(&path).unwrap();
    assert_eq!(reader.title(), "renamed");
    assert_eq!(reader.string_table(), ["one", "two", "three"]);
    assert_eq!(reader.frame_count(), 2);
    drop(reader);

    editor.clear_string_table().unwrap();
    let reader = AnyReader::open(&path).unwrap();
    assert!(reader.string_table().is_empty());
    assert_eq!(reader.frame_count(), 2);
}

fn assert_organization_contract(version: FormatVersion) {
    let dir = tempdir().unwrap();
    let source = dir.path().join("series.fwob");
    create_query_file(&source, version);

    let parts = split_by_keys(
        &source,
        dir.path().join("parts"),
        &[Key::I32(2), Key::I32(3)],
        SplitOptions::default(),
    )
    .unwrap();
    assert_eq!(parts.len(), 3);
    assert_eq!(
        parts
            .iter()
            .map(|path| AnyReader::open(path).unwrap().frame_count())
            .collect::<Vec<_>>(),
        [40, 140, 120]
    );

    let joined = dir.path().join("joined.fwob");
    assert_eq!(concat_files(&joined, &parts, 0).unwrap(), 300);
    let mut reader = AnyReader::open(&joined).unwrap();
    assert_eq!(
        reader
            .frames(0..300)
            .unwrap()
            .map(|frame| frame_key(&frame.unwrap()))
            .collect::<Vec<_>>(),
        (0..300).map(query_key).collect::<Vec<_>>()
    );

    let reversed = parts.iter().rev().cloned().collect::<Vec<_>>();
    assert!(concat_files(dir.path().join("invalid.fwob"), &reversed, 0).is_err());
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

#[test]
fn bounded_memory_editor_contract_is_identical_for_v1_and_v2() {
    assert_editor_contract(FormatVersion::V1);
    assert_editor_contract(FormatVersion::V2);
}

#[test]
fn metadata_editor_contract_is_identical_for_v1_and_v2() {
    assert_metadata_contract(FormatVersion::V1);
    assert_metadata_contract(FormatVersion::V2);
}

#[test]
fn split_and_concat_contract_is_identical_for_v1_and_v2() {
    assert_organization_contract(FormatVersion::V1);
    assert_organization_contract(FormatVersion::V2);
}

#[test]
fn verification_and_repair_are_identical_for_v1_and_v2() {
    let dir = tempdir().unwrap();
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("{version:?}.fwob"));
        create_query_file(&path, version);
        let options = VerificationOptions::default();

        assert_eq!(light_verify_file(&path, options).unwrap(), version);
        assert_eq!(verify_file(&path, options).unwrap(), version);

        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[0xaa, 0xbb, 0xcc])
            .unwrap();

        assert!(light_verify_file(&path, options).is_err());
        assert!(verify_file(&path, options).is_err());
        assert_eq!(repair_file(&path, options).unwrap(), version);
        assert_eq!(verify_file(&path, options).unwrap(), version);
    }
}
