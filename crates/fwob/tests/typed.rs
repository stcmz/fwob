use std::path::Path;

use fwob::{FormatVersion, OperationOptions, TypedEditor, TypedReader, TypedWriter};
use fwob_core::{
    Decimal, FieldSemantic, FieldType, FixedString, FwobFrame, StringIndex, StringIndex16,
    StringIndex64, StringIndex8, TimestampUnit,
};
use tempfile::tempdir;

#[derive(Debug, Clone, Copy, PartialEq, FwobFrame)]
struct TypedTick {
    #[fwob(key)]
    time: i32,
    price: u32,
    size: i16,
    label: [u8; 4],
    #[fwob(string_index)]
    symbol: StringIndex,
    #[fwob(ignore)]
    transient: u8,
}

fn tick(time: i32, symbol: u32) -> TypedTick {
    TypedTick {
        time,
        price: (time as u32) * 10,
        size: time as i16,
        label: *b"TEST",
        symbol: StringIndex(symbol),
        transient: 0,
    }
}

fn create(path: &Path, version: FormatVersion) {
    match version {
        FormatVersion::V1 => {
            let mut options = fwob_v1::WriterOptions::new("typed");
            options.string_table_preserved_length = 256;
            let strings = vec!["AAPL".to_owned(), "SPOT".to_owned()];
            let mut writer = TypedWriter::<TypedTick>::create_v1(path, options, &strings).unwrap();
            writer.append(&tick(1, 0)).unwrap();
            writer.append(&tick(2, 1)).unwrap();
            writer.append(&tick(2, 1)).unwrap();
            writer.append(&tick(3, 0)).unwrap();
            writer.finish().unwrap();
        }
        FormatVersion::V2 => {
            let mut options = fwob_v2::WriterOptions::new("typed");
            options.page_size = 1024;
            options.string_table = vec!["AAPL".to_owned(), "SPOT".to_owned()];
            let mut writer = TypedWriter::<TypedTick>::create_v2(path, options).unwrap();
            writer.append(&tick(1, 0)).unwrap();
            writer.append(&tick(2, 1)).unwrap();
            writer.append(&tick(2, 1)).unwrap();
            writer.append(&tick(3, 0)).unwrap();
            writer.finish().unwrap();
        }
    }
}

fn assert_typed_contract(version: FormatVersion) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("typed.fwob");
    create(&path, version);

    let mut reader = TypedReader::<TypedTick>::open(&path).unwrap();
    assert_eq!(reader.frame_count(), 4);
    assert_eq!(reader.first_key().unwrap(), Some(1));
    assert_eq!(reader.last_key().unwrap(), Some(3));
    assert_eq!(reader.read_frame(0).unwrap(), Some(tick(1, 0)));
    assert_eq!(reader.equal_range(2).unwrap(), 1..3);
    assert_eq!(
        reader
            .frames_by_key(2..=3)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        [tick(2, 1), tick(2, 1), tick(3, 0)]
    );
    drop(reader);

    let mut appender = TypedWriter::<TypedTick>::open(&path, OperationOptions::default()).unwrap();
    appender.append(&tick(4, 0)).unwrap();
    appender.finish().unwrap();

    let mut editor = TypedEditor::<TypedTick>::open(&path).unwrap();
    assert_eq!(editor.delete_key(2).unwrap(), 2);

    let mut reader = TypedReader::<TypedTick>::open(&path).unwrap();
    assert_eq!(
        reader
            .frames(0..reader.frame_count())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        [tick(1, 0), tick(3, 0), tick(4, 0)]
    );
}

#[test]
fn typed_frames_work_identically_for_v1_and_v2() {
    assert_typed_contract(FormatVersion::V1);
    assert_typed_contract(FormatVersion::V2);
}

#[test]
fn typed_ordered_multi_key_operations_work_for_v1_and_v2() {
    let dir = tempdir().unwrap();
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("typed-multi-{version:?}.fwob"));
        create(&path, version);

        let mut reader = TypedReader::<TypedTick>::open(&path).unwrap();
        assert_eq!(
            reader
                .frames_by_keys(&[1, 2, 2, 4])
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            [tick(1, 0), tick(2, 1), tick(2, 1)]
        );
        assert!(reader.frames_by_keys(&[3, 2]).is_err());
        drop(reader);

        let mut editor = TypedEditor::<TypedTick>::open(&path).unwrap();
        assert!(editor.delete_keys(&[3, 2]).is_err());
        assert_eq!(editor.delete_keys(&[1, 1, 3]).unwrap(), 2);

        let mut reader = TypedReader::<TypedTick>::open(&path).unwrap();
        assert_eq!(
            reader
                .frames(0..reader.frame_count())
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            [tick(2, 1), tick(2, 1)]
        );
    }
}

