use std::{
    fs,
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    path::Path,
};

use fwob::{
    DeletionPacking, Editor, FormatVersion, FrameSelection, KeySelector, Maintenance,
    OperationOptions, Organizer, Reader, ReaderOptions, Writer,
};
use fwob_core::{Field, FieldType, Key, Schema};
use tempfile::tempdir;

mod common;

use common::for_each_format;

fn assert_exact_size_iterator<I: ExactSizeIterator>(iterator: &I, expected: usize) {
    assert_eq!(iterator.len(), expected);
}

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

fn create_string_lookup_file(path: &Path, version: FormatVersion) {
    let strings = ["alpha".to_owned(), "beta".to_owned(), "alpha".to_owned()];
    match version {
        FormatVersion::V1 => {
            let mut options = fwob_v1::WriterOptions::new("strings");
            options.string_table_preserved_length = 128;
            let mut writer = fwob_v1::Writer::create(path, schema(), options).unwrap();
            for value in &strings {
                writer.append_string(value).unwrap();
            }
        }
        FormatVersion::V2 => {
            let mut options = fwob_v2::WriterOptions::new("strings");
            options.string_table = strings.into();
            fwob_v2::Writer::create(path, schema(), options)
                .unwrap()
                .finish()
                .unwrap();
        }
    }
}

fn create_empty_file(path: &Path, version: FormatVersion) {
    match version {
        FormatVersion::V1 => {
            Writer::create_v1(path, schema(), fwob_v1::WriterOptions::new("empty"), &[])
                .unwrap()
                .finish()
                .unwrap();
        }
        FormatVersion::V2 => {
            Writer::create_v2(path, schema(), fwob_v2::WriterOptions::new("empty"))
                .unwrap()
                .finish()
                .unwrap();
        }
    }
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

fn create_linear_file(path: &Path, version: FormatVersion, count: i32) {
    let mut writer = match version {
        FormatVersion::V1 => {
            Writer::create_v1(path, schema(), fwob_v1::WriterOptions::new("linear"), &[]).unwrap()
        }
        FormatVersion::V2 => {
            let mut options = fwob_v2::WriterOptions::new("linear");
            options.page_size = 1024;
            options.codec = fwob_v2::Codec::None;
            options.codec_selection = fwob_v2::CodecSelection::Fixed(fwob_v2::Codec::None);
            Writer::create_v2(path, schema(), options).unwrap()
        }
    };
    for index in 0..count {
        writer.append_frame(&frame(index, index as u32)).unwrap();
    }
    writer.finish().unwrap();
}

fn create_compressed_linear_v2(path: &Path, count: i32) {
    let mut options = fwob_v2::WriterOptions::new("linear");
    options.page_size = 1024;
    options.codec = fwob_v2::Codec::Zstd;
    options.codec_selection = fwob_v2::CodecSelection::Fixed(fwob_v2::Codec::Zstd);
    options.encoding = fwob_v2::Encoding::ColumnarBasicV1;
    options.encoding_selection =
        fwob_v2::EncodingSelection::Fixed(fwob_v2::Encoding::ColumnarBasicV1);
    options.compress_partial_page = true;
    let mut writer = fwob_v2::Writer::create(path, schema(), options).unwrap();
    for index in 0..count {
        writer.append_frame(&frame(index, index as u32)).unwrap();
    }
    writer.finish().unwrap();
}

fn read_values(path: &Path) -> Vec<u32> {
    Reader::open(path)
        .unwrap()
        .read_all_frames()
        .unwrap()
        .iter()
        .map(|frame| u32::from_le_bytes(frame.bytes()[4..8].try_into().unwrap()))
        .collect()
}

fn assert_reader_contract(path: &Path, expected_version: FormatVersion) {
    let mut reader = Reader::open(path).unwrap();
    assert_eq!(reader.format_version(), expected_version);
    assert_eq!(reader.schema(), &schema());
    assert_eq!(reader.title(), "facade");
    assert_eq!(reader.frame_count(), 2);
    assert_eq!(reader.string_table(), ["alpha"]);
    assert_eq!(reader.read_raw_frames_chunk(1, 10).unwrap(), frame(2, 20));
    let frames = reader.read_all_frames().unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].bytes(), frame(1, 10));
    assert_eq!(frames[1].bytes(), frame(2, 20));
}

