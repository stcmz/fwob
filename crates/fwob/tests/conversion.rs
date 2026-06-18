use std::process::Command;

use fwob::Reader;
use fwob_core::{Field, FieldSemantic, FieldType, FormatVersion, Schema, TimestampUnit};
use fwob_v1::{Reader as V1Reader, Writer as V1Writer, WriterOptions};
use fwob_v2::{Codec, Writer as V2Writer, WriterOptions as V2WriterOptions};
use tempfile::tempdir;

fn assert_command_success(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "command failed with status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn command_output(command: &mut Command) -> std::process::Output {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "command failed with status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn assert_command_failure(command: &mut Command, expected_stderr: &str) {
    let output = command.output().unwrap();
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected_stderr),
        "stderr did not contain {expected_stderr:?}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

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

/// The same structure as `tick_schema`, but `Time` carries a Unix-second timestamp semantic. v2
/// persists this; v1 reads it back as `None`.
fn timestamp_tick_schema() -> Schema {
    Schema::new(
        "Tick",
        vec![
            Field::new("Time", FieldType::SignedInteger, 4, 0)
                .with_semantic(FieldSemantic::UnixTimestamp(TimestampUnit::Seconds)),
            Field::new("Value", FieldType::FloatingPoint, 8, 4),
            Field::new("Str", FieldType::Utf8String, 4, 12),
        ],
        0,
    )
    .unwrap()
}

fn tick(time: i32, value: f64) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&time.to_le_bytes());
    out.extend_from_slice(&value.to_le_bytes());
    out.extend_from_slice(&[b' '; 4]);
    out
}

fn write_v1_file(path: &std::path::Path, schema: Schema, times: std::ops::Range<i32>) {
    let mut writer = V1Writer::create(path, schema, WriterOptions::new("Tick")).unwrap();
    for i in times {
        writer.append_frame(&tick(i, i as f64)).unwrap();
    }
}

