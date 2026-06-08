use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fwob_v2::Codec;

fn raw_data() -> Vec<u8> {
    let mut data = Vec::with_capacity(2 * 1024 * 1024);
    for i in 0..180_000i32 {
        data.extend_from_slice(&i.to_le_bytes());
        data.extend_from_slice(&(i as u32 / 10).to_le_bytes());
        data.extend_from_slice(&(0i32).to_le_bytes());
    }
    data
}

fn bench_codec_compare(c: &mut Criterion) {
    let data = raw_data();
    c.bench_function("codec none 2MiB", |b| {
        b.iter(|| black_box(Codec::None.compress(&data).unwrap().len()))
    });
    c.bench_function("codec lz4 2MiB", |b| {
        b.iter(|| black_box(Codec::Lz4.compress(&data).unwrap().len()))
    });
    c.bench_function("codec zstd 2MiB", |b| {
        b.iter(|| black_box(Codec::Zstd.compress(&data).unwrap().len()))
    });
}

criterion_group!(benches, bench_codec_compare);
criterion_main!(benches);