fn assert_appender_contract(path: &Path, expected_version: FormatVersion) {
    let mut appender = Writer::open(path, OperationOptions::default()).unwrap();
    assert_eq!(appender.format_version(), expected_version);
    assert_eq!(appender.schema(), &schema());
    assert_eq!(appender.title(), "facade");
    assert_eq!(appender.frame_count(), 2);
    assert_eq!(appender.string_table(), ["alpha"]);
    appender.append_frame(&frame(3, 30)).unwrap();
    assert_eq!(appender.frame_count(), 3);
    appender.finish().unwrap();

    let mut reader = Reader::open(path).unwrap();
    let frames = reader.read_all_frames().unwrap();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[2].bytes(), frame(3, 30));
}

fn frame_key(frame: &fwob_core::OwnedFrame) -> i32 {
    i32::from_le_bytes(frame.bytes()[..4].try_into().unwrap())
}

fn assert_query_contract(path: &Path, expected_version: FormatVersion) {
    let mut reader = Reader::open(path).unwrap();
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

    let stream = reader.frames(38..42).unwrap();
    assert_exact_size_iterator(&stream, 4);
    let keys = stream
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
    let mut reader = Reader::open(path).unwrap();
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
    let mut editor = Editor::open(&single).unwrap();
    assert!(editor.delete_frame(40).unwrap());
    assert!(!editor.delete_frame(999).unwrap());
    assert_eq!(editor.frame_count(), 299);
    let mut expected = (0..300).map(query_key).collect::<Vec<_>>();
    expected.remove(40);
    assert_remaining_keys(&single, &expected);

    let indexes = dir.path().join("indexes.fwob");
    create_query_file(&indexes, version);
    let mut editor = Editor::open(&indexes).unwrap();
    assert_eq!(editor.delete_frames(40..180).unwrap(), 140);
    let expected = (0..40).chain(180..300).map(query_key).collect::<Vec<_>>();
    assert_remaining_keys(&indexes, &expected);

    let key = dir.path().join("key.fwob");
    create_query_file(&key, version);
    let mut editor = Editor::open(&key).unwrap();
    assert_eq!(editor.delete_key(Key::I32(2)).unwrap(), 140);
    let expected = (0..40).chain(180..300).map(query_key).collect::<Vec<_>>();
    assert_remaining_keys(&key, &expected);

    let key_range = dir.path().join("key-range.fwob");
    create_query_file(&key_range, version);
    let mut editor = Editor::open(&key_range).unwrap();
    assert_eq!(
        editor.delete_key_range(Key::I32(2)..=Key::I32(3)).unwrap(),
        220
    );
    let expected = (0..40).chain(260..300).map(query_key).collect::<Vec<_>>();
    assert_remaining_keys(&key_range, &expected);

    let all = dir.path().join("all.fwob");
    create_query_file(&all, version);
    let mut editor = Editor::open(&all).unwrap();
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

    let mut editor = Editor::open(&path).unwrap();
    editor.set_title("renamed").unwrap();
    assert_eq!(editor.append_string("beta").unwrap(), 1);
    editor
        .replace_string_table(&["one".into(), "two".into(), "three".into()])
        .unwrap();

    let reader = Reader::open(&path).unwrap();
    assert_eq!(reader.title(), "renamed");
    assert_eq!(reader.string_table(), ["one", "two", "three"]);
    assert_eq!(reader.frame_count(), 2);
    drop(reader);

    editor.clear_string_table().unwrap();
    let reader = Reader::open(&path).unwrap();
    assert!(reader.string_table().is_empty());
    assert_eq!(reader.frame_count(), 2);
}

#[test]
fn v1_metadata_edits_do_not_rewrite_frames_or_resize_the_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata-in-place.fwob");
    create_v1(&path);
    let original = fs::read(&path).unwrap();
    let frame_offset = (fwob_v1::HEADER_LEN + 128) as usize;

    let mut editor = Editor::open(&path).unwrap();
    editor
        .update_metadata(
            Some("renamed"),
            Some(&["one".into(), "two".into(), "three".into()]),
        )
        .unwrap();

    let edited = fs::read(&path).unwrap();
    assert_eq!(edited.len(), original.len());
    assert_eq!(&edited[frame_offset..], &original[frame_offset..]);
    let mut reader = Reader::open(&path).unwrap();
    assert_eq!(reader.title(), "renamed");
    assert_eq!(reader.string_table(), ["one", "two", "three"]);
    assert_eq!(
        reader
            .read_all_frames()
            .unwrap()
            .iter()
            .map(|frame| frame.bytes().to_vec())
            .collect::<Vec<_>>(),
        [frame(1, 10).to_vec(), frame(2, 20).to_vec()]
    );
}