fn write_v2_file(path: &std::path::Path, schema: Schema, times: std::ops::Range<i32>) {
    let mut writer = V2Writer::create(path, schema, V2WriterOptions::new("Tick")).unwrap();
    for i in times {
        writer.append_frame(&tick(i, i as f64)).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn cli_concat_refuses_to_overwrite_existing_output_without_force() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a.fwob");
    let b = dir.path().join("b.fwob");
    let out = dir.path().join("out.fwob");
    write_v1_file(&a, tick_schema(), 0..30);
    write_v1_file(&b, tick_schema(), 30..70);

    let exe = env!("CARGO_BIN_EXE_fwob");
    // Fresh output: concat succeeds and writes a's frames.
    assert_command_success(Command::new(exe).args([
        "concat",
        out.to_str().unwrap(),
        a.to_str().unwrap(),
    ]));
    assert_eq!(Reader::open(&out).unwrap().frame_count(), 30);

    // Output now exists: refuse without --force.
    assert_command_failure(
        Command::new(exe).args(["concat", out.to_str().unwrap(), b.to_str().unwrap()]),
        "already exists",
    );
    assert_eq!(Reader::open(&out).unwrap().frame_count(), 30);

    // --force replaces it.
    assert_command_success(Command::new(exe).args([
        "concat",
        "--force",
        out.to_str().unwrap(),
        b.to_str().unwrap(),
    ]));
    assert_eq!(Reader::open(&out).unwrap().frame_count(), 40);
}

#[test]
fn cli_append_key_order_error_names_the_input_file_and_keys() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target.fwob");
    let lower = dir.path().join("lower.fwob");
    // target ends at key 99; the next input starts at key 50 < 99, which violates global key order.
    write_v1_file(&target, tick_schema(), 0..100);
    write_v1_file(&lower, tick_schema(), 50..60);

    let exe = env!("CARGO_BIN_EXE_fwob");
    let output = Command::new(exe)
        .args(["append", target.to_str().unwrap(), lower.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Names the offending input file...
    assert!(
        stderr.contains("lower.fwob"),
        "stderr did not name the input file\nstderr:\n{stderr}"
    );
    // ...and reports both the violating key and the previous key.
    assert!(
        stderr.contains("key order violation") && stderr.contains("50") && stderr.contains("99"),
        "stderr did not report the violating keys\nstderr:\n{stderr}"
    );
}

#[test]
fn cli_appends_v1_input_into_v2_target_with_timestamp_semantic() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target.fwob");
    let input = dir.path().join("input.fwob");
    // v2 target's Time has a timestamp semantic; v1 input's Time is structurally identical but has
    // no semantic. Before the compatibility fix this failed with "input schema does not match".
    write_v2_file(&target, timestamp_tick_schema(), 0..10);
    write_v1_file(&input, tick_schema(), 10..20);

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_success(Command::new(exe).args([
        "append",
        target.to_str().unwrap(),
        input.to_str().unwrap(),
    ]));

    let reader = Reader::open(&target).unwrap();
    assert_eq!(reader.frame_count(), 20);
    assert_eq!(
        reader.schema().fields[0].semantic,
        FieldSemantic::UnixTimestamp(TimestampUnit::Seconds)
    );
}

#[test]
fn cli_converts_v2_to_v2_with_a_new_codec_and_v2_to_v1() {
    let dir = tempdir().unwrap();
    let input = dir.path().join("in.fwob");
    let repacked = dir.path().join("repacked.fwob");
    let downgraded = dir.path().join("down.fwob");
    write_v2_file(&input, timestamp_tick_schema(), 0..50);

    let exe = env!("CARGO_BIN_EXE_fwob");
    // v2 -> v2 re-pack with a different codec (previously errored "failed to open v1 file").
    assert_command_success(Command::new(exe).args([
        "convert",
        "v2",
        input.to_str().unwrap(),
        repacked.to_str().unwrap(),
        "uncompressed",
    ]));
    let mut repacked_reader = fwob_v2::Reader::open(&repacked).unwrap();
    assert_eq!(repacked_reader.header().frame_count, 50);
    assert_eq!(
        repacked_reader.read_page_header(0).unwrap().codec,
        Codec::None
    );
    // The semantic is preserved across a v2 -> v2 conversion.
    assert_eq!(
        repacked_reader.header().schema.fields[0].semantic,
        FieldSemantic::UnixTimestamp(TimestampUnit::Seconds)
    );

    // v2 -> v1 still works.
    assert_command_success(Command::new(exe).args([
        "convert",
        "v1",
        input.to_str().unwrap(),
        downgraded.to_str().unwrap(),
    ]));
    let reader = Reader::open(&downgraded).unwrap();
    assert_eq!(reader.format_version(), FormatVersion::V1);
    assert_eq!(reader.frame_count(), 50);
}

#[test]
fn cli_concat_merges_mixed_v1_and_v2_sources() {
    let dir = tempdir().unwrap();
    let v1_src = dir.path().join("v1.fwob");
    let v2_src = dir.path().join("v2.fwob");
    let out = dir.path().join("merged.fwob");
    write_v1_file(&v1_src, tick_schema(), 0..30);
    write_v2_file(&v2_src, timestamp_tick_schema(), 30..70);

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_success(Command::new(exe).args([
        "concat",
        out.to_str().unwrap(),
        v1_src.to_str().unwrap(),
        v2_src.to_str().unwrap(),
    ]));

    let reader = Reader::open(&out).unwrap();
    assert_eq!(reader.format_version(), FormatVersion::V2);
    assert_eq!(reader.frame_count(), 70);
    // The richer (v2) schema's timestamp semantic survives the merge.
    let v2 = fwob_v2::Reader::open(&out).unwrap();
    assert_eq!(
        v2.header().schema.fields[0].semantic,
        FieldSemantic::UnixTimestamp(TimestampUnit::Seconds)
    );
}

#[test]
fn cli_appends_into_a_v1_target_from_v1_and_v2_inputs() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target.fwob");
    let v2_input = dir.path().join("input_v2.fwob");
    let v1_input = dir.path().join("input_v1.fwob");
    write_v1_file(&target, tick_schema(), 0..10);
    write_v2_file(&v2_input, tick_schema(), 10..20);
    write_v1_file(&v1_input, tick_schema(), 20..30);

    let exe = env!("CARGO_BIN_EXE_fwob");
    // v2 input -> v1 target (previously errored "failed to open target v2 file").
    assert_command_success(Command::new(exe).args([
        "append",
        target.to_str().unwrap(),
        v2_input.to_str().unwrap(),
    ]));
    // v1 input -> v1 target.
    assert_command_success(Command::new(exe).args([
        "append",
        target.to_str().unwrap(),
        v1_input.to_str().unwrap(),
    ]));

    let reader = Reader::open(&target).unwrap();
    assert_eq!(reader.format_version(), FormatVersion::V1);
    assert_eq!(reader.frame_count(), 30);
}

