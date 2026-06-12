use fwob::{FormatVersion, Reader, VerificationOptions, Writer};
use fwob_core::{Field, FieldType, Key, Schema};
use tempfile::tempdir;

fn schema() -> Schema {
    Schema::new(
        "Record",
        vec![Field::new("key", FieldType::SignedInteger, 4, 0)],
        0,
    )
    .unwrap()
}

fn frame(value: i32) -> [u8; 4] {
    value.to_le_bytes()
}

#[test]
fn public_reader_writer_names_are_polymorphic_for_v1_and_v2() {
    let dir = tempdir().unwrap();
    for version in [FormatVersion::V1, FormatVersion::V2] {
        let path = dir.path().join(format!("{version:?}.fwob"));
        let mut writer = match version {
            FormatVersion::V1 => {
                Writer::create_v1(&path, schema(), fwob_v1::WriterOptions::new("test"), &[])
                    .unwrap()
            }
            FormatVersion::V2 => {
                Writer::create_v2(&path, schema(), fwob_v2::WriterOptions::new("test")).unwrap()
            }
        };
        writer.append_frame(&frame(1)).unwrap();
        writer.append_frame(&frame(2)).unwrap();
        writer.finish().unwrap();

        let mut reader = Reader::open(&path).unwrap();
        assert_eq!(reader.format_version(), version);
        assert_eq!(reader.first_key().unwrap(), Some(Key::I32(1)));
        assert_eq!(reader.last_key().unwrap(), Some(Key::I32(2)));
        assert_eq!(reader.equal_range(Key::I32(2)).unwrap(), 1..2);

        assert_eq!(
            fwob::verify_file(&path, VerificationOptions::default()).unwrap(),
            version
        );
    }
}