#[test]
fn v1_metadata_validation_fails_before_modifying_the_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata-validation.fwob");
    create_v1(&path);
    let original = fs::read(&path).unwrap();

    let mut editor = Editor::open(&path).unwrap();
    assert!(editor.set_title("title-is-far-too-long").is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(editor.set_title("非ASCII").is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(editor.replace_string_table(&["x".repeat(129)]).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn indexed_string_lookup_is_identical_for_v1_and_v2() {
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("strings.fwob");
        create_string_lookup_file(&path, version);
        let reader = Reader::open(&path).unwrap();

        assert_eq!(reader.string_at(0), Some("alpha"));
        assert_eq!(reader.string_at(1), Some("beta"));
        assert_eq!(reader.string_at(3), None);
        assert_eq!(reader.string_index("alpha"), Some(2));
        assert_eq!(reader.string_index("beta"), Some(1));
        assert_eq!(reader.string_index("missing"), None);
        assert!(reader.contains_string("alpha"));
        assert!(!reader.contains_string("missing"));

        // Repeated calls exercise the cached path.
        assert_eq!(reader.string_index("alpha"), Some(2));
    }
}

#[test]
fn ordered_multi_index_deletion_is_identical_for_v1_and_v2() {
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let dir = tempdir().unwrap();
        let indices = dir.path().join("indices.fwob");
        create_linear_file(&indices, version, 10);
        let mut editor = Editor::open(&indices).unwrap();
        assert_eq!(editor.delete_indices(&[1, 4, 8]).unwrap(), 3);
        assert_eq!(read_values(&indices), [0, 2, 3, 5, 6, 7, 9]);

        let ranges = dir.path().join("ranges.fwob");
        create_linear_file(&ranges, version, 10);
        let mut editor = Editor::open(&ranges).unwrap();
        assert_eq!(editor.delete_ranges(&[1..3, 5..7]).unwrap(), 4);
        assert_eq!(read_values(&ranges), [0, 3, 4, 7, 8, 9]);

        assert!(editor.delete_indices(&[2, 2]).is_err());
        assert!(editor.delete_ranges(&[2..4, 3..5]).is_err());
    }
}

#[test]
fn v1_deletion_preserves_prefix_and_compacts_only_the_suffix() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v1-in-place-delete.fwob");
    create_linear_file(&path, FormatVersion::V1, 100);
    let original = fs::read(&path).unwrap();
    let first_frame = fwob_v1::Reader::open(&path, 0)
        .unwrap()
        .header()
        .first_frame_position() as usize;
    let unchanged_end = first_frame + 40 * 8;

    let mut editor = Editor::open(&path).unwrap();
    assert_eq!(editor.delete_frames(40..60).unwrap(), 20);
    let edited = fs::read(&path).unwrap();

    assert_eq!(
        &edited[first_frame..unchanged_end],
        &original[first_frame..unchanged_end]
    );
    assert_eq!(&edited[unchanged_end..], &original[first_frame + 60 * 8..]);
    assert_eq!(edited.len(), original.len() - 20 * 8);
}

#[test]
fn v2_metadata_edits_touch_only_the_fixed_header() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v2-metadata-in-place.fwob");
    create_linear_file(&path, FormatVersion::V2, 100);
    let original = fs::read(&path).unwrap();

    let mut editor = Editor::open(&path).unwrap();
    editor
        .update_metadata(
            Some("renamed"),
            Some(&["one".into(), "two".into(), "three".into()]),
        )
        .unwrap();
    let edited = fs::read(&path).unwrap();

    assert_eq!(edited.len(), original.len());
    assert_eq!(
        &edited[fwob_v2::FILE_HEADER_LEN as usize..],
        &original[fwob_v2::FILE_HEADER_LEN as usize..]
    );
}