#[test]
fn cli_concat_honors_explicit_output_format() {
    let dir = tempdir().unwrap();
    let v1_src = dir.path().join("v1.fwob");
    let v2_src = dir.path().join("v2.fwob");
    write_v1_file(&v1_src, tick_schema(), 0..30);
    write_v2_file(&v2_src, tick_schema(), 30..70);
    let exe = env!("CARGO_BIN_EXE_fwob");

    // Force a v1 output from mixed sources.
    let out_v1 = dir.path().join("out_v1.fwob");
    assert_command_success(Command::new(exe).args([
        "concat",
        "v1",
        out_v1.to_str().unwrap(),
        v1_src.to_str().unwrap(),
        v2_src.to_str().unwrap(),
    ]));
    let reader = Reader::open(&out_v1).unwrap();
    assert_eq!(reader.format_version(), FormatVersion::V1);
    assert_eq!(reader.frame_count(), 70);

    // Force a v2 output (with a codec write token) from a single v1 source.
    let out_v2 = dir.path().join("out_v2.fwob");
    assert_command_success(Command::new(exe).args([
        "concat",
        "v2",
        out_v2.to_str().unwrap(),
        v1_src.to_str().unwrap(),
        "uncompressed",
    ]));
    let mut v2 = fwob_v2::Reader::open(&out_v2).unwrap();
    assert_eq!(v2.header().frame_count, 30);
    assert_eq!(v2.read_page_header(0).unwrap().codec, Codec::None);

    // v2 write tokens are rejected for a v1 output.
    assert_command_failure(
        Command::new(exe).args([
            "concat",
            "v1",
            dir.path().join("bad.fwob").to_str().unwrap(),
            v1_src.to_str().unwrap(),
            "zstd",
        ]),
        "v2 write tokens are not valid",
    );
}

#[test]
fn cli_appends_multiple_inputs() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target.fwob");
    let in1 = dir.path().join("in1.fwob");
    let in2 = dir.path().join("in2.fwob");
    write_v2_file(&target, tick_schema(), 0..10);
    write_v1_file(&in1, tick_schema(), 10..20);
    write_v2_file(&in2, tick_schema(), 20..30);

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_success(Command::new(exe).args([
        "append",
        target.to_str().unwrap(),
        in1.to_str().unwrap(),
        in2.to_str().unwrap(),
    ]));
    assert_eq!(Reader::open(&target).unwrap().frame_count(), 30);
}

#[test]
fn cli_edit_sets_and_clears_field_semantic() {
    let dir = tempdir().unwrap();
    let v2 = dir.path().join("v2.fwob");
    write_v2_file(&v2, tick_schema(), 0..10);
    let exe = env!("CARGO_BIN_EXE_fwob");

    assert_command_success(Command::new(exe).args([
        "edit",
        v2.to_str().unwrap(),
        "--set-semantic",
        "Time=unix-seconds",
    ]));
    assert_eq!(
        fwob_v2::Reader::open(&v2).unwrap().header().schema.fields[0].semantic,
        FieldSemantic::UnixTimestamp(TimestampUnit::Seconds)
    );

    assert_command_success(Command::new(exe).args([
        "edit",
        v2.to_str().unwrap(),
        "--set-semantic",
        "Time=none",
    ]));
    assert_eq!(
        fwob_v2::Reader::open(&v2).unwrap().header().schema.fields[0].semantic,
        FieldSemantic::None
    );

    // v1 cannot store semantics.
    let v1 = dir.path().join("v1.fwob");
    write_v1_file(&v1, tick_schema(), 0..10);
    assert_command_failure(
        Command::new(exe).args([
            "edit",
            v1.to_str().unwrap(),
            "--set-semantic",
            "Time=unix-seconds",
        ]),
        "v1 files cannot store field semantics",
    );
}