#[test]
fn typed_ordered_multi_index_deletion_works_for_v1_and_v2() {
    let dir = tempdir().unwrap();
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir
            .path()
            .join(format!("typed-index-delete-{version:?}.fwob"));
        create(&path, version);

        let mut editor = TypedEditor::<TypedTick>::open(&path).unwrap();
        assert_eq!(editor.delete_indices(&[0, 3]).unwrap(), 2);
        let mut reader = TypedReader::<TypedTick>::open(&path).unwrap();
        assert_eq!(
            reader
                .frames(0..reader.frame_count())
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            [tick(2, 1), tick(2, 1)]
        );

        drop(reader);
        create(&path, version);
        let mut editor = TypedEditor::<TypedTick>::open(&path).unwrap();
        assert_eq!(editor.delete_ranges(&[0..1, 3..4]).unwrap(), 2);
    }
}

#[test]
fn typed_unbounded_key_operations_work_for_v1_and_v2() {
    let dir = tempdir().unwrap();
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("typed-unbounded-{version:?}.fwob"));
        create(&path, version);

        let mut reader = TypedReader::<TypedTick>::open(&path).unwrap();
        assert_eq!(
            reader
                .frames_before(2)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            [tick(1, 0), tick(2, 1), tick(2, 1)]
        );
        assert_eq!(
            reader
                .frames_after(2)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            [tick(2, 1), tick(2, 1), tick(3, 0)]
        );
        drop(reader);

        let mut editor = TypedEditor::<TypedTick>::open(&path).unwrap();
        assert_eq!(editor.delete_before(1).unwrap(), 1);
        assert_eq!(editor.delete_after(3).unwrap(), 1);
        let mut reader = TypedReader::<TypedTick>::open(&path).unwrap();
        assert_eq!(
            reader
                .frames(0..reader.frame_count())
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            [tick(2, 1), tick(2, 1)]
        );
    }
}

#[test]
fn typed_transactional_append_rejects_the_entire_invalid_batch() {
    let dir = tempdir().unwrap();
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir
            .path()
            .join(format!("typed-transactional-{version:?}.fwob"));
        let mut writer = match version {
            FormatVersion::V1 => TypedWriter::<TypedTick>::create_v1(
                &path,
                fwob_v1::WriterOptions::new("typed"),
                &[],
            )
            .unwrap(),
            FormatVersion::V2 => {
                TypedWriter::<TypedTick>::create_v2(&path, fwob_v2::WriterOptions::new("typed"))
                    .unwrap()
            }
        };
        writer.append(&tick(1, 0)).unwrap();
        assert!(writer
            .append_all_transactional([tick(2, 0), tick(4, 0), tick(3, 0)])
            .is_err());
        assert_eq!(writer.frame_count(), 1);
        assert_eq!(
            writer
                .append_all_transactional([tick(2, 0), tick(3, 0)])
                .unwrap(),
            2
        );
        writer.finish().unwrap();

        let mut reader = TypedReader::<TypedTick>::open(&path).unwrap();
        assert_eq!(
            reader
                .frames(0..reader.frame_count())
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            [tick(1, 0), tick(2, 0), tick(3, 0)]
        );
    }
}

#[derive(Debug, Clone, Copy, FwobFrame)]
struct IncompatibleTick {
    #[fwob(key)]
    time: i64,
}

#[test]
fn typed_reader_rejects_incompatible_schema() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("typed.fwob");
    create(&path, FormatVersion::V2);
    assert!(TypedReader::<IncompatibleTick>::open(&path).is_err());
}

#[derive(Debug, Clone, Copy, PartialEq, FwobFrame)]
struct SupportedFields {
    #[fwob(key)]
    signed_8: i8,
    signed_16: i16,
    signed_32: i32,
    signed_64: i64,
    unsigned_8: u8,
    unsigned_16: u16,
    unsigned_32: u32,
    unsigned_64: u64,
    float_32: f32,
    float_64: f64,
    text: [u8; 5],
    fixed_text: FixedString<8>,
    decimal: Decimal,
    #[fwob(string_index)]
    string_index_8: StringIndex8,
    #[fwob(string_index)]
    string_index_16: StringIndex16,
    #[fwob(string_index)]
    string_index: StringIndex,
    #[fwob(string_index)]
    string_index_64: StringIndex64,
    #[fwob(ignore)]
    ignored: u64,
}