#[test]
fn v2_same_length_title_update_touches_only_title_bytes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v2-title-direct.fwob");
    create_v2(&path);
    let original = fs::read(&path).unwrap();

    let mut editor = Editor::open(&path).unwrap();
    editor.set_title("rename").unwrap();
    let edited = fs::read(&path).unwrap();

    let changed = original
        .iter()
        .zip(&edited)
        .enumerate()
        .filter_map(|(index, (before, after))| (before != after).then_some(index))
        .collect::<Vec<_>>();
    assert!(!changed.is_empty());
    assert!(changed.iter().all(|index| (29..35).contains(index)));
    assert_eq!(Reader::open(&path).unwrap().title(), "rename");
}

#[test]
fn v2_deletion_rewrites_only_affected_pages_and_later_page_headers() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v2-local-delete.fwob");
    create_linear_file(&path, FormatVersion::V2, 500);

    let original = fs::read(&path).unwrap();
    let mut original_reader = fwob_v2::Reader::open(&path).unwrap();
    let original_header = original_reader.header().clone();
    let first_page_after_deletion = original_reader.find_page_for_index(250).unwrap().unwrap();
    let original_tail_header = original_reader
        .read_page_header(first_page_after_deletion)
        .unwrap();
    let original_tail_offset = original_header.page_offset(first_page_after_deletion) as usize;
    let original_tail_payload = original[original_tail_offset + fwob_v2::PAGE_HEADER_LEN
        ..original_tail_offset + original_header.page_size as usize]
        .to_vec();

    let mut editor = Editor::open(&path).unwrap();
    assert_eq!(editor.delete_frames(100..180).unwrap(), 80);
    assert_eq!(
        read_values(&path),
        (0..100)
            .chain(180..500)
            .map(|value| value as u32)
            .collect::<Vec<_>>()
    );

    let edited = fs::read(&path).unwrap();
    let mut edited_reader = fwob_v2::Reader::open(&path).unwrap();
    edited_reader.verify().unwrap();
    let edited_tail_header = edited_reader
        .read_page_header(first_page_after_deletion)
        .unwrap();
    let edited_tail_offset = edited_reader
        .header()
        .page_offset(first_page_after_deletion) as usize;
    assert_eq!(
        &edited[edited_tail_offset + fwob_v2::PAGE_HEADER_LEN
            ..edited_tail_offset + edited_reader.header().page_size as usize],
        original_tail_payload
    );
    assert_eq!(
        edited_tail_header.first_frame_index,
        original_tail_header.first_frame_index - 80
    );
}

#[test]
fn v2_deletion_can_compress_the_partial_replacement_page() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v2-compressed-partial-delete.fwob");
    create_compressed_linear_v2(&path, 1_000);

    let mut editor = Editor::open_with_operation_options(
        &path,
        OperationOptions {
            reader_options: ReaderOptions::default(),
            deletion_packing: DeletionPacking::RepackToEnd,
            v2: Some({
                let mut options = fwob_v2::WriterOptions::new("");
                options.compress_partial_page = true;
                options
            }),
        },
    )
    .unwrap();
    assert_eq!(editor.delete_frames(100..900).unwrap(), 800);

    let mut reader = fwob_v2::Reader::open(&path).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, 200);
    assert!(
        reader
            .read_page_header(reader.header().page_count - 1)
            .unwrap()
            .codec
            != fwob_v2::Codec::None
    );
}

#[test]
fn v2_repack_to_end_can_leave_the_final_eof_remainder_raw() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v2-raw-eof-remainder-delete.fwob");
    create_compressed_linear_v2(&path, 1_000);

    let mut editor = Editor::open_with_operation_options(
        &path,
        OperationOptions {
            reader_options: ReaderOptions::default(),
            deletion_packing: DeletionPacking::RepackToEnd,
            v2: Some(fwob_v2::WriterOptions::new("")),
        },
    )
    .unwrap();
    assert_eq!(editor.delete_frames(100..900).unwrap(), 800);

    let mut reader = fwob_v2::Reader::open(&path).unwrap();
    reader.verify().unwrap();
    assert_eq!(
        reader
            .read_page_header(reader.header().page_count - 1)
            .unwrap()
            .codec,
        fwob_v2::Codec::None
    );
}