#[test]
fn cli_prints_package_version() {
    let output = command_output(Command::new(env!("CARGO_BIN_EXE_fwob")).arg("--version"));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("fwob {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn cli_splits_concatenates_and_edits_metadata() {
    let dir = tempdir().unwrap();
    let input = dir.path().join("input.fwob");
    let parts_dir = dir.path().join("parts");
    let joined = dir.path().join("joined.fwob");
    {
        let mut writer =
            V1Writer::create(&input, tick_schema(), WriterOptions::new("HelloFwob")).unwrap();
        writer.append_string("sym").unwrap();
        for i in 0..30 {
            writer.append_frame(&tick(i, i as f64)).unwrap();
        }
    }

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_success(Command::new(exe).args([
        "split",
        input.to_str().unwrap(),
        parts_dir.to_str().unwrap(),
        "10",
        "20",
    ]));
    let parts = (0..3)
        .map(|index| parts_dir.join(format!("input.part{index}.fwob")))
        .collect::<Vec<_>>();
    assert_eq!(
        parts
            .iter()
            .map(|path| Reader::open(path).unwrap().frame_count())
            .collect::<Vec<_>>(),
        [10, 10, 10]
    );

    let mut concat = Command::new(exe);
    concat.arg("concat").arg(&joined);
    concat.args(&parts);
    assert_command_success(&mut concat);
    assert_command_success(Command::new(exe).args([
        "edit",
        joined.to_str().unwrap(),
        "--title",
        "Renamed",
        "--clear-strings",
        "--append-string",
        "new-symbol",
    ]));

    let mut reader = Reader::open(&joined).unwrap();
    assert_eq!(reader.title(), "Renamed");
    assert_eq!(reader.string_table(), ["new-symbol"]);
    assert_eq!(reader.read_all_frames().unwrap().len(), 30);
}

#[test]
fn cli_finds_and_deletes_by_key_or_key_range() {
    let dir = tempdir().unwrap();
    let v1_path = dir.path().join("query-v1.fwob");
    let v2_path = dir.path().join("query-v2.fwob");
    {
        let mut writer =
            V1Writer::create(&v1_path, tick_schema(), WriterOptions::new("Query")).unwrap();
        for i in 0..30 {
            writer.append_frame(&tick(i, i as f64)).unwrap();
        }
    }

    let exe = env!("CARGO_BIN_EXE_fwob");
    let find =
        command_output(Command::new(exe).args(["find", v1_path.to_str().unwrap(), "10..12"]));
    let stdout = String::from_utf8_lossy(&find.stdout);
    assert!(stdout.contains("[find]"));
    assert!(stdout.contains("start_index = 10"));
    assert!(stdout.contains("end_index = 13"));
    assert!(stdout.contains("frame_count = 3"));
    assert!(stdout.contains("preview = \"\"\""));

    let mixed = command_output(Command::new(exe).args([
        "find",
        v1_path.to_str().unwrap(),
        "18..",
        "2",
        "5..7",
        "..0",
        "6",
    ]));
    assert!(mixed.status.success());
    let stdout = String::from_utf8_lossy(&mixed.stdout);
    assert!(stdout.contains("selector_count = 5"));
    assert!(stdout.contains("range_count = 4"));
    assert!(stdout.contains("frame_count = 17"), "{stdout}");

    let all = command_output(Command::new(exe).args(["find", v1_path.to_str().unwrap()]));
    assert!(all.status.success());
    assert!(String::from_utf8_lossy(&all.stdout).contains("frame_count = 30"));

    let reversed = Command::new(exe)
        .args(["find", v1_path.to_str().unwrap(), "12..10"])
        .output()
        .unwrap();
    assert!(!reversed.status.success());

    assert_command_success(Command::new(exe).args([
        "convert",
        v1_path.to_str().unwrap(),
        v2_path.to_str().unwrap(),
        "4KiB",
        "zstd",
    ]));
    let deletion = command_output(Command::new(exe).args([
        "delete",
        v2_path.to_str().unwrap(),
        "10",
        "12",
        "repack-to-end",
        "zstd",
        "columnar-basic",
        "compress-partial-page",
        "verify",
    ]));
    let stdout = String::from_utf8_lossy(&deletion.stdout);
    assert!(stdout.contains("[deletion]"));
    assert!(stdout.contains("deleted_frames = 3"));
    assert!(stdout.contains("remaining_frames = 27"));
    assert!(stdout.contains("deletion_packing = \"repack-to-end\""));
    assert!(stdout.contains("verified = true"));

    let mut reader = Reader::open(&v2_path).unwrap();
    assert_eq!(reader.equal_range(fwob_core::Key::I32(10)).unwrap(), 10..10);
    assert_eq!(reader.read_key(10).unwrap(), Some(fwob_core::Key::I32(13)));
}

#[test]
fn cli_dumps_all_or_mixed_key_selections_in_reusable_formats() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("dump-v1.fwob");
    {
        let mut writer =
            V1Writer::create(&path, tick_schema(), WriterOptions::new("Dump")).unwrap();
        for i in 0..6 {
            writer.append_frame(&tick(i, i as f64 + 0.5)).unwrap();
        }
    }

    let exe = env!("CARGO_BIN_EXE_fwob");
    let raw = command_output(Command::new(exe).args([
        "dump",
        path.to_str().unwrap(),
        "4..",
        "..1",
        "raw",
    ]));
    let raw = String::from_utf8(raw.stdout).unwrap();
    let rows = raw.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 4);
    assert!(rows[0].starts_with("0 "));
    assert!(rows[1].starts_with("1 "));
    assert!(rows[2].starts_with("4 "));
    assert!(rows[3].starts_with("5 "));
    assert!(!raw.contains(','));

    let csv = command_output(Command::new(exe).args(["dump", path.to_str().unwrap(), "2", "csv"]));
    let csv = String::from_utf8(csv.stdout).unwrap();
    assert!(csv.starts_with("Time,Value,Str\n"));
    assert_eq!(csv.lines().count(), 2);

    let jsonl =
        command_output(Command::new(exe).args(["dump", path.to_str().unwrap(), "2..3", "jsonl"]));
    let jsonl = String::from_utf8(jsonl.stdout).unwrap();
    assert_eq!(jsonl.lines().count(), 2);
    assert!(jsonl.lines().all(|line| line.starts_with('{')));
}

