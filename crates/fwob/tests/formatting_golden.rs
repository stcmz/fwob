use fwob::formatting::{FrameFormat, FrameFormatter};
use fwob_core::{Field, FieldSemantic, FieldType, Schema, TimestampUnit};

fn schema() -> Schema {
    Schema::new(
        "Quote",
        vec![
            Field::new("time", FieldType::SignedInteger, 8, 0)
                .with_semantic(FieldSemantic::UnixTimestamp(TimestampUnit::Milliseconds)),
            Field::new("price", FieldType::UnsignedInteger, 4, 8),
            Field::new("symbol", FieldType::StringTableIndex, 1, 12),
            Field::new("name", FieldType::Utf8String, 4, 13),
        ],
        0,
    )
    .unwrap()
}

fn frame() -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(&1_522_742_400_125i64.to_le_bytes());
    frame.extend_from_slice(&5_678_900u32.to_le_bytes());
    frame.push(1);
    frame.extend_from_slice(b"AB  ");
    frame
}

fn render(format: FrameFormat) -> String {
    let schema = schema();
    let strings = vec!["MSFT".to_owned(), "AAPL".to_owned()];
    let mut formatter = FrameFormatter::new(&schema, &strings, format);
    let mut output = Vec::new();
    formatter.write_frame(&mut output, &frame()).unwrap();
    String::from_utf8(output).unwrap()
}

#[test]
fn formatter_outputs_match_golden_files() {
    let cases = [
        (FrameFormat::Raw, include_str!("golden/format_raw.txt")),
        (FrameFormat::Table, include_str!("golden/format_table.txt")),
        (FrameFormat::Markdown, include_str!("golden/format_md.txt")),
        (FrameFormat::Csv, include_str!("golden/format_csv.txt")),
        (
            FrameFormat::JsonLines,
            include_str!("golden/format_jsonl.txt"),
        ),
        (FrameFormat::Hex, include_str!("golden/format_hex.txt")),
    ];
    for (format, expected) in cases {
        assert_eq!(render(format), expected, "format {}", format.as_str());
    }
}