#[test]
fn v2_local_repack_expands_the_affected_interval_when_selected_pages_need_more_space() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v2-local-expand-delete.fwob");
    create_compressed_linear_v2(&path, 1_000);

    let mut raw_options = fwob_v2::WriterOptions::new("");
    raw_options.codec = fwob_v2::Codec::None;
    raw_options.codec_selection = fwob_v2::CodecSelection::Fixed(fwob_v2::Codec::None);
    let mut editor = Editor::open_with_operation_options(
        &path,
        OperationOptions {
            reader_options: ReaderOptions::default(),
            deletion_packing: DeletionPacking::LocalRepack,
            v2: Some(raw_options),
        },
    )
    .unwrap();
    assert!(editor.delete_frame(10).unwrap());

    let mut reader = fwob_v2::Reader::open(&path).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, 999);
    assert_eq!(reader.read_key_at(10).unwrap(), Some(Key::I32(11)));
}

#[test]
fn append_and_delete_share_operation_options() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("shared-operation-options.fwob");
    create_compressed_linear_v2(&path, 500);

    let mut raw = fwob_v2::WriterOptions::new("");
    raw.codec = fwob_v2::Codec::None;
    raw.codec_selection = fwob_v2::CodecSelection::Fixed(fwob_v2::Codec::None);
    let options = OperationOptions {
        reader_options: ReaderOptions::default(),
        v2: Some(raw),
        deletion_packing: DeletionPacking::LocalRepack,
    };

    let mut writer = Writer::open(&path, options.clone()).unwrap();
    writer.append_frame(&frame(500, 500)).unwrap();
    writer.finish().unwrap();

    let mut editor = Editor::open_with_operation_options(&path, options).unwrap();
    assert_eq!(editor.delete_key(Key::I32(250)).unwrap(), 1);

    let mut reader = fwob_v2::Reader::open(&path).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, 500);
    assert_eq!(reader.read_key_at(250).unwrap(), Some(Key::I32(251)));
}

fn assert_organization_contract(version: FormatVersion) {
    let dir = tempdir().unwrap();
    let source = dir.path().join("series.fwob");
    create_query_file(&source, version);

    let organizer = Organizer::default();
    let parts = organizer
        .split(
            &source,
            dir.path().join("parts"),
            &[Key::I32(2), Key::I32(3)],
        )
        .unwrap();
    assert_eq!(parts.len(), 3);
    assert_eq!(
        parts
            .iter()
            .map(|path| Reader::open(path).unwrap().frame_count())
            .collect::<Vec<_>>(),
        [40, 140, 120]
    );

    let joined = dir.path().join("joined.fwob");
    assert_eq!(organizer.concat(&joined, &parts).unwrap(), 300);
    let mut reader = Reader::open(&joined).unwrap();
    assert_eq!(
        reader
            .frames(0..300)
            .unwrap()
            .map(|frame| frame_key(&frame.unwrap()))
            .collect::<Vec<_>>(),
        (0..300).map(query_key).collect::<Vec<_>>()
    );

    let reversed = parts.iter().rev().cloned().collect::<Vec<_>>();
    assert!(organizer
        .concat(dir.path().join("invalid.fwob"), &reversed)
        .is_err());
}

