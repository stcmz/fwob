use std::io::Cursor;

use fwob_core::{Field, FieldType, Key, Schema};
use fwob_v1::{Editor, Reader, Writer, WriterOptions};

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
    assert!(text.len() <= 4);
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&time.to_le_bytes());
    out.extend_from_slice(&value.to_le_bytes());
    let mut str_bytes = [b' '; 4];
    str_bytes[..text.len()].copy_from_slice(text.as_bytes());
    out.extend_from_slice(&str_bytes);
    out
}

#[test]
fn reads_real_spot_v1_prefix_fixture() {
    let path = "tests/fixtures/spot_4096.fwob";
    let mut reader = Reader::open(path, 0).unwrap();
    assert_eq!(reader.header().title, "SPOT");
    assert_eq!(reader.header().frame_type, "ShortTick");
    assert_eq!(reader.header().frame_count, 4096);
    assert_eq!(reader.header().frame_length, 12);
    assert_eq!(reader.header().field_types, 0x011);
    assert_eq!(reader.schema().fields[0].name, "Time");
    assert_eq!(
        reader.schema().fields[0].field_type,
        FieldType::UnsignedInteger
    );
    assert_eq!(reader.schema().fields[1].name, "Price");
    assert_eq!(
        reader.schema().fields[1].field_type,
        FieldType::UnsignedInteger
    );
    assert_eq!(reader.schema().fields[2].name, "Size");
    assert_eq!(
        reader.schema().fields[2].field_type,
        FieldType::SignedInteger
    );
    reader.verify_key_order().unwrap();
    let first = reader.read_key_at(0).unwrap().unwrap();
    let last = reader.read_key_at(4095).unwrap().unwrap();
    assert!(first <= last);
    assert!(!reader.frames_between(first, last).unwrap().is_empty());
}

#[test]
fn writes_and_reads_v1_header_and_frames() {
    let schema = tick_schema();
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer =
            Writer::new(&mut cursor, schema.clone(), WriterOptions::new("HelloFwob")).unwrap();
        writer.append_frame(&tick(12, 99.88, "")).unwrap();
        writer.append_frame(&tick(13, 44456.0111, "a")).unwrap();
        writer.append_frame(&tick(100, 77234.56, "abcd")).unwrap();
    }

    cursor.set_position(0);
    let mut reader = Reader::new(cursor, 0).unwrap();
    assert_eq!(reader.header().title, "HelloFwob");
    assert_eq!(reader.header().frame_type, "Tick");
    assert_eq!(reader.header().frame_count, 3);
    assert_eq!(reader.header().frame_length, 16);
    assert_eq!(reader.header().field_types, 0x320);
    assert_eq!(reader.schema(), &schema);
    assert_eq!(reader.read_key_at(0).unwrap(), Some(Key::I32(12)));
    assert_eq!(reader.read_key_at(1).unwrap(), Some(Key::I32(13)));
    assert_eq!(reader.read_key_at(2).unwrap(), Some(Key::I32(100)));
    assert_eq!(reader.read_key_at(3).unwrap(), None);
}

#[test]
fn v1_binary_search_matches_csharp_inclusive_range_behavior() {
    let schema = tick_schema();
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema, WriterOptions::new("HelloFwob")).unwrap();
        writer.append_frame(&tick(12, 1.0, "")).unwrap();
        writer.append_frame(&tick(12, 2.0, "")).unwrap();
        writer.append_frame(&tick(13, 3.0, "")).unwrap();
        writer.append_frame(&tick(100, 4.0, "")).unwrap();
    }

    cursor.set_position(0);
    let mut reader = Reader::new(cursor, 0).unwrap();
    assert_eq!(reader.lower_bound(Key::I32(12)).unwrap(), 0);
    assert_eq!(reader.upper_bound(Key::I32(12)).unwrap(), 2);
    assert_eq!(reader.equal_range(Key::I32(12)).unwrap(), (0, 2));
    assert_eq!(
        reader
            .frames_between(Key::I32(12), Key::I32(13))
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        reader
            .frames_between(Key::I32(14), Key::I32(99))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn v1_string_table_roundtrip_uses_dotnet_string_encoding() {
    let schema = tick_schema();
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema, WriterOptions::new("HelloFwob")).unwrap();
        assert_eq!(writer.append_string("mystr").unwrap(), 0);
        assert_eq!(writer.append_string("hello2").unwrap(), 1);
        assert_eq!(writer.append_string("test_string3").unwrap(), 2);
    }

    cursor.set_position(0);
    let mut reader = Reader::new(cursor, 0).unwrap();
    assert_eq!(
        reader.read_string_table().unwrap(),
        vec!["mystr", "hello2", "test_string3"]
    );
}

#[test]
fn v1_writer_rejects_unsorted_append() {
    let schema = tick_schema();
    let mut cursor = Cursor::new(Vec::new());
    let mut writer = Writer::new(&mut cursor, schema, WriterOptions::new("HelloFwob")).unwrap();
    writer.append_frame(&tick(100, 1.0, "")).unwrap();
    assert!(writer.append_frame(&tick(13, 1.0, "")).is_err());
}

#[test]
fn editor_supports_v1_delete_and_rewrite() {
    let schema = tick_schema();
    let mut editor = Editor::new(schema, "HelloFwob").unwrap();
    for key in [12, 12, 13, 13, 14, 15, 15, 100, 100] {
        editor.append_frame(&tick(key, key as f64, "")).unwrap();
    }

    assert_eq!(
        editor
            .delete_frames_between(Key::I32(13), Key::I32(15))
            .unwrap(),
        5
    );
    assert_eq!(editor.frame_count(), 4);
    assert_eq!(editor.delete_frames([Key::I32(100)]).unwrap(), 2);
    assert_eq!(editor.frame_count(), 2);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edited.fwob");
    editor.save_as(&path).unwrap();

    let mut reader = Reader::open(&path, 0).unwrap();
    assert_eq!(reader.header().frame_count, 2);
    assert_eq!(reader.read_key_at(0).unwrap(), Some(Key::I32(12)));
    assert_eq!(reader.read_key_at(1).unwrap(), Some(Key::I32(12)));
    reader.verify_key_order().unwrap();
}
