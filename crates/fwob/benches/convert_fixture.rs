use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use fwob_core::{Field, FieldType, Schema};
use fwob_v1::{Reader as V1Reader, Writer as V1Writer, WriterOptions as V1WriterOptions};
use fwob_v2::{
    Codec, CodecSelection, Encoding, Writer as V2Writer, WriterOptions as V2WriterOptions,
};
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

fn v1_fixture() -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer =
            V1Writer::new(&mut cursor, schema(), V1WriterOptions::new("BENCH")).unwrap();
        for i in 0..100_000 {
            writer.append_frame(&frame(i)).unwrap();
        }
    }
    cursor.into_inner()
}

fn bench_convert_fixture(c: &mut Criterion) {
    let data = v1_fixture();
    c.bench_function("convert v1 to v2 100k", |b| {
        b.iter(|| {
            let mut v1 = V1Reader::new(Cursor::new(data.clone()), 0).unwrap();
            let strings = v1.read_string_table().unwrap();
            let mut options = V2WriterOptions::new(v1.header().title.clone());
            options.page_size = 256 * 1024;
            options.codec = Codec::Zstd;
            options.codec_selection = CodecSelection::Fixed(Codec::Zstd);
            options.encoding = Encoding::RowRawV1;
            options.string_table = strings;
            let mut out = Cursor::new(Vec::new());
            let mut writer = V2Writer::new(&mut out, v1.schema().clone(), options).unwrap();
            let frame_len = v1.header().frame_length as usize;
            let chunk = v1
                .read_raw_frames_chunk(0, v1.header().frame_count as usize)
                .unwrap();
            for frame in chunk.chunks_exact(frame_len) {
                writer.append_frame(frame).unwrap();
            }
            writer.finish().unwrap();
            black_box(out.into_inner().len())
        })
    });
}

criterion_group!(benches, bench_convert_fixture);
criterion_main!(benches);