#[test]
fn v2_split_and_concat_share_operation_options() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("series.fwob");
    create_query_v2(&source);

    let mut v2 = fwob_v2::WriterOptions::new("");
    v2.codec = fwob_v2::Codec::Zstd;
    v2.codec_selection = fwob_v2::CodecSelection::Fixed(fwob_v2::Codec::Zstd);
    v2.compress_partial_page = true;
    let organizer = Organizer {
        operation_options: OperationOptions {
            v2: Some(v2),
            ..Default::default()
        },
        ..Default::default()
    };
    let parts = organizer
        .split(
            &source,
            dir.path().join("parts"),
            &[Key::I32(2), Key::I32(3)],
        )
        .unwrap();
    for part in &parts {
        let mut reader = fwob_v2::Reader::open(part).unwrap();
        assert_eq!(reader.header().page_size, 1024);
        assert_eq!(
            reader.read_page_header(0).unwrap().codec,
            fwob_v2::Codec::Zstd
        );
    }

    let joined = dir.path().join("joined.fwob");
    organizer.concat(&joined, &parts).unwrap();
    let mut reader = fwob_v2::Reader::open(joined).unwrap();
    assert_eq!(reader.header().page_size, 1024);
    assert_eq!(
        reader.read_page_header(0).unwrap().codec,
        fwob_v2::Codec::Zstd
    );
    reader.verify().unwrap();
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
fn writer_creation_and_bulk_append_are_identical_for_v1_and_v2() {
    let dir = tempdir().unwrap();
    let strings = vec!["alpha".to_owned(), "beta".to_owned()];
    let mut frames = Vec::new();
    frames.extend_from_slice(&frame(1, 10));
    frames.extend_from_slice(&frame(2, 20));
    frames.extend_from_slice(&frame(3, 30));

    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("bulk-{version:?}.fwob"));
        let mut writer = match version {
            FormatVersion::V1 => {
                let mut options = fwob_v1::WriterOptions::new("bulk");
                options.string_table_preserved_length = 128;
                Writer::create_v1(&path, schema(), options, &strings).unwrap()
            }
            FormatVersion::V2 => {
                let mut options = fwob_v2::WriterOptions::new("bulk");
                options.string_table = strings.clone();
                options.codec = fwob_v2::Codec::None;
                options.codec_selection = fwob_v2::CodecSelection::Fixed(fwob_v2::Codec::None);
                Writer::create_v2(&path, schema(), options).unwrap()
            }
        };

        assert_eq!(writer.format_version(), version);
        assert_eq!(writer.schema(), &schema());
        assert_eq!(writer.title(), "bulk");
        assert_eq!(writer.frame_count(), 0);
        assert_eq!(writer.string_table(), strings);

        writer.append_presorted_frames(&frames).unwrap();
        assert_eq!(writer.frame_count(), 3);
        writer.finish().unwrap();

        let mut reader = Reader::open(&path).unwrap();
        assert_eq!(reader.format_version(), version);
        assert_eq!(reader.title(), "bulk");
        assert_eq!(reader.string_table(), strings);
        assert_eq!(
            reader
                .read_all_frames()
                .unwrap()
                .iter()
                .map(|frame| frame.bytes().to_vec())
                .collect::<Vec<_>>(),
            frames
                .chunks_exact(8)
                .map(<[u8]>::to_vec)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn transactional_bulk_append_rejects_the_entire_invalid_batch_for_v1_and_v2() {
    let dir = tempdir().unwrap();

    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("transactional-{version:?}.fwob"));
        let mut writer = match version {
            FormatVersion::V1 => Writer::create_v1(
                &path,
                schema(),
                fwob_v1::WriterOptions::new("transactional"),
                &[],
            )
            .unwrap(),
            FormatVersion::V2 => Writer::create_v2(
                &path,
                schema(),
                fwob_v2::WriterOptions::new("transactional"),
            )
            .unwrap(),
        };
        writer.append_frame(&frame(10, 1)).unwrap();

        let mut invalid = Vec::new();
        invalid.extend_from_slice(&frame(11, 2));
        invalid.extend_from_slice(&frame(13, 3));
        invalid.extend_from_slice(&frame(12, 4));
        assert!(writer.append_frames_transactional(&invalid).is_err());
        assert_eq!(writer.frame_count(), 1);

        let mut valid = Vec::new();
        valid.extend_from_slice(&frame(11, 5));
        valid.extend_from_slice(&frame(12, 6));
        writer.append_frames_transactional(&valid).unwrap();
        writer.finish().unwrap();

        let mut reader = Reader::open(&path).unwrap();
        assert_eq!(
            reader
                .frames(0..reader.frame_count())
                .unwrap()
                .map(|frame| frame_key(&frame.unwrap()))
                .collect::<Vec<_>>(),
            [10, 11, 12]
        );
    }
}

