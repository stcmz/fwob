use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fwob_core::{Field, FieldType, Key, Schema};
use fwob_v1::{Reader, Writer, WriterOptions};
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

fn frame(time: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&time.to_le_bytes());
    out.extend_from_slice(&(time as u32).to_le_bytes());
    out.extend_from_slice(&(time * 10).to_le_bytes());
    out
}

fn fixture() -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = Writer::new(&mut cursor, schema(), WriterOptions::new("BENCH")).unwrap();
        for i in 0..100_000 {
            writer.append_frame(&frame(i)).unwrap();
        }
    }
    cursor.into_inner()
}

fn bench_v1_range(c: &mut Criterion) {
    let data = fixture();
    c.bench_function("v1 lower/upper bound 100k", |b| {
        b.iter(|| {
            let mut reader = Reader::new(Cursor::new(data.clone()), 0).unwrap();
            black_box(
                reader
                    .frames_between(Key::I32(25_000), Key::I32(75_000))
                    .unwrap()
                    .len(),
            )
        })
    });
}

criterion_group!(benches, bench_v1_range);
criterion_main!(benches);