#[test]
fn cli_inspect_and_dump_use_v2_timestamp_semantics() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("timestamp.fwob");
    let schema = Schema::new(
        "Event",
        vec![
            Field::new("Time", FieldType::SignedInteger, 8, 0)
                .with_semantic(FieldSemantic::UnixTimestamp(TimestampUnit::Milliseconds)),
            Field::new("Value", FieldType::SignedInteger, 4, 8),
        ],
        0,
    )
    .unwrap();
    let mut frame = Vec::new();
    frame.extend_from_slice(&1_522_742_400_125i64.to_le_bytes());
    frame.extend_from_slice(&7i32.to_le_bytes());
    let mut writer =
        fwob_v2::Writer::create(&path, schema, fwob_v2::WriterOptions::new("event")).unwrap();
    writer.append_frame(&frame).unwrap();
    writer.finish().unwrap();

    let exe = env!("CARGO_BIN_EXE_fwob");
    let inspect = command_output(Command::new(exe).args(["inspect", path.to_str().unwrap()]));
    assert!(String::from_utf8(inspect.stdout)
        .unwrap()
        .contains("semantic = \"unix-milliseconds\""));

    let table = command_output(Command::new(exe).args(["dump", path.to_str().unwrap(), "table"]));
    assert!(String::from_utf8(table.stdout)
        .unwrap()
        .contains("2018-04-03T08:00:00.125Z"));
}

#[test]
fn cli_roundtrips_v1_to_v2_to_v1() {
    let dir = tempdir().unwrap();
    let v1_path = dir.path().join("input.fwob");
    let v2_path = dir.path().join("output-v2.fwob");
    let restored_path = dir.path().join("restored.fwob");

    {
        let mut writer =
            V1Writer::create(&v1_path, tick_schema(), WriterOptions::new("HelloFwob")).unwrap();
        writer.append_string("sym").unwrap();
        for i in 0..256 {
            writer.append_frame(&tick(i, i as f64)).unwrap();
        }
    }

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_success(Command::new(exe).args([
        "convert",
        v1_path.to_str().unwrap(),
        v2_path.to_str().unwrap(),
        "64KiB",
        "uncompressed",
    ]));

    assert_command_success(Command::new(exe).args([
        "convert",
        "v1",
        v2_path.to_str().unwrap(),
        restored_path.to_str().unwrap(),
    ]));

    let mut original = V1Reader::open(&v1_path, 0).unwrap();
    let mut restored = V1Reader::open(&restored_path, 0).unwrap();
    assert_eq!(
        original.read_string_table().unwrap(),
        restored.read_string_table().unwrap()
    );
    assert_eq!(
        original.read_all_frames().unwrap(),
        restored.read_all_frames().unwrap()
    );
}

#[test]
fn cli_appends_v1_frames_to_existing_v2() {
    let dir = tempdir().unwrap();
    let base_v1_path = dir.path().join("base.fwob");
    let append_v1_path = dir.path().join("append.fwob");
    let v2_path = dir.path().join("target.fwob");

    {
        let mut writer = V1Writer::create(
            &base_v1_path,
            tick_schema(),
            WriterOptions::new("HelloFwob"),
        )
        .unwrap();
        writer.append_string("sym").unwrap();
        for i in 0..128 {
            writer.append_frame(&tick(i, i as f64)).unwrap();
        }
    }
    {
        let mut writer = V1Writer::create(
            &append_v1_path,
            tick_schema(),
            WriterOptions::new("HelloFwob"),
        )
        .unwrap();
        writer.append_string("sym").unwrap();
        for i in 128..256 {
            writer.append_frame(&tick(i, i as f64)).unwrap();
        }
    }

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_success(Command::new(exe).args([
        "convert",
        "v2",
        base_v1_path.to_str().unwrap(),
        v2_path.to_str().unwrap(),
        "4KiB",
        "zstd",
    ]));

    assert_command_success(Command::new(exe).args([
        "append",
        v2_path.to_str().unwrap(),
        "verify",
        append_v1_path.to_str().unwrap(),
    ]));

    let mut reader = fwob_v2::Reader::open(&v2_path).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.header().frame_count, 256);
    assert_eq!(
        reader
            .frames_between(fwob_core::Key::I32(0), fwob_core::Key::I32(255))
            .unwrap()
            .len(),
        256
    );
}