#[test]
fn empty_reader_boundaries_are_identical_for_v1_and_v2() {
    let dir = tempdir().unwrap();

    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("empty-{version:?}.fwob"));
        create_empty_file(&path, version);

        let mut reader = Reader::open_with_options(&path, ReaderOptions::default()).unwrap();
        assert_eq!(reader.format_version(), version);
        assert_eq!(reader.frame_count(), 0);
        assert_eq!(reader.read_frame(0).unwrap(), None);
        assert_eq!(reader.read_key(0).unwrap(), None);
        assert_eq!(reader.first_frame().unwrap(), None);
        assert_eq!(reader.last_frame().unwrap(), None);
        assert_eq!(reader.first_key().unwrap(), None);
        assert_eq!(reader.last_key().unwrap(), None);
        assert_eq!(reader.lower_bound(Key::I32(1)).unwrap(), 0);
        assert_eq!(reader.upper_bound(Key::I32(1)).unwrap(), 0);
        assert_eq!(reader.equal_range(Key::I32(1)).unwrap(), 0..0);
        assert_eq!(reader.frames(0..0).unwrap().count(), 0);
        assert_eq!(
            reader
                .frames_by_key(Key::I32(1)..=Key::I32(2))
                .unwrap()
                .count(),
            0
        );
        assert!(reader.read_all_frames().unwrap().is_empty());
    }
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
fn mixed_key_selectors_union_identically_for_v1_and_v2() {
    let dir = tempdir().unwrap();
    for_each_format(|version| {
        let path = dir.path().join(format!("selectors-{version:?}.fwob"));
        create_linear_file(&path, version, 20);
        let mut reader = Reader::open(&path).unwrap();
        let selection = FrameSelection::resolve(
            &mut reader,
            &[
                KeySelector::From(Key::I32(18)),
                KeySelector::Exact(Key::I32(2)),
                KeySelector::Between {
                    first: Key::I32(5),
                    last: Key::I32(7),
                },
                KeySelector::Through(Key::I32(0)),
                KeySelector::Exact(Key::I32(6)),
            ],
        )
        .unwrap();
        assert_eq!(selection.ranges(), &[0..1, 2..3, 5..8, 18..20]);
        assert_eq!(selection.frame_count(), 7);
    });
}

#[test]
fn ordered_multi_key_query_and_deletion_are_identical_for_v1_and_v2() {
    let dir = tempdir().unwrap();
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("multi-key-{version:?}.fwob"));
        create_query_file(&path, version);

        let mut reader = Reader::open(&path).unwrap();
        let stream = reader
            .frames_by_keys(&[
                Key::I32(1),
                Key::I32(2),
                Key::I32(2),
                Key::I32(4),
                Key::I32(5),
            ])
            .unwrap();
        assert_exact_size_iterator(&stream, 220);
        let frames = stream.collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(frames.len(), 220);
        assert_eq!(
            frames.iter().map(frame_key).collect::<Vec<_>>(),
            (0..40)
                .map(|_| 1)
                .chain((0..140).map(|_| 2))
                .chain((0..40).map(|_| 5))
                .collect::<Vec<_>>()
        );
        assert!(reader.frames_by_keys(&[Key::I32(3), Key::I32(2)]).is_err());
        drop(reader);

        let mut editor = Editor::open(&path).unwrap();
        assert!(editor.delete_keys(&[Key::I32(3), Key::I32(2)]).is_err());
        assert_eq!(editor.frame_count(), 300);
        assert_eq!(
            editor
                .delete_keys(&[Key::I32(1), Key::I32(1), Key::I32(3), Key::I32(4)])
                .unwrap(),
            120
        );
        assert_remaining_keys(
            &path,
            &(0..140)
                .map(|_| 2)
                .chain((0..40).map(|_| 5))
                .collect::<Vec<_>>(),
        );
    }
}

#[test]
fn unbounded_key_operations_are_identical_for_v1_and_v2() {
    let dir = tempdir().unwrap();
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let query_path = dir.path().join(format!("unbounded-query-{version:?}.fwob"));
        create_query_file(&query_path, version);
        let mut reader = Reader::open(&query_path).unwrap();
        assert_eq!(
            reader
                .frames_before(Key::I32(2))
                .unwrap()
                .map(|frame| frame_key(&frame.unwrap()))
                .collect::<Vec<_>>(),
            (0..40)
                .map(|_| 1)
                .chain((0..140).map(|_| 2))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            reader
                .frames_after(Key::I32(3))
                .unwrap()
                .map(|frame| frame_key(&frame.unwrap()))
                .collect::<Vec<_>>(),
            (0..80)
                .map(|_| 3)
                .chain((0..40).map(|_| 5))
                .collect::<Vec<_>>()
        );

        let before_path = dir.path().join(format!("delete-before-{version:?}.fwob"));
        create_query_file(&before_path, version);
        let mut editor = Editor::open(&before_path).unwrap();
        assert_eq!(editor.delete_before(Key::I32(2)).unwrap(), 180);
        assert_remaining_keys(
            &before_path,
            &(0..80)
                .map(|_| 3)
                .chain((0..40).map(|_| 5))
                .collect::<Vec<_>>(),
        );

        let after_path = dir.path().join(format!("delete-after-{version:?}.fwob"));
        create_query_file(&after_path, version);
        let mut editor = Editor::open(&after_path).unwrap();
        assert_eq!(editor.delete_after(Key::I32(3)).unwrap(), 120);
        assert_remaining_keys(
            &after_path,
            &(0..40)
                .map(|_| 1)
                .chain((0..140).map(|_| 2))
                .collect::<Vec<_>>(),
        );
    }
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
        let options = ReaderOptions::default();

        assert_eq!(
            Maintenance::light_verify(&path, options)
                .unwrap()
                .format_version,
            version
        );
        assert_eq!(
            Maintenance::verify(&path, options).unwrap().format_version,
            version
        );

        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[0xaa, 0xbb, 0xcc])
            .unwrap();

        assert!(Maintenance::light_verify(&path, options).is_err());
        assert!(Maintenance::verify(&path, options).is_err());
        assert_eq!(
            Maintenance::repair(&path, options).unwrap().format_version,
            version
        );
        assert_eq!(
            Maintenance::verify(&path, options).unwrap().format_version,
            version
        );
    }
}