#[test]
fn derive_maps_supported_fields_and_ignores_transient_fields() {
    let schema = SupportedFields::schema();
    assert_eq!(schema.frame_type, "SupportedFields");
    assert_eq!(schema.frame_len, 86);
    assert_eq!(schema.key_field_index, 0);
    assert_eq!(schema.fields.len(), 17);
    assert_eq!(
        schema
            .fields
            .iter()
            .map(|field| (field.field_type, field.length, field.offset))
            .collect::<Vec<_>>(),
        [
            (FieldType::SignedInteger, 1, 0),
            (FieldType::SignedInteger, 2, 1),
            (FieldType::SignedInteger, 4, 3),
            (FieldType::SignedInteger, 8, 7),
            (FieldType::UnsignedInteger, 1, 15),
            (FieldType::UnsignedInteger, 2, 16),
            (FieldType::UnsignedInteger, 4, 18),
            (FieldType::UnsignedInteger, 8, 22),
            (FieldType::FloatingPoint, 4, 30),
            (FieldType::FloatingPoint, 8, 34),
            (FieldType::Utf8String, 5, 42),
            (FieldType::Utf8String, 8, 47),
            (FieldType::FloatingPoint, 16, 55),
            (FieldType::StringTableIndex, 1, 71),
            (FieldType::StringTableIndex, 2, 72),
            (FieldType::StringTableIndex, 4, 74),
            (FieldType::StringTableIndex, 8, 78),
        ]
    );

    let frame = SupportedFields {
        signed_8: -8,
        signed_16: -16,
        signed_32: -32,
        signed_64: -64,
        unsigned_8: 8,
        unsigned_16: 16,
        unsigned_32: 32,
        unsigned_64: 64,
        float_32: 3.25,
        float_64: 6.5,
        text: *b"hello",
        fixed_text: FixedString::new("hi").unwrap(),
        decimal: Decimal::new(-12_345, 2),
        string_index_8: StringIndex8(5),
        string_index_16: StringIndex16(6),
        string_index: StringIndex(7),
        string_index_64: StringIndex64(8),
        ignored: 99,
    };
    let mut bytes = Vec::new();
    frame.encode(&mut bytes);
    let decoded = SupportedFields::decode(&bytes).unwrap();
    assert_eq!(
        decoded,
        SupportedFields {
            ignored: 0,
            ..frame
        }
    );
}

#[test]
fn narrow_string_indexes_roundtrip_and_resolve_for_v1_and_v2() {
    #[derive(Debug, Clone, Copy, PartialEq, FwobFrame)]
    struct NarrowIndexes {
        #[fwob(key)]
        key: i32,
        #[fwob(string_index)]
        short: StringIndex8,
        #[fwob(string_index)]
        wide: StringIndex16,
    }

    let dir = tempdir().unwrap();
    let strings = vec!["AAPL".to_owned(), "SPOT".to_owned()];
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("narrow-index-{version:?}.fwob"));
        let expected = NarrowIndexes {
            key: 1,
            short: StringIndex8(0),
            wide: StringIndex16(1),
        };
        match version {
            FormatVersion::V1 => {
                let mut options = fwob_v1::WriterOptions::new("narrow-index");
                options.string_table_preserved_length = 32;
                let mut writer =
                    TypedWriter::<NarrowIndexes>::create_v1(&path, options, &strings).unwrap();
                writer.append(&expected).unwrap();
                writer.finish().unwrap();
            }
            FormatVersion::V2 => {
                let mut options = fwob_v2::WriterOptions::new("narrow-index");
                options.string_table = strings.clone();
                let mut writer = TypedWriter::<NarrowIndexes>::create_v2(&path, options).unwrap();
                writer.append(&expected).unwrap();
                writer.finish().unwrap();
            }
        }
        let mut reader = TypedReader::<NarrowIndexes>::open(path).unwrap();
        assert_eq!(reader.read_frame(0).unwrap(), Some(expected));
        assert_eq!(reader.string_at_u64(expected.short.0.into()), Some("AAPL"));
        assert_eq!(reader.string_at_u64(expected.wide.0.into()), Some("SPOT"));
    }
}

