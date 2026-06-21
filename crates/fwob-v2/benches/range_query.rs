use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use fwob_core::{Field, FieldType, Key, Schema};
use fwob_v2::{Codec, CodecSelection, Reader, Writer, WriterOptions};
use std::io::Cursor;

fn schema() -> Schema {
    Schema::new(
        "Tick",
        vec![
            Field::new("Time", FieldType::SignedInteger, 4, 0),
            Field::new("Price", FieldType::UnsignedInteger, 4, 4),
            Field::new("Size", FieldType::SignedInteger, 4, 8),
        ],
        0,
    )
    .unwrap()
}

fn frame(time: i32) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[0..4].copy_from_slice(&time.to_le_bytes());
    out[4..8].copy_from_slice(&(time as u32).to_le_bytes());
    out[8..12].copy_from_slice(&(time * 10).to_le_bytes());
    out
}

fn fixture() -> Vec<u8> {
    let mut options = WriterOptions::new("BENCH");
    options.page_size = 256 * 1024;
    options.codec = Codec::Zstd;
    options.codec_selection = CodecSelection::Fixed(Codec::Zstd);
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema(), options).unwrap();
        for i in 0..100_000 {
            writer.append_frame(&frame(i)).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

fn bench_range_query(c: &mut Criterion) {
    let data = fixture();
    c.bench_function("v2 range 50k rows", |b| {
        b.iter(|| {
            let mut reader = Reader::new(Cursor::new(data.clone())).unwrap();
            black_box(
                reader
                    .frames_between(Key::I32(25_000), Key::I32(75_000))
                    .unwrap()
                    .len(),
            )
        })
    });
}

criterion_group!(benches, bench_range_query);
criterion_main!(benches);