#[test]
fn cli_converts_v1_to_columnar_v2() {
    let dir = tempdir().unwrap();
    let v1_path = dir.path().join("input.fwob");
    let v2_path = dir.path().join("columnar.fwob");

    {
        let mut writer =
            V1Writer::create(&v1_path, tick_schema(), WriterOptions::new("HelloFwob")).unwrap();
        writer.append_string("sym").unwrap();
        for i in 0..256 {
            writer.append_frame(&tick(i, i as f64)).unwrap();
        }
    }

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_success(Command::new(exe).args([
        "convert",
        v1_path.to_str().unwrap(),
        v2_path.to_str().unwrap(),
        "4KiB",
        "zstd",
        "columnar-basic",
        "verify",
    ]));

    let mut reader = fwob_v2::Reader::open(&v2_path).unwrap();
    reader.verify().unwrap();
    assert_eq!(
        reader.read_page_header(0).unwrap().encoding,
        fwob_v2::Encoding::ColumnarBasicV1
    );
    assert_eq!(
        reader
            .frames_between(fwob_core::Key::I32(0), fwob_core::Key::I32(255))
            .unwrap()
            .len(),
        256
    );
}

#[test]
fn cli_converts_v1_to_columnar_delta_v2() {
    let dir = tempdir().unwrap();
    let v1_path = dir.path().join("input.fwob");
    let v2_path = dir.path().join("columnar_delta.fwob");

    {
        let mut writer =
            V1Writer::create(&v1_path, tick_schema(), WriterOptions::new("HelloFwob")).unwrap();
        writer.append_string("sym").unwrap();
        for i in 0..256 {
            writer.append_frame(&tick(i, i as f64)).unwrap();
        }
    }

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_success(Command::new(exe).args([
        "convert",
        "v2",
        v1_path.to_str().unwrap(),
        "columnar-delta",
        v2_path.to_str().unwrap(),
        "4KiB",
        "zstd",
        "verify",
    ]));

    let mut reader = fwob_v2::Reader::open(&v2_path).unwrap();
    reader.verify().unwrap();
    assert_eq!(
        reader.read_page_header(0).unwrap().encoding,
        fwob_v2::Encoding::ColumnarDeltaV1
    );
    assert_eq!(
        reader
            .frames_between(fwob_core::Key::I32(0), fwob_core::Key::I32(255))
            .unwrap()
            .len(),
        256
    );
}

#[test]
fn cli_convert_accepts_plain_tokens_in_arbitrary_positions() {
    let dir = tempdir().unwrap();
    let v1_path = dir.path().join("input.fwob");
    let v2_path = dir.path().join("plain_tokens.fwob");

    {
        let mut writer =
            V1Writer::create(&v1_path, tick_schema(), WriterOptions::new("HelloFwob")).unwrap();
        for i in 0..128 {
            writer.append_frame(&tick(i, i as f64)).unwrap();
        }
    }

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_success(Command::new(exe).args([
        "convert",
        "zstd",
        v1_path.to_str().unwrap(),
        "row-raw",
        v2_path.to_str().unwrap(),
        "tight-fit",
        "verify",
        "4KiB",
    ]));

    let mut reader = fwob_v2::Reader::open(&v2_path).unwrap();
    reader.verify().unwrap();
    assert_eq!(
        reader.read_page_header(0).unwrap().encoding,
        fwob_v2::Encoding::RowRawV1
    );
}

#[test]
fn cli_convert_treats_prefixed_reserved_word_as_path() {
    let dir = tempdir().unwrap();
    let input_path = dir.path().join("row-raw");
    let output_path = dir.path().join("out.fwob");

    {
        let mut writer =
            V1Writer::create(&input_path, tick_schema(), WriterOptions::new("HelloFwob")).unwrap();
        writer.append_frame(&tick(1, 1.0)).unwrap();
    }

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_success(Command::new(exe).current_dir(dir.path()).args([
        "convert",
        "./row-raw",
        output_path.to_str().unwrap(),
        "uncompressed",
    ]));

    let reader = fwob_v2::Reader::open(&output_path).unwrap();
    assert_eq!(reader.header().frame_count, 1);
}