#[test]
fn floating_keys_query_identically_for_v1_and_v2() {
    #[derive(Debug, Clone, Copy, PartialEq, FwobFrame)]
    struct FloatKey {
        #[fwob(key)]
        key: f64,
        value: i32,
    }

    let dir = tempdir().unwrap();
    let frames = [
        FloatKey {
            key: -1.5,
            value: 1,
        },
        FloatKey { key: 0.0, value: 2 },
        FloatKey { key: 0.0, value: 3 },
        FloatKey {
            key: 2.25,
            value: 4,
        },
    ];
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("float-key-{version:?}.fwob"));
        match version {
            FormatVersion::V1 => {
                let options = fwob_v1::WriterOptions::new("float-key");
                let mut writer = TypedWriter::<FloatKey>::create_v1(&path, options, &[]).unwrap();
                writer.append_all(frames.iter().copied()).unwrap();
                writer.finish().unwrap();
            }
            FormatVersion::V2 => {
                let options = fwob_v2::WriterOptions::new("float-key");
                let mut writer = TypedWriter::<FloatKey>::create_v2(&path, options).unwrap();
                writer.append_all(frames.iter().copied()).unwrap();
                writer.finish().unwrap();
            }
        }
        let mut reader = TypedReader::<FloatKey>::open(path).unwrap();
        assert_eq!(reader.equal_range(0.0).unwrap(), 1..3);
        assert_eq!(reader.first_key().unwrap(), Some(-1.5));
        assert_eq!(reader.last_key().unwrap(), Some(2.25));
    }
}

#[test]
fn decimal_keys_query_identically_for_v1_and_v2() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, FwobFrame)]
    struct DecimalKey {
        #[fwob(key)]
        key: Decimal,
        value: i32,
    }

    let dir = tempdir().unwrap();
    let frames = [
        DecimalKey {
            key: Decimal::new(-125, 2),
            value: 1,
        },
        DecimalKey {
            key: Decimal::new(100, 2),
            value: 2,
        },
        DecimalKey {
            key: Decimal::new(100, 2),
            value: 3,
        },
        DecimalKey {
            key: Decimal::new(250, 2),
            value: 4,
        },
    ];
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("decimal-key-{version:?}.fwob"));
        match version {
            FormatVersion::V1 => {
                let options = fwob_v1::WriterOptions::new("decimal-key");
                let mut writer = TypedWriter::<DecimalKey>::create_v1(&path, options, &[]).unwrap();
                writer.append_all(frames.iter().copied()).unwrap();
                writer.finish().unwrap();
            }
            FormatVersion::V2 => {
                let options = fwob_v2::WriterOptions::new("decimal-key");
                let mut writer = TypedWriter::<DecimalKey>::create_v2(&path, options).unwrap();
                writer.append_all(frames.iter().copied()).unwrap();
                writer.finish().unwrap();
            }
        }
        let mut reader = TypedReader::<DecimalKey>::open(path).unwrap();
        assert_eq!(reader.equal_range(Decimal::new(1, 0)).unwrap(), 1..3);
        assert_eq!(reader.first_key().unwrap(), Some(Decimal::new(-125, 2)));
        assert_eq!(reader.last_key().unwrap(), Some(Decimal::new(250, 2)));
    }
}

#[test]
fn timestamp_semantics_persist_in_v2_and_are_rejected_by_v1() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, FwobFrame)]
    struct Event {
        #[fwob(key, timestamp = "milliseconds")]
        time: i64,
        value: i32,
    }

    assert_eq!(
        Event::schema().fields[0].semantic,
        FieldSemantic::UnixTimestamp(TimestampUnit::Milliseconds)
    );
    let dir = tempdir().unwrap();
    let v1_path = dir.path().join("event-v1.fwob");
    let v1_result =
        TypedWriter::<Event>::create_v1(&v1_path, fwob_v1::WriterOptions::new("event"), &[]);
    assert!(v1_result.is_err());
    assert!(!v1_path.exists());

    let v2_path = dir.path().join("event-v2.fwob");
    let expected = Event {
        time: 1_522_742_400_125,
        value: 7,
    };
    let mut writer =
        TypedWriter::<Event>::create_v2(&v2_path, fwob_v2::WriterOptions::new("event")).unwrap();
    writer.append(&expected).unwrap();
    writer.finish().unwrap();

    let untyped = fwob::Reader::open(&v2_path).unwrap();
    assert_eq!(
        untyped.schema().fields[0].semantic,
        Event::schema().fields[0].semantic
    );
    let mut reader = TypedReader::<Event>::open(v2_path).unwrap();
    assert_eq!(reader.read_frame(0).unwrap(), Some(expected));
}

