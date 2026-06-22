//! Sequential frame iteration and v1 verify read in bulk chunks rather than one
//! backend call per frame. These tests exercise multi-chunk files and the
//! chunk-boundary seams to guard the batched `FrameIter`/`MultiRangeFrameIter`
//! and the chunked `verify_key_order`.

use std::{fs, path::Path};

use fwob::{Maintenance, Reader, ReaderOptions};
use fwob_core::{Field, FieldType, Key, Schema};
use tempfile::tempdir;

/// Frames per streaming chunk for this schema: `STREAM_CHUNK_BYTES (256 KiB) /
/// frame_len (8)`. Tests plant seams at multiples of this to cross chunks.
const CHUNK_FRAMES: u64 = 32 * 1024;
const TOTAL: u64 = CHUNK_FRAMES * 2 + 100;

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

/// Frame whose key (`Time`) and `Value` both equal `i`, so frame index `i`
/// has key `i` and a byte pattern unique to that index.
fn frame(i: u64) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&(i as i32).to_le_bytes());
    bytes[4..].copy_from_slice(&(i as u32).to_le_bytes());
    bytes
}

fn decode(bytes: &[u8]) -> (i32, u32) {
    (
        i32::from_le_bytes(bytes[0..4].try_into().unwrap()),
        u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
    )
}

fn create_v1(path: &Path, n: u64) {
    let mut writer =
        fwob_v1::Writer::create(path, schema(), fwob_v1::WriterOptions::new("linear")).unwrap();
    for i in 0..n {
        writer.append_frame(&frame(i)).unwrap();
    }
}

fn create_v2(path: &Path, n: u64) {
    let mut writer =
        fwob_v2::Writer::create(path, schema(), fwob_v2::WriterOptions::new("linear")).unwrap();
    for i in 0..n {
        writer.append_frame(&frame(i)).unwrap();
    }
    writer.finish().unwrap();
}

fn scan(path: &Path) -> Vec<(i32, u32)> {
    let mut reader = Reader::open(path).unwrap();
    let count = reader.frame_count();
    reader
        .frames(0..count)
        .unwrap()
        .map(|frame| decode(frame.unwrap().bytes()))
        .collect()
}

#[test]
fn v1_full_scan_spans_multiple_chunks_and_matches_random_access() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v1.fwob");
    create_v1(&path, TOTAL);

    let scanned = scan(&path);
    assert_eq!(scanned.len(), TOTAL as usize);
    for (i, (time, value)) in scanned.iter().enumerate() {
        assert_eq!((*time, *value), (i as i32, i as u32));
    }

    // Random-access read_frame must agree with the streamed values, including
    // the frames sitting right on the chunk seams.
    let mut reader = Reader::open(&path).unwrap();
    for index in [
        0,
        CHUNK_FRAMES - 1,
        CHUNK_FRAMES,
        CHUNK_FRAMES + 1,
        TOTAL - 1,
    ] {
        let bytes = reader.read_frame(index).unwrap().unwrap();
        assert_eq!(decode(bytes.bytes()), (index as i32, index as u32));
    }
}

#[test]
fn v1_subrange_straddling_a_chunk_boundary() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v1.fwob");
    create_v1(&path, TOTAL);

    let mut reader = Reader::open(&path).unwrap();
    let start = CHUNK_FRAMES - 2;
    let end = CHUNK_FRAMES + 3;
    let got: Vec<_> = reader
        .frames(start..end)
        .unwrap()
        .map(|f| decode(f.unwrap().bytes()).0)
        .collect();
    let expected: Vec<i32> = (start..end).map(|i| i as i32).collect();
    assert_eq!(got, expected);
}

#[test]
fn frames_by_keys_across_chunk_boundaries() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v1.fwob");
    create_v1(&path, TOTAL);

    let mut reader = Reader::open(&path).unwrap();
    let keys = [
        Key::I32(10),
        Key::I32(CHUNK_FRAMES as i32),
        Key::I32(CHUNK_FRAMES as i32 + 1),
        Key::I32(2 * CHUNK_FRAMES as i32),
        Key::I32(TOTAL as i32 - 1),
    ];
    let got: Vec<_> = reader
        .frames_by_keys(&keys)
        .unwrap()
        .map(|f| decode(f.unwrap().bytes()).0)
        .collect();
    assert_eq!(
        got,
        keys.iter()
            .map(|k| match k {
                Key::I32(v) => *v,
                _ => unreachable!(),
            })
            .collect::<Vec<_>>()
    );
}

#[test]
fn v2_iteration_matches_v1() {
    let dir = tempdir().unwrap();
    let v1_path = dir.path().join("v1.fwob");
    let v2_path = dir.path().join("v2.fwob");
    create_v1(&v1_path, TOTAL);
    create_v2(&v2_path, TOTAL);
    assert_eq!(scan(&v1_path), scan(&v2_path));
}

#[test]
fn v1_verify_carries_last_key_across_chunk_boundary() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v1.fwob");
    create_v1(&path, TOTAL);

    // In order: verify passes.
    Maintenance::verify(&path, ReaderOptions::default()).unwrap();

    // Plant an out-of-order key at the *first frame of chunk 1*. Detection here
    // is only possible if the chunked scan carries `last` across the seam.
    let mut bytes = fs::read(&path).unwrap();
    let needle = frame(CHUNK_FRAMES);
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("boundary frame present");
    bytes[pos..pos + 4].copy_from_slice(&(-1i32).to_le_bytes());
    fs::write(&path, &bytes).unwrap();

    assert!(Maintenance::verify(&path, ReaderOptions::default()).is_err());
}

#[test]
fn v1_repair_scans_committed_frames_across_chunks() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v1.fwob");
    create_v1(&path, TOTAL);

    // Append one out-of-order frame past a multi-chunk committed region.
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&frame(0))
        .unwrap();

    let report = Maintenance::repair(&path, ReaderOptions::default()).unwrap();
    assert_eq!(report.frame_count, TOTAL);
}