#[test]
fn cli_convert_rejects_duplicate_plain_tokens() {
    let dir = tempdir().unwrap();
    let v1_path = dir.path().join("input.fwob");
    let v2_path = dir.path().join("out.fwob");

    {
        let mut writer =
            V1Writer::create(&v1_path, tick_schema(), WriterOptions::new("HelloFwob")).unwrap();
        writer.append_frame(&tick(1, 1.0)).unwrap();
    }

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_failure(
        Command::new(exe).args([
            "convert",
            v1_path.to_str().unwrap(),
            v2_path.to_str().unwrap(),
            "zstd",
            "lz4",
        ]),
        "duplicate codec token",
    );
}

#[test]
fn cli_convert_v1_rejects_v2_write_tokens() {
    let dir = tempdir().unwrap();
    let input_path = dir.path().join("input.fwob");
    let output_path = dir.path().join("out.fwob");

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_failure(
        Command::new(exe).args([
            "convert",
            "v1",
            input_path.to_str().unwrap(),
            output_path.to_str().unwrap(),
            "zstd",
        ]),
        "v2 write tokens are not valid when converting to v1",
    );
}

#[test]
fn cli_bench_conversion_matrix_runs_all_supported_cases_and_cleans_outputs() {
    let dir = tempdir().unwrap();
    let v1_path = dir.path().join("input.fwob");

    {
        let mut writer =
            V1Writer::create(&v1_path, tick_schema(), WriterOptions::new("HelloFwob")).unwrap();
        writer.append_string("sym").unwrap();
        for i in 0..64 {
            writer.append_frame(&tick(i, i as f64)).unwrap();
        }
    }

    let exe = env!("CARGO_BIN_EXE_fwob");
    let output = command_output(Command::new(exe).args([
        "bench",
        v1_path.to_str().unwrap(),
        "--output-dir",
        dir.path().to_str().unwrap(),
        "--iterations",
        "1",
        "--scan-iterations",
        "1",
    ]));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("mode: conversion-matrix"));
    assert!(stdout.contains("cases: 99"));
    assert!(stdout.contains("[conversion_matrix_dimensions]"));
    assert!(stdout.contains("page_size (3): 512KiB (33 cases), 1MiB (33 cases), 2MiB (33 cases)"));
    assert!(stdout.contains("codec (3): zstd (72 cases), lz4 (18 cases), uncompressed (9 cases)"));
    assert!(stdout.contains(
        "zstd_level (4; zstd only): 3 (18 cases), 6 (18 cases), 9 (18 cases), 12 (18 cases)"
    ));
    assert!(stdout.contains(
        "encoding (3): row-raw (33 cases), columnar-basic (33 cases), columnar-delta (33 cases)"
    ));
    assert!(stdout.contains("page_packing (2): estimate-shrink (54 cases), tight-fit (45 cases)"));
    assert!(stdout.contains("excluded: codec=uncompressed + page_packing=tight-fit"));
    assert!(stdout.contains("conditional: zstd_level applies only to codec=zstd"));
    assert!(stdout.contains("[conversion_matrix_test_runs]"));
    assert!(stdout.contains("conversion: 99"));
    assert!(stdout.contains("random_page: 99 cases x 1 iterations = 99 reads"));
    assert!(stdout.contains("scan: 99 cases x 1 iterations = 99 scans"));
    assert!(stdout.contains("range: 99 cases x 1 iterations = 99 queries"));
    assert!(stdout.contains("[conversion_matrix_summary]"));
    assert!(stdout.contains("convert_s"));
    assert!(stdout.contains("random_ms"));
    assert!(stdout.contains("scan_avg_ms"));
    assert!(stdout.contains("range_ms"));
    assert!(stdout.contains("[conversion_matrix_storage]"));
    assert!(stdout.contains("compressed_bytes"));
    assert!(stdout.contains("avg_frames_compressed_page"));
    assert!(stdout.contains("[conversion_matrix_read_samples]"));
    assert!(stdout.contains("random_iterations"));
    assert!(stdout.contains("range_iterations"));
    assert!(stdout.contains("[conversion_matrix_packing]"));
    assert!(stdout.contains("subseq_attempt_range"));
    assert!(stderr.contains("test=conversion started"));
    assert!(stderr.contains("test=conversion completed"));
    assert!(stderr.contains("test=metadata started"));
    assert!(stderr.contains("test=metadata completed"));
    assert!(stderr.contains("test=random-page started"));
    assert!(stderr.contains("test=random-page completed"));
    assert!(stderr.contains("test=scan started"));
    assert!(stderr.contains("test=scan completed"));
    assert!(stderr.contains("test=range started"));
    assert!(stderr.contains("test=range completed"));

    let leftover_outputs = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".bench."))
        .count();
    assert_eq!(leftover_outputs, 0);
}