#[test]
fn string_index_64_roundtrips_and_resolves_for_v1_and_v2() {
    #[derive(Debug, Clone, Copy, PartialEq, FwobFrame)]
    struct StringIndexTick {
        #[fwob(key)]
        time: i32,
        #[fwob(string_index)]
        symbol: StringIndex64,
    }

    let dir = tempdir().unwrap();
    let strings = vec!["AAPL".to_owned(), "SPOT".to_owned()];
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("string-index-64-{version:?}.fwob"));
        let expected = StringIndexTick {
            time: 1,
            symbol: StringIndex64(1),
        };
        match version {
            FormatVersion::V1 => {
                let mut options = fwob_v1::WriterOptions::new("index64");
                options.string_table_preserved_length = 128;
                let mut writer =
                    TypedWriter::<StringIndexTick>::create_v1(&path, options, &strings).unwrap();
                writer.append(&expected).unwrap();
                writer.finish().unwrap();
            }
            FormatVersion::V2 => {
                let mut options = fwob_v2::WriterOptions::new("index64");
                options.string_table = strings.clone();
                let mut writer = TypedWriter::<StringIndexTick>::create_v2(&path, options).unwrap();
                writer.append(&expected).unwrap();
                writer.finish().unwrap();
            }
        }
        let mut reader = TypedReader::<StringIndexTick>::open(path).unwrap();
        assert_eq!(reader.read_frame(0).unwrap(), Some(expected));
        assert_eq!(reader.string_at_u64(expected.symbol.0), Some("SPOT"));
        assert_eq!(reader.string_index_u64("SPOT"), Some(1));
        assert_eq!(reader.string_at_u64(u64::MAX), None);
    }
}

#[test]
fn decimals_roundtrip_for_v1_and_v2() {
    #[derive(Debug, Clone, Copy, PartialEq, FwobFrame)]
    struct DecimalTick {
        #[fwob(key)]
        time: i32,
        price: Decimal,
    }

    let dir = tempdir().unwrap();
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("decimal-{version:?}.fwob"));
        let expected = DecimalTick {
            time: 1,
            price: Decimal::new(-12_345, 2),
        };
        match version {
            FormatVersion::V1 => {
                let mut writer = TypedWriter::<DecimalTick>::create_v1(
                    &path,
                    fwob_v1::WriterOptions::new("decimal"),
                    &[],
                )
                .unwrap();
                writer.append(&expected).unwrap();
                writer.finish().unwrap();
            }
            FormatVersion::V2 => {
                let mut writer = TypedWriter::<DecimalTick>::create_v2(
                    &path,
                    fwob_v2::WriterOptions::new("decimal"),
                )
                .unwrap();
                writer.append(&expected).unwrap();
                writer.finish().unwrap();
            }
        }
        let mut reader = TypedReader::<DecimalTick>::open(path).unwrap();
        assert_eq!(reader.read_frame(0).unwrap(), Some(expected));
    }
}

#[test]
fn fixed_strings_roundtrip_for_v1_and_v2() {
    #[derive(Debug, Clone, Copy, PartialEq, FwobFrame)]
    struct FixedStringTick {
        #[fwob(key)]
        time: i32,
        text: FixedString<8>,
    }

    let dir = tempdir().unwrap();
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("fixed-string-{version:?}.fwob"));
        let expected = FixedStringTick {
            time: 1,
            text: FixedString::new("cafe").unwrap(),
        };
        match version {
            FormatVersion::V1 => {
                let mut writer = TypedWriter::<FixedStringTick>::create_v1(
                    &path,
                    fwob_v1::WriterOptions::new("fixed"),
                    &[],
                )
                .unwrap();
                writer.append(&expected).unwrap();
                writer.finish().unwrap();
            }
            FormatVersion::V2 => {
                let mut writer = TypedWriter::<FixedStringTick>::create_v2(
                    &path,
                    fwob_v2::WriterOptions::new("fixed"),
                )
                .unwrap();
                writer.append(&expected).unwrap();
                writer.finish().unwrap();
            }
        }
        let mut reader = TypedReader::<FixedStringTick>::open(path).unwrap();
        let actual = reader.read_frame(0).unwrap().unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.text.as_str(), "cafe");
    }
}