#[test]
fn repair_adopts_complete_ordered_uncommitted_suffixes() {
    let dir = tempdir().unwrap();

    let v1_path = dir.path().join("v1-suffix.fwob");
    create_linear_file(&v1_path, FormatVersion::V1, 3);
    OpenOptions::new()
        .append(true)
        .open(&v1_path)
        .unwrap()
        .write_all(&frame(3, 3))
        .unwrap();
    let v1_report = Maintenance::repair(&v1_path, ReaderOptions::default()).unwrap();
    assert_eq!(v1_report.frame_count, 4);
    let mut v1_reader = Reader::open(&v1_path).unwrap();
    assert_eq!(v1_reader.last_key().unwrap(), Some(Key::I32(3)));

    let v2_path = dir.path().join("v2-suffix.fwob");
    create_linear_file(&v2_path, FormatVersion::V2, 500);
    let mut v2_reader = fwob_v2::Reader::open(&v2_path).unwrap();
    let original_pages = v2_reader.header().page_count;
    let original_frames = v2_reader.header().frame_count;
    assert!(original_pages > 1);
    let uncommitted_page = v2_reader.read_page_header(original_pages - 1).unwrap();
    drop(v2_reader);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&v2_path)
        .unwrap();
    fwob_v2::update_counts(
        &mut file,
        original_pages - 1,
        uncommitted_page.first_frame_index,
    )
    .unwrap();
    drop(file);

    let v2_report = Maintenance::repair(&v2_path, ReaderOptions::default()).unwrap();
    assert_eq!(v2_report.frame_count, original_frames);
    let v2_reader = Reader::open(&v2_path).unwrap();
    assert_eq!(v2_reader.frame_count(), original_frames);
}

#[test]
fn repair_discards_unordered_or_corrupt_uncommitted_suffixes() {
    let dir = tempdir().unwrap();

    let v1_path = dir.path().join("v1-invalid-suffix.fwob");
    create_linear_file(&v1_path, FormatVersion::V1, 3);
    OpenOptions::new()
        .append(true)
        .open(&v1_path)
        .unwrap()
        .write_all(&frame(0, 0))
        .unwrap();
    let v1_report = Maintenance::repair(&v1_path, ReaderOptions::default()).unwrap();
    assert_eq!(v1_report.frame_count, 3);

    let v2_path = dir.path().join("v2-invalid-suffix.fwob");
    create_linear_file(&v2_path, FormatVersion::V2, 500);
    let mut v2_reader = fwob_v2::Reader::open(&v2_path).unwrap();
    let original_pages = v2_reader.header().page_count;
    assert!(original_pages > 1);
    let uncommitted_page = v2_reader.read_page_header(original_pages - 1).unwrap();
    let page_size = v2_reader.header().page_size;
    drop(v2_reader);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&v2_path)
        .unwrap();
    fwob_v2::update_counts(
        &mut file,
        original_pages - 1,
        uncommitted_page.first_frame_index,
    )
    .unwrap();
    file.seek(SeekFrom::Start(
        fwob_v2::FILE_HEADER_LEN
            + (original_pages - 1) * u64::from(page_size)
            + fwob_v2::PAGE_HEADER_LEN as u64,
    ))
    .unwrap();
    file.write_all(&[0xff]).unwrap();
    drop(file);

    let v2_report = Maintenance::repair(&v2_path, ReaderOptions::default()).unwrap();
    assert_eq!(v2_report.frame_count, uncommitted_page.first_frame_index);
}