#[test]
fn cli_creates_blank_v2_from_template_with_new_title() {
    let dir = tempdir().unwrap();
    let template_path = dir.path().join("template.fwob");
    let output_path = dir.path().join("blank.fwob");

    {
        let mut writer = V1Writer::create(
            &template_path,
            tick_schema(),
            WriterOptions::new("Template"),
        )
        .unwrap();
        writer.append_string("sym").unwrap();
        writer.append_frame(&tick(1, 1.0)).unwrap();
    }

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_success(Command::new(exe).args([
        "create",
        "v2",
        output_path.to_str().unwrap(),
        "--template",
        template_path.to_str().unwrap(),
        "--title",
        "Blank",
    ]));

    let reader = fwob_v2::Reader::open(&output_path).unwrap();
    assert_eq!(reader.header().title, "Blank");
    assert_eq!(reader.header().frame_count, 0);
    assert_eq!(reader.header().schema, tick_schema());
    assert_eq!(reader.header().string_table, vec!["sym".to_string()]);
}

#[test]
fn cli_creates_blank_v2_from_schema_args() {
    let dir = tempdir().unwrap();
    let output_path = dir.path().join("blank.fwob");

    let exe = env!("CARGO_BIN_EXE_fwob");
    assert_command_success(Command::new(exe).args([
        "create",
        "v2",
        output_path.to_str().unwrap(),
        "--title",
        "Blank",
        "--frame-type",
        "Tick",
        "--field",
        "Time:i:4",
        "--field",
        "Value:f:8",
        "--field",
        "Str:utf8:4",
    ]));

    let reader = fwob_v2::Reader::open(&output_path).unwrap();
    assert_eq!(reader.header().title, "Blank");
    assert_eq!(reader.header().frame_count, 0);
    assert_eq!(reader.header().page_size, fwob_v2::DEFAULT_PAGE_SIZE);
    assert_eq!(reader.header().schema, tick_schema());
    assert!(reader.header().string_table.is_empty());
}

#[test]
fn cli_accepts_bounded_page_size_tokens_with_all_supported_suffixes() {
    let dir = tempdir().unwrap();
    let exe = env!("CARGO_BIN_EXE_fwob");
    let cases = [
        ("1024B", 1024),
        ("2KB", 2000),
        ("1KiB", 1024),
        ("1MB", 1_000_000),
        ("1MiB", 1024 * 1024),
        ("16MiB", 16 * 1024 * 1024),
    ];

    for (index, (token, expected)) in cases.into_iter().enumerate() {
        let output_path = dir.path().join(format!("blank-{index}.fwob"));
        assert_command_success(Command::new(exe).args([
            "create",
            "v2",
            token,
            output_path.to_str().unwrap(),
            "--frame-type",
            "Tick",
            "--field",
            "Time:i:4",
        ]));
        let reader = fwob_v2::Reader::open(&output_path).unwrap();
        assert_eq!(reader.header().page_size, expected, "token {token}");
    }

    for token in ["1023B", "1KB", "17MiB"] {
        let output_path = dir.path().join(format!("invalid-{token}.fwob"));
        assert_command_failure(
            Command::new(exe).args([
                "create",
                "v2",
                token,
                output_path.to_str().unwrap(),
                "--frame-type",
                "Tick",
                "--field",
                "Time:i:4",
            ]),
            "page size must be between 1KiB and 16MiB",
        );
    }
}

#[test]
fn cli_create_without_template_requires_schema_args() {
    let dir = tempdir().unwrap();
    let output_path = dir.path().join("blank.fwob");

    let exe = env!("CARGO_BIN_EXE_fwob");
    let output = Command::new(exe)
        .args(["create", output_path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--frame-type is required"));
}

#[test]
fn cli_inspect_prints_frame_preview() {
    let dir = tempdir().unwrap();
    let v1_path = dir.path().join("input.fwob");

    {
        let mut writer =
            V1Writer::create(&v1_path, tick_schema(), WriterOptions::new("Preview")).unwrap();
        for i in 0..8 {
            writer.append_frame(&tick(i, i as f64)).unwrap();
        }
    }

    let exe = env!("CARGO_BIN_EXE_fwob");
    let output = command_output(Command::new(exe).args(["inspect", v1_path.to_str().unwrap()]));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[frames]"));
    assert!(stdout.lines().any(|line| {
        line.contains("index")
            && line.contains("Time")
            && line.contains("Value")
            && line.contains("Str")
    }));
    assert!(stdout
        .lines()
        .any(|line| line.split_whitespace().eq(["...", "...", "...", "..."])));
}
