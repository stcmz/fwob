use std::{
    fs::File,
    io::{IsTerminal, Read},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use fwob_core::{Field, FieldType, Key, Schema};
use fwob_v2::{Codec, CodecSelection, Encoding, EncodingSelection, PagePacking};

#[derive(Debug, Parser)]
#[command(name = "fwob")]
#[command(version)]
#[command(about = "FWOB v1/v2 inspection, verification, and conversion tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create an empty v1 or v2 file from a template schema or explicit schema
    /// parameters.
    Create(CreateArgs),
    /// Inspect a v1 or v2 file. v1 inputs need --key-field-index when the key is
    /// not field 0.
    Inspect(AutoFileArgs),
    /// Verify a v1 or v2 file. v1 inputs need --key-field-index when the key is
    /// not field 0.
    Verify(AutoFileArgs),
    /// Benchmark conversion and read performance for v1 or v2 inputs.
    Bench(BenchArgs),
    /// Convert between v1 and v2. Target format defaults to v2. Plain tokens
    /// like zstd, row-raw, tight-fit, and verify may appear anywhere.
    Convert(ConvertArgs),
    /// Append frames from a v1 input file into an existing v2 target file.
    Append(AppendArgs),
    /// Split a v1 or v2 file at key lower bounds.
    Split(SplitArgs),
    /// Concatenate ordered, compatible v1 or v2 files.
    Concat(ConcatArgs),
    /// Rewrite title or string-table metadata without changing frames.
    Edit(EditArgs),
    /// Find all frames or the union of exact keys and inclusive key ranges.
    Find(FindArgs),
    /// Stream selected frame data in raw, table, Markdown, CSV, JSON Lines, or
    /// hexadecimal form.
    Dump(DumpArgs),
    /// Delete frames matching one key or an inclusive key range.
    Delete(DeleteArgs),
}

const FRAME_PREVIEW_COUNT: usize = 3;

#[derive(Debug, Args)]
struct V1FileArgs {
    /// v1 input file.
    path: PathBuf,

    /// Key field index for v1 files. v1 does not store this in metadata.
    #[arg(long, default_value_t = 0)]
    key_field_index: usize,
}

#[derive(Debug, Args)]
struct AutoFileArgs {
    /// Input FWOB file. The command auto-detects v1 or v2.
    path: PathBuf,

    /// Key field index for v1 inputs only. Ignored for v2 because v2 stores the
    /// key field index in metadata.
    #[arg(long, default_value_t = 0)]
    key_field_index: usize,
}

#[derive(Debug, Args)]
struct V2FileArgs {
    /// v2 input file.
    path: PathBuf,
}

#[derive(Debug, Args)]
#[command(override_usage = "fwob create [OPTIONS] [TOKENS] OUTPUT")]
#[command(after_help = "Plain tokens:
  formats: v1, v2
  v2 page size: INTEGER{B|KB|KiB|MB|MiB} (1KiB..16MiB; default 512KiB)

Tokens may appear before or after OUTPUT. The default format is v2. Reserved tokens win on exact match; use ./v1 for a file named v1.")]
struct CreateArgs {
    /// Create target. Forms: `OUTPUT`, `v1 OUTPUT`, or `v2 OUTPUT`. If the
    /// format is omitted, the command creates v2.
    #[arg(value_name = "TARGET", num_args = 1..)]
    target: Vec<String>,

    /// Existing v1 or v2 file to copy schema from. Also copies the string table.
    /// When omitted, --frame-type and at least one --field are required.
    #[arg(long)]
    template: Option<PathBuf>,

    /// Title stored in the new file. Defaults to the template title when
    /// --template is used, otherwise the output file stem.
    #[arg(long)]
    title: Option<String>,

    /// Frame type name for explicit schema creation. Applies only when
    /// --template is omitted.
    #[arg(long)]
    frame_type: Option<String>,

    /// Field definition used when --template is omitted: name:type:length.
    /// Types: i, u, f, utf8, string-index.
    #[arg(long = "field")]
    fields: Vec<String>,

    /// Key field index for explicit schema creation and v1 template reads. For
    /// v2 templates, the stored key field index is used.
    #[arg(long, default_value_t = 0)]
    key_field_index: usize,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TargetFormat {
    V1,
    V2,
}

#[derive(Debug, Args)]
#[command(override_usage = "fwob bench [OPTIONS] [MODE] PATH")]
#[command(after_help = "Plain tokens:
  modes: conversion-matrix, range, random-page, scan

The mode token may appear before or after PATH. The default is conversion-matrix. Reserved tokens win on exact match; use ./scan for a file named scan.")]
struct BenchArgs {
    /// Benchmark target. Forms: `PATH`, `conversion-matrix PATH`,
    /// `range PATH`, `random-page PATH`, or `scan PATH`. If the mode is
    /// omitted, the command uses conversion-matrix.
    #[arg(value_name = "TARGET", num_args = 1..)]
    target: Vec<String>,

    /// Repetitions for read benchmarks. Applies to range and random-page.
    #[arg(long, default_value_t = 1000)]
    iterations: u64,

    /// Inclusive lower key for range mode. Currently supports i32 key fields.
    #[arg(long)]
    first_key_i32: Option<i32>,

    /// Inclusive upper key for range mode. Currently supports i32 key fields.
    #[arg(long)]
    last_key_i32: Option<i32>,

    /// Key field index for v1 inputs only. v2 stores the key field index in
    /// file metadata.
    #[arg(long, default_value_t = 0)]
    key_field_index: usize,

    /// Directory for temporary v2 files produced by conversion-matrix. Defaults
    /// to the input file directory.
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Full scan repetitions. Applies to scan mode and each conversion-matrix
    /// case's scan read test.
    #[arg(long, default_value_t = 3)]
    scan_iterations: u64,

    /// Keep conversion-matrix output files instead of deleting each file after
    /// its read tests complete.
    #[arg(long)]
    keep_outputs: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BenchMode {
    ConversionMatrix,
    Range,
    RandomPage,
    Scan,
}

impl BenchMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::ConversionMatrix => "conversion-matrix",
            Self::Range => "range",
            Self::RandomPage => "random-page",
            Self::Scan => "scan",
        }
    }
}

struct ResolvedBenchArgs {
    path: PathBuf,
    mode: BenchMode,
    iterations: u64,
    first_key_i32: Option<i32>,
    last_key_i32: Option<i32>,
    key_field_index: usize,
    output_dir: Option<PathBuf>,
    scan_iterations: u64,
    keep_outputs: bool,
}

#[derive(Debug, Args)]
#[command(override_usage = "fwob convert [OPTIONS] [TOKENS] INPUT OUTPUT")]
#[command(after_help = "Plain tokens:
  formats: v1, v2
  codecs: zstd, lz4, smallest, uncompressed
  encodings: row-raw, columnar-basic, columnar-delta, smallest
  page packing: estimate-shrink, tight-fit
  page size: INTEGER{B|KB|KiB|MB|MiB} (1KiB..16MiB; default 512KiB)
  switches: verify, compress-partial-page

Tokens may appear anywhere. Reserved tokens win on exact match; use ./row-raw for a file named row-raw.")]
struct ConvertArgs {
    /// Convert target and plain tokens. Forms: `INPUT OUTPUT`, `v1 INPUT OUTPUT`,
    /// or `v2 INPUT OUTPUT`. Plain tokens such as zstd, row-raw, tight-fit, and
    /// verify may appear anywhere.
    #[arg(value_name = "TARGET", num_args = 1..)]
    target: Vec<String>,

    /// Key field index for a v1 input. Applies when converting to v2 because v1
    /// does not store this in metadata.
    #[arg(long, default_value_t = 0)]
    key_field_index: usize,

    #[command(flatten)]
    write: V2WriteArgs,
}

#[derive(Debug, Clone, Copy, Args)]
struct V2WriteArgs {
    /// zstd compression level for newly written zstd pages. Applies only when
    /// zstd or smallest can choose zstd.
    #[arg(long, default_value_t = fwob_v2::DEFAULT_ZSTD_LEVEL)]
    zstd_level: i32,
}

#[derive(Debug, Args)]
#[command(override_usage = "fwob append [OPTIONS] TARGET INPUT [TOKENS]")]
#[command(after_help = "Plain tokens:
  codecs: zstd, lz4, smallest, uncompressed
  encodings: row-raw, columnar-basic, columnar-delta, smallest
  page packing: estimate-shrink, tight-fit
  switches: verify, compress-partial-page

Tokens may appear anywhere. Reserved tokens win on exact match; use ./row-raw for a file named row-raw.")]
struct AppendArgs {
    /// Existing v2 target, input file, and plain tokens such as zstd, row-raw,
    /// tight-fit, verify, and compress-partial-page.
    #[arg(value_name = "TARGET", num_args = 2..)]
    target: Vec<String>,

    /// Key field index for the v1 input. v1 does not store this in metadata.
    #[arg(long, default_value_t = 0)]
    key_field_index: usize,

    #[command(flatten)]
    write: V2WriteArgs,
}

#[derive(Debug, Args)]
#[command(override_usage = "fwob split [OPTIONS] INPUT OUTPUT_DIR FIRST_KEY... [TOKENS]")]
#[command(after_help = "V2 output tokens:
  codecs: zstd, lz4, smallest, uncompressed
  encodings: row-raw, columnar-basic, columnar-delta, smallest
  page packing: estimate-shrink, tight-fit
  switches: compress-partial-page

V2 parts preserve the source page size. Without write tokens, codec and encoding
are inherited from the source. Tokens may appear anywhere.")]
struct SplitArgs {
    /// Input, output directory, split keys, and optional v2 output tokens.
    #[arg(value_name = "TARGET", num_args = 3..)]
    target: Vec<String>,
    /// Key field index for v1 input only.
    #[arg(long, default_value_t = 0)]
    key_field_index: usize,
    /// Emit empty parts when adjacent keys resolve to the same frame index.
    #[arg(long)]
    keep_empty_parts: bool,
    /// zstd level for v2 output pages.
    #[arg(long)]
    zstd_level: Option<i32>,
}

#[derive(Debug, Args)]
#[command(override_usage = "fwob concat [OPTIONS] OUTPUT INPUT... [TOKENS]")]
#[command(after_help = "V2 output tokens:
  codecs: zstd, lz4, smallest, uncompressed
  encodings: row-raw, columnar-basic, columnar-delta, smallest
  page packing: estimate-shrink, tight-fit
  switches: compress-partial-page

V2 output preserves the first source page size. Without write tokens, codec and
encoding are inherited from the first source. Tokens may appear anywhere.")]
struct ConcatArgs {
    /// Output, ordered input files, and optional v2 output tokens.
    #[arg(value_name = "TARGET", num_args = 2..)]
    target: Vec<String>,
    /// Key field index for v1 inputs only.
    #[arg(long, default_value_t = 0)]
    key_field_index: usize,
    /// zstd level for v2 output pages.
    #[arg(long)]
    zstd_level: Option<i32>,
}

#[derive(Debug, Args)]
struct EditArgs {
    /// Input v1 or v2 file to rewrite atomically.
    path: PathBuf,
    /// Replacement title.
    #[arg(long)]
    title: Option<String>,
    /// Append a string-table value. May be repeated.
    #[arg(long = "append-string")]
    append_strings: Vec<String>,
    /// Clear the string table before applying appended values.
    #[arg(long)]
    clear_strings: bool,
    /// Key field index for v1 input only.
    #[arg(long, default_value_t = 0)]
    key_field_index: usize,
}

#[derive(Debug, Args)]
struct FindArgs {
    /// Input v1 or v2 file.
    path: PathBuf,
    /// Selectors: KEY, FIRST.., ..LAST, or FIRST..LAST. May be mixed,
    /// reordered, duplicated, or omitted to select all frames.
    selectors: Vec<String>,
    /// Key field index for v1 input only.
    #[arg(long, default_value_t = 0)]
    key_field_index: usize,
}

#[derive(Debug, Args)]
#[command(override_usage = "fwob dump [OPTIONS] PATH [SELECTOR...] [FORMAT]")]
#[command(after_help = "Plain tokens:
  selectors: KEY, FIRST.., ..LAST, FIRST..LAST
  formats: raw (default), table, md, csv, jsonl, hex

Selectors may be mixed, reordered, duplicated, or omitted to stream all frames.
Overlapping selectors are silently unioned. The format token may appear among
the selectors. Output is written to stdout; diagnostics are written to stderr.")]
struct DumpArgs {
    /// Input v1 or v2 file.
    path: PathBuf,
    /// Mixed selectors and one optional output-format token.
    #[arg(value_name = "SELECTOR_OR_FORMAT")]
    target: Vec<String>,
    /// Key field index for v1 input only.
    #[arg(long, default_value_t = 0)]
    key_field_index: usize,
}

#[derive(Debug, Args)]
#[command(override_usage = "fwob delete [OPTIONS] PATH FIRST_KEY [LAST_KEY] [TOKENS]")]
#[command(after_help = "Plain tokens:
  deletion packing: local-repack (default), repack-to-end
  codecs: zstd, lz4, smallest, uncompressed
  encodings: row-raw, columnar-basic, columnar-delta, smallest
  page packing: estimate-shrink, tight-fit
  switches: verify, compress-partial-page

FIRST_KEY alone deletes every equal key. FIRST_KEY LAST_KEY deletes the inclusive range.
compress-partial-page applies to the final EOF remainder in repack-to-end mode.
Tokens may appear anywhere. Reserved tokens win on exact match.")]
struct DeleteArgs {
    /// File, one key or key range, and optional v2 mutation tokens.
    #[arg(value_name = "TARGET", num_args = 2..)]
    target: Vec<String>,
    /// Key field index for v1 input only.
    #[arg(long, default_value_t = 0)]
    key_field_index: usize,
    /// zstd level for rewritten pages. Supplying any v2 write setting selects
    /// explicit mutation settings instead of inheriting the affected page.
    #[arg(long)]
    zstd_level: Option<i32>,
}

#[derive(Debug, Clone, Copy)]
struct V2WriteOptions {
    codec: CodecArg,
    encoding: EncodingArg,
    zstd_level: i32,
    compress_partial_page: bool,
    page_packing: PagePackingArg,
    verify: bool,
}

impl V2WriteOptions {
    fn from_args(args: V2WriteArgs) -> Self {
        Self {
            codec: CodecArg::Zstd,
            encoding: EncodingArg::ColumnarBasic,
            zstd_level: args.zstd_level,
            compress_partial_page: false,
            page_packing: PagePackingArg::EstimateShrink,
            verify: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CodecArg {
    Uncompressed,
    Zstd,
    Lz4,
    Smallest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum EncodingArg {
    RowRaw,
    ColumnarBasic,
    ColumnarDelta,
    Smallest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PagePackingArg {
    EstimateShrink,
    TightFit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeletionPackingArg {
    LocalRepack,
    RepackToEnd,
}

impl DeletionPackingArg {
    fn deletion_packing(self) -> fwob::DeletionPacking {
        match self {
            Self::LocalRepack => fwob::DeletionPacking::LocalRepack,
            Self::RepackToEnd => fwob::DeletionPacking::RepackToEnd,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::LocalRepack => "local-repack",
            Self::RepackToEnd => "repack-to-end",
        }
    }
}

impl PagePackingArg {
    fn page_packing(self) -> PagePacking {
        match self {
            Self::EstimateShrink => PagePacking::EstimateShrink,
            Self::TightFit => PagePacking::TightFit,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::EstimateShrink => "estimate-shrink",
            Self::TightFit => "tight-fit",
        }
    }
}

impl EncodingArg {
    fn encoding(self) -> Encoding {
        match self {
            Self::RowRaw => Encoding::RowRawV1,
            Self::ColumnarBasic => Encoding::ColumnarBasicV1,
            Self::ColumnarDelta | Self::Smallest => Encoding::ColumnarDeltaV1,
        }
    }

    fn selection(self) -> EncodingSelection {
        match self {
            Self::RowRaw => EncodingSelection::Fixed(Encoding::RowRawV1),
            Self::ColumnarBasic => EncodingSelection::Fixed(Encoding::ColumnarBasicV1),
            Self::ColumnarDelta => EncodingSelection::Fixed(Encoding::ColumnarDeltaV1),
            Self::Smallest => EncodingSelection::Smallest,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::RowRaw => "row-raw",
            Self::ColumnarBasic => "columnar-basic",
            Self::ColumnarDelta => "columnar-delta",
            Self::Smallest => "smallest",
        }
    }
}

impl CodecArg {
    fn codec(self) -> Codec {
        match self {
            CodecArg::Uncompressed => Codec::None,
            CodecArg::Zstd | CodecArg::Smallest => Codec::Zstd,
            CodecArg::Lz4 => Codec::Lz4,
        }
    }

    fn selection(self) -> CodecSelection {
        match self {
            CodecArg::Uncompressed => CodecSelection::Fixed(Codec::None),
            CodecArg::Zstd => CodecSelection::Fixed(Codec::Zstd),
            CodecArg::Lz4 => CodecSelection::Fixed(Codec::Lz4),
            CodecArg::Smallest => CodecSelection::Smallest,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Uncompressed => "uncompressed",
            Self::Zstd => "zstd",
            Self::Lz4 => "lz4",
            Self::Smallest => "smallest",
        }
    }
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Create(args) => create_blank(args),
        Command::Inspect(args) => inspect_auto(args),
        Command::Verify(args) => verify_auto(args),
        Command::Bench(args) => bench_v2(args),
        Command::Convert(args) => convert(args),
        Command::Append(args) => append_to_v2(args),
        Command::Split(args) => split_file(args),
        Command::Concat(args) => concat_file(args),
        Command::Edit(args) => edit_file(args),
        Command::Find(args) => find_frames(args),
        Command::Dump(args) => dump_frames(args),
        Command::Delete(args) => delete_frames(args),
    }
}

fn dump_frames(args: DumpArgs) -> Result<()> {
    let mut format = None;
    let mut selector_values = Vec::new();
    for value in &args.target {
        if let Some(parsed) = fwob::formatting::FrameFormat::parse(value) {
            if format.replace(parsed).is_some() {
                bail!("dump accepts at most one output format token");
            }
        } else {
            selector_values.push(value);
        }
    }

    let reader_options = fwob::ReaderOptions {
        v1_key_field_index: args.key_field_index,
    };
    let mut reader = fwob::Reader::open_with_options(&args.path, reader_options)?;
    let schema = reader.schema().clone();
    let string_table = reader.string_table().to_vec();
    let key_type = fwob_core::KeyType::from_field(schema.key_field())?;
    let selectors = selector_values
        .into_iter()
        .map(|value| fwob::KeySelector::parse(value, key_type))
        .collect::<fwob::Result<Vec<_>>>()?;
    let selection = fwob::FrameSelection::resolve(&mut reader, &selectors)?;
    let mut formatter = fwob::formatting::FrameFormatter::new(
        &schema,
        &string_table,
        format.unwrap_or(fwob::formatting::FrameFormat::Raw),
    );
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    formatter.write_header(&mut output)?;
    for range in selection.ranges() {
        for frame in reader.frames(range.clone())? {
            formatter.write_frame(&mut output, frame?.bytes())?;
        }
    }
    Ok(())
}

fn find_frames(args: FindArgs) -> Result<()> {
    let reader_options = fwob::ReaderOptions {
        v1_key_field_index: args.key_field_index,
    };
    let mut reader = fwob::Reader::open_with_options(&args.path, reader_options)?;
    let schema = reader.schema().clone();
    let key_type = fwob_core::KeyType::from_field(schema.key_field())?;
    let selectors = args
        .selectors
        .iter()
        .map(|value| fwob::KeySelector::parse(value, key_type))
        .collect::<fwob::Result<Vec<_>>>()?;
    let selection = fwob::FrameSelection::resolve(&mut reader, &selectors)?;
    let rows = selection_preview_rows(&mut reader, &selection)?;

    toml_section("find");
    toml_kv_str("path", &args.path.display().to_string());
    toml_kv_num("selector_count", selectors.len());
    toml_kv_num("range_count", selection.ranges().len());
    if let Some(start) = selection.first_index() {
        toml_kv_num("start_index", start);
        toml_kv_num("end_index", selection.end_index().unwrap());
    }
    toml_kv_num("frame_count", selection.frame_count());
    if !rows.is_empty() {
        println!();
        toml_section("frames");
        toml_kv_multiline("preview", &format_frame_preview_rows(&schema, &rows));
    }
    Ok(())
}

fn selection_preview_rows(
    reader: &mut fwob::Reader,
    selection: &fwob::FrameSelection,
) -> Result<Vec<PreviewRow>> {
    let count = selection.frame_count();
    let positions = preview_indices(count);
    let mut rows = Vec::with_capacity(positions.len());
    for position in positions {
        match position {
            PreviewIndex::Ellipsis => rows.push(PreviewRow::Ellipsis),
            PreviewIndex::Frame(position) => {
                let mut remaining = position;
                let mut global_index = None;
                for range in selection.ranges() {
                    let len = range.end - range.start;
                    if remaining < len {
                        global_index = Some(range.start + remaining);
                        break;
                    }
                    remaining -= len;
                }
                let global_index =
                    global_index.context("selected preview index is out of range")?;
                let frame = reader
                    .read_frame(global_index)?
                    .context("matched frame index is out of range")?;
                rows.push(PreviewRow::Frame(global_index, frame.bytes().to_vec()));
            }
        }
    }
    Ok(rows)
}

fn delete_frames(args: DeleteArgs) -> Result<()> {
    let parsed = parse_command_tokens(&args.target, false, true, false, false, true)?;
    let deletion_packing = parsed
        .deletion_packing
        .unwrap_or(DeletionPackingArg::LocalRepack);
    let ([path, first_key] | [path, first_key, _]) = parsed.paths.as_slice() else {
        bail!("delete expects PATH FIRST_KEY or PATH FIRST_KEY LAST_KEY after tokens");
    };
    let last_key = parsed.paths.get(2).copied();
    let path = PathBuf::from(path);
    let (operation_options, write) = parsed.operation_options(
        args.key_field_index,
        args.zstd_level,
        deletion_packing.deletion_packing(),
        matches!(deletion_packing, DeletionPackingArg::LocalRepack),
    );
    let reader_options = operation_options.reader_options;
    let reader = fwob::Reader::open_with_options(&path, reader_options)?;
    let key_type = fwob_core::KeyType::from_field(reader.schema().key_field())?;
    let first_key = Key::parse(key_type, first_key)?;
    let last_key_value = last_key
        .map(|value| Key::parse(key_type, value))
        .transpose()?
        .unwrap_or(first_key);
    if first_key > last_key_value {
        bail!("FIRST_KEY must be less than or equal to LAST_KEY");
    }
    drop(reader);

    let effective_compress_partial_page =
        matches!(deletion_packing, DeletionPackingArg::LocalRepack) || write.compress_partial_page;
    let mut editor = fwob::Editor::open_with_operation_options(&path, operation_options)?;
    let removed = if first_key == last_key_value {
        editor.delete_key(first_key)?
    } else {
        editor.delete_key_range(first_key..=last_key_value)?
    };
    if write.verify {
        fwob::Maintenance::verify(&path, reader_options)?;
    }

    toml_section("deletion");
    toml_kv_str("path", &path.display().to_string());
    toml_kv_key("first_key", first_key);
    toml_kv_key("last_key", last_key_value);
    toml_kv_num("deleted_frames", removed);
    toml_kv_num("remaining_frames", editor.frame_count());
    toml_kv_str("deletion_packing", deletion_packing.as_str());
    toml_kv_bool("compress_partial_page", effective_compress_partial_page);
    toml_kv_bool("verified", write.verify);
    Ok(())
}

fn split_file(args: SplitArgs) -> Result<()> {
    use fwob::{Organizer, Reader};

    let parsed = parse_command_tokens(&args.target, false, true, false, false, false)?;
    if parsed.paths.len() < 3 {
        bail!("split expects INPUT OUTPUT_DIR and at least one FIRST_KEY after tokens");
    }
    let input = PathBuf::from(parsed.paths[0]);
    let output_dir = PathBuf::from(parsed.paths[1]);
    let (operation_options, _) = parsed.operation_options(
        args.key_field_index,
        args.zstd_level,
        fwob::DeletionPacking::LocalRepack,
        false,
    );
    let reader = Reader::open_with_options(&input, operation_options.reader_options)?;
    let key_type = fwob_core::KeyType::from_field(reader.schema().key_field())?;
    let keys = parsed.paths[2..]
        .iter()
        .map(|value| Key::parse(key_type, value).map_err(Into::into))
        .collect::<Result<Vec<_>>>()?;
    drop(reader);
    let outputs = Organizer {
        operation_options,
        keep_empty_parts: args.keep_empty_parts,
    }
    .split(&input, &output_dir, &keys)?;
    println!("[split]");
    println!("parts = {}", outputs.len());
    for (index, path) in outputs.iter().enumerate() {
        println!("part_{index} = {:?}", path.display().to_string());
    }
    Ok(())
}

fn concat_file(args: ConcatArgs) -> Result<()> {
    let parsed = parse_command_tokens(&args.target, false, true, false, false, false)?;
    if parsed.paths.len() < 2 {
        bail!("concat expects OUTPUT and at least one INPUT after tokens");
    }
    let output = PathBuf::from(parsed.paths[0]);
    let inputs = parsed.paths[1..]
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let (operation_options, _) = parsed.operation_options(
        args.key_field_index,
        args.zstd_level,
        fwob::DeletionPacking::LocalRepack,
        false,
    );
    let frames = fwob::Organizer {
        operation_options,
        ..Default::default()
    }
    .concat(&output, &inputs)?;
    println!("[concat]");
    println!("frames = {frames}");
    println!("output = {:?}", output.display().to_string());
    Ok(())
}

fn edit_file(args: EditArgs) -> Result<()> {
    use fwob::Editor;

    if args.title.is_none() && args.append_strings.is_empty() && !args.clear_strings {
        bail!("edit requires --title, --append-string, or --clear-strings");
    }
    let mut editor = Editor::open_with_options(
        &args.path,
        fwob::ReaderOptions {
            v1_key_field_index: args.key_field_index,
        },
    )?;
    let strings = if args.clear_strings || !args.append_strings.is_empty() {
        let mut values = if args.clear_strings {
            Vec::new()
        } else {
            editor.string_table().to_vec()
        };
        values.extend(args.append_strings);
        Some(values)
    } else {
        None
    };
    editor.update_metadata(args.title.as_deref(), strings.as_deref())?;
    println!("[edit]");
    println!("title = {:?}", editor.title());
    println!("string_count = {}", editor.string_table().len());
    Ok(())
}

fn inspect_auto(args: AutoFileArgs) -> Result<()> {
    match detect_format(&args.path)? {
        Format::V1 => inspect_v1(V1FileArgs {
            path: args.path,
            key_field_index: args.key_field_index,
        }),
        Format::V2 => inspect_v2(V2FileArgs { path: args.path }),
    }
}

fn verify_auto(args: AutoFileArgs) -> Result<()> {
    match detect_format(&args.path)? {
        Format::V1 => verify_v1(V1FileArgs {
            path: args.path,
            key_field_index: args.key_field_index,
        }),
        Format::V2 => verify_v2(V2FileArgs { path: args.path }),
    }
}

#[derive(Debug, Clone, Copy)]
enum Format {
    V1,
    V2,
}

fn detect_format(path: &PathBuf) -> Result<Format> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    match &magic {
        b"FWOB" => Ok(Format::V1),
        b"FWB2" => Ok(Format::V2),
        _ => bail!("unrecognized FWOB file signature"),
    }
}

fn color_enabled() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

const GITHUB_BLUE: &str = "38;2;121;192;255";
const GITHUB_GREEN: &str = "38;2;165;214;255";
const GITHUB_ORANGE: &str = "38;2;255;166;87";
const GITHUB_PURPLE: &str = "38;2;210;168;255";

fn colorize(value: impl AsRef<str>, code: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{}\x1b[0m", value.as_ref())
    } else {
        value.as_ref().to_string()
    }
}

fn toml_section(name: &str) {
    println!("{}", colorize(format!("[{name}]"), GITHUB_PURPLE));
}

fn toml_array_section(name: &str) {
    println!("{}", colorize(format!("[[{name}]]"), GITHUB_PURPLE));
}

fn toml_key(key: &str) -> String {
    colorize(key, GITHUB_BLUE)
}

fn toml_string(value: &str) -> String {
    colorize(format!("\"{}\"", escape_toml_string(value)), GITHUB_GREEN)
}

fn toml_value(value: impl ToString) -> String {
    colorize(value.to_string(), GITHUB_ORANGE)
}

fn toml_kv_str(key: &str, value: &str) {
    println!("{} = {}", toml_key(key), toml_string(value));
}

fn toml_kv_num(key: &str, value: impl ToString) {
    println!("{} = {}", toml_key(key), toml_value(value));
}

fn toml_kv_bool(key: &str, value: bool) {
    println!("{} = {}", toml_key(key), toml_value(value));
}

fn toml_kv_key(key: &str, value: Key) {
    println!("{} = {}", toml_key(key), toml_value(toml_key_value(value)));
}

fn toml_key_value(key: Key) -> String {
    match key {
        Key::I8(value) => value.to_string(),
        Key::I16(value) => value.to_string(),
        Key::I32(value) => value.to_string(),
        Key::I64(value) => value.to_string(),
        Key::U8(value) => value.to_string(),
        Key::U16(value) => value.to_string(),
        Key::U32(value) => value.to_string(),
        Key::U64(value) => value.to_string(),
        Key::F32(value) => value.to_string(),
        Key::F64(value) => value.to_string(),
        Key::Decimal(value) => value.to_string(),
    }
}

fn toml_kv_multiline(key: &str, value: &str) {
    println!("{} = \"\"\"", toml_key(key));
    print!("{}", escape_toml_multiline(value));
    if !value.ends_with('\n') {
        println!();
    }
    println!("\"\"\"");
}

fn toml_kv_float_array(key: &str, values: &[f64], precision: usize) {
    let values = values
        .iter()
        .map(|value| format!("{value:.precision$}", precision = precision))
        .collect::<Vec<_>>()
        .join(", ");
    println!("{} = {}", toml_key(key), toml_value(format!("[{values}]")));
}

fn escape_toml_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn escape_toml_multiline(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace("\"\"\"", "\\\"\\\"\\\"")
}

fn create_blank(args: CreateArgs) -> Result<()> {
    let (format, output, page_size) = parse_create_target(&args.target)?;
    let (schema, strings, template_title) = if let Some(template) = &args.template {
        read_template_schema(template, args.key_field_index)?
    } else {
        (
            schema_from_create_args(
                args.frame_type.as_deref(),
                &args.fields,
                args.key_field_index,
            )?,
            Vec::new(),
            None,
        )
    };
    let title = args.title.or(template_title).unwrap_or_else(|| {
        output
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("FWOB")
            .to_string()
    });

    match format {
        TargetFormat::V1 => {
            let mut options = fwob_v1::WriterOptions::new(title);
            let estimated_string_bytes: usize = strings.iter().map(|s| s.len() + 5).sum();
            options.string_table_preserved_length = estimated_string_bytes.max(1834) as u32;
            let mut writer = fwob_v1::Writer::create(&output, schema, options)?;
            for value in &strings {
                writer.append_string(value)?;
            }
        }
        TargetFormat::V2 => {
            let mut options = fwob_v2::WriterOptions::new(title);
            options.page_size = page_size;
            options.string_table = strings;
            fwob_v2::Writer::create(&output, schema, options)?.finish()?;
        }
    }

    println!("created {}", output.display());
    Ok(())
}

fn parse_create_target(values: &[String]) -> Result<(TargetFormat, PathBuf, u32)> {
    let mut format = None;
    let mut page_size = None;
    let mut paths = Vec::new();
    for value in values {
        if let Some(parsed) = match_target_format(value) {
            set_once(&mut format, parsed, "format")?;
        } else if let Some(parsed) = parse_page_size_token(value) {
            set_once(&mut page_size, parsed?, "page size")?;
        } else if is_any_reserved_token(value) {
            bail!("token '{value}' is not valid for create");
        } else {
            paths.push(value);
        }
    }
    let format = format.unwrap_or(TargetFormat::V2);
    if matches!(format, TargetFormat::V1) && page_size.is_some() {
        bail!("page size token is not valid when creating v1");
    }
    match paths.as_slice() {
        [output] => Ok((
            format,
            PathBuf::from(output),
            page_size.unwrap_or(fwob_v2::DEFAULT_PAGE_SIZE),
        )),
        [] => bail!("create expects OUTPUT or FORMAT OUTPUT"),
        _ => bail!("create expects exactly one output path"),
    }
}

fn parse_convert_target(
    values: &[String],
    write_args: V2WriteArgs,
) -> Result<(TargetFormat, PathBuf, PathBuf, u32, V2WriteOptions)> {
    let parsed = parse_command_tokens(values, true, true, true, false, false)?;
    let format = parsed.format.unwrap_or(TargetFormat::V2);
    if matches!(format, TargetFormat::V1) && parsed.has_v2_write_tokens() {
        bail!("v2 write tokens are not valid when converting to v1");
    }
    let write = parsed.write_options(write_args);
    match parsed.paths.as_slice() {
        [input, output] => Ok((
            format,
            PathBuf::from(input),
            PathBuf::from(output),
            parsed.page_size.unwrap_or(fwob_v2::DEFAULT_PAGE_SIZE),
            write,
        )),
        _ => bail!("convert expects exactly input and output paths after tokens"),
    }
}

fn resolve_bench_args(args: BenchArgs) -> Result<ResolvedBenchArgs> {
    let (mode, path) = parse_bench_target(&args.target)?;
    Ok(ResolvedBenchArgs {
        path,
        mode,
        iterations: args.iterations,
        first_key_i32: args.first_key_i32,
        last_key_i32: args.last_key_i32,
        key_field_index: args.key_field_index,
        output_dir: args.output_dir,
        scan_iterations: args.scan_iterations,
        keep_outputs: args.keep_outputs,
    })
}

fn parse_bench_target(values: &[String]) -> Result<(BenchMode, PathBuf)> {
    let mut mode = None;
    let mut paths = Vec::new();
    for value in values {
        if let Some(parsed) = match_bench_mode(value) {
            set_once(&mut mode, parsed, "bench mode")?;
        } else if is_any_reserved_token(value) {
            bail!("token '{value}' is not valid for bench");
        } else {
            paths.push(value);
        }
    }
    match paths.as_slice() {
        [path] => Ok((
            mode.unwrap_or(BenchMode::ConversionMatrix),
            PathBuf::from(path),
        )),
        [] => bail!("bench expects PATH or MODE PATH"),
        _ => bail!("bench expects exactly one input path"),
    }
}

fn match_bench_mode(value: &str) -> Option<BenchMode> {
    match value {
        "conversion-matrix" => Some(BenchMode::ConversionMatrix),
        "range" => Some(BenchMode::Range),
        "random-page" => Some(BenchMode::RandomPage),
        "scan" => Some(BenchMode::Scan),
        _ => None,
    }
}

fn match_target_format(value: &str) -> Option<TargetFormat> {
    match value {
        "v1" => Some(TargetFormat::V1),
        "v2" => Some(TargetFormat::V2),
        _ => None,
    }
}

#[derive(Default)]
struct ParsedTokens<'a> {
    paths: Vec<&'a str>,
    format: Option<TargetFormat>,
    codec: Option<CodecArg>,
    encoding: Option<EncodingArg>,
    page_packing: Option<PagePackingArg>,
    deletion_packing: Option<DeletionPackingArg>,
    page_size: Option<u32>,
    verify: bool,
    compress_partial_page: bool,
}

impl ParsedTokens<'_> {
    fn has_v2_write_tokens(&self) -> bool {
        self.codec.is_some()
            || self.encoding.is_some()
            || self.page_packing.is_some()
            || self.page_size.is_some()
            || self.verify
            || self.compress_partial_page
    }

    fn write_options(&self, args: V2WriteArgs) -> V2WriteOptions {
        let mut write = V2WriteOptions::from_args(args);
        if let Some(codec) = self.codec {
            write.codec = codec;
        }
        if let Some(encoding) = self.encoding {
            write.encoding = encoding;
        }
        if let Some(page_packing) = self.page_packing {
            write.page_packing = page_packing;
        }
        write.verify = self.verify;
        write.compress_partial_page = self.compress_partial_page;
        write
    }

    fn has_mutation_write_tokens(&self) -> bool {
        self.codec.is_some()
            || self.encoding.is_some()
            || self.page_packing.is_some()
            || self.compress_partial_page
    }

    fn operation_options(
        &self,
        v1_key_field_index: usize,
        zstd_level: Option<i32>,
        deletion_packing: fwob::DeletionPacking,
        force_compress_partial_page: bool,
    ) -> (fwob::OperationOptions, V2WriteOptions) {
        let explicit_v2_options = self.has_mutation_write_tokens() || zstd_level.is_some();
        let mut write = self.write_options(V2WriteArgs {
            zstd_level: zstd_level.unwrap_or(fwob_v2::DEFAULT_ZSTD_LEVEL),
        });
        write.compress_partial_page |= force_compress_partial_page;
        let v2 = explicit_v2_options.then(|| {
            let mut options = fwob_v2::WriterOptions::new("");
            options.codec = write.codec.codec();
            options.codec_selection = write.codec.selection();
            options.zstd_level = write.zstd_level;
            options.encoding = write.encoding.encoding();
            options.encoding_selection = write.encoding.selection();
            options.compress_partial_page = write.compress_partial_page;
            options.page_packing = write.page_packing.page_packing();
            options
        });
        (
            fwob::OperationOptions {
                reader_options: fwob::ReaderOptions { v1_key_field_index },
                v2,
                deletion_packing,
            },
            write,
        )
    }
}

fn parse_command_tokens<'a>(
    values: &'a [String],
    allow_format: bool,
    allow_write: bool,
    allow_page_size: bool,
    allow_bench: bool,
    allow_deletion_packing: bool,
) -> Result<ParsedTokens<'a>> {
    let mut parsed = ParsedTokens::default();
    let mut seen_verify = false;
    let mut seen_compress_partial_page = false;

    for value in values {
        if allow_format {
            if let Some(format) = match_target_format(value) {
                set_once(&mut parsed.format, format, "format")?;
                continue;
            }
        }
        if allow_bench && match_bench_mode(value).is_some() {
            bail!("bench mode token '{value}' is not valid in this position");
        }
        if let Some(page_size) = parse_page_size_token(value) {
            if !allow_page_size {
                bail!("page size token '{value}' is not valid for this command");
            }
            set_once(&mut parsed.page_size, page_size?, "page size")?;
            continue;
        }
        if allow_write {
            match value.as_str() {
                "uncompressed" => {
                    set_once(&mut parsed.codec, CodecArg::Uncompressed, "codec")?;
                    continue;
                }
                "zstd" => {
                    set_once(&mut parsed.codec, CodecArg::Zstd, "codec")?;
                    continue;
                }
                "lz4" => {
                    set_once(&mut parsed.codec, CodecArg::Lz4, "codec")?;
                    continue;
                }
                "row-raw" => {
                    set_once(&mut parsed.encoding, EncodingArg::RowRaw, "encoding")?;
                    continue;
                }
                "columnar-basic" => {
                    set_once(&mut parsed.encoding, EncodingArg::ColumnarBasic, "encoding")?;
                    continue;
                }
                "columnar-delta" => {
                    set_once(&mut parsed.encoding, EncodingArg::ColumnarDelta, "encoding")?;
                    continue;
                }
                "smallest" => {
                    set_once(&mut parsed.codec, CodecArg::Smallest, "codec")?;
                    set_once(&mut parsed.encoding, EncodingArg::Smallest, "encoding")?;
                    continue;
                }
                "estimate-shrink" => {
                    set_once(
                        &mut parsed.page_packing,
                        PagePackingArg::EstimateShrink,
                        "page packing",
                    )?;
                    continue;
                }
                "tight-fit" => {
                    set_once(
                        &mut parsed.page_packing,
                        PagePackingArg::TightFit,
                        "page packing",
                    )?;
                    continue;
                }
                "verify" => {
                    set_bool_once(&mut seen_verify, "verify")?;
                    parsed.verify = true;
                    continue;
                }
                "compress-partial-page" => {
                    set_bool_once(&mut seen_compress_partial_page, "compress-partial-page")?;
                    parsed.compress_partial_page = true;
                    continue;
                }
                _ => {}
            }
        }
        if allow_deletion_packing {
            match value.as_str() {
                "local-repack" => {
                    set_once(
                        &mut parsed.deletion_packing,
                        DeletionPackingArg::LocalRepack,
                        "deletion packing",
                    )?;
                    continue;
                }
                "repack-to-end" => {
                    set_once(
                        &mut parsed.deletion_packing,
                        DeletionPackingArg::RepackToEnd,
                        "deletion packing",
                    )?;
                    continue;
                }
                _ => {}
            }
        }
        if is_any_reserved_token(value) {
            bail!("token '{value}' is not valid for this command");
        }
        parsed.paths.push(value);
    }

    Ok(parsed)
}

fn set_once<T: Copy>(slot: &mut Option<T>, value: T, name: &str) -> Result<()> {
    if slot.is_some() {
        bail!("duplicate {name} token");
    }
    *slot = Some(value);
    Ok(())
}

fn set_bool_once(seen: &mut bool, name: &str) -> Result<()> {
    if *seen {
        bail!("duplicate {name} token");
    }
    *seen = true;
    Ok(())
}

fn is_any_reserved_token(value: &str) -> bool {
    matches!(
        value,
        "v1" | "v2"
            | "conversion-matrix"
            | "range"
            | "random-page"
            | "scan"
            | "uncompressed"
            | "zstd"
            | "lz4"
            | "smallest"
            | "row-raw"
            | "columnar-basic"
            | "columnar-delta"
            | "estimate-shrink"
            | "tight-fit"
            | "verify"
            | "compress-partial-page"
            | "local-repack"
            | "repack-to-end"
    )
}

fn schema_from_create_args(
    frame_type: Option<&str>,
    fields: &[String],
    key_field_index: usize,
) -> Result<Schema> {
    let frame_type = frame_type.context("--frame-type is required when --template is omitted")?;
    if fields.is_empty() {
        bail!("at least one --field is required when --template is omitted");
    }

    let mut offset = 0u32;
    let mut parsed = Vec::with_capacity(fields.len());
    for field in fields {
        let mut parts = field.split(':');
        let name = parts
            .next()
            .filter(|value| !value.is_empty())
            .context("--field must use name:type:length")?;
        let field_type = parts
            .next()
            .map(parse_field_type)
            .transpose()?
            .context("--field must use name:type:length")?;
        let length = parts
            .next()
            .context("--field must use name:type:length")?
            .parse::<u16>()
            .with_context(|| format!("invalid field length in '{field}'"))?;
        if parts.next().is_some() {
            bail!("--field must use name:type:length");
        }
        if length == 0 {
            bail!("field '{name}' length must be greater than zero");
        }
        parsed.push(Field::new(name, field_type, length, offset));
        offset = offset
            .checked_add(u32::from(length))
            .context("schema frame length overflow")?;
    }

    Schema::new(frame_type, parsed, key_field_index).map_err(Into::into)
}

fn parse_field_type(value: &str) -> Result<FieldType> {
    match value {
        "i" | "int" | "signed" | "signed-integer" => Ok(FieldType::SignedInteger),
        "u" | "uint" | "unsigned" | "unsigned-integer" => Ok(FieldType::UnsignedInteger),
        "f" | "float" | "floating" | "floating-point" => Ok(FieldType::FloatingPoint),
        "utf8" | "utf8-string" | "string" => Ok(FieldType::Utf8String),
        "string-index" | "string-table-index" | "stridx" => Ok(FieldType::StringTableIndex),
        _ => bail!("unsupported field type '{value}'"),
    }
}

#[cfg(test)]
mod token_case_tests {
    use super::*;

    #[test]
    fn positional_tokens_are_case_sensitive() {
        assert!(matches!(match_target_format("v2"), Some(TargetFormat::V2)));
        assert!(match_target_format("V2").is_none());
        assert!(matches!(match_bench_mode("range"), Some(BenchMode::Range)));
        assert!(match_bench_mode("RANGE").is_none());

        let values = vec!["ZSTD".to_string(), "input.fwob".to_string()];
        let parsed = parse_command_tokens(&values, false, true, false, false, false).unwrap();
        assert_eq!(parsed.paths, ["ZSTD", "input.fwob"]);
        assert_eq!(parsed.codec, None);
    }

    #[test]
    fn field_type_tokens_are_case_sensitive() {
        assert_eq!(parse_field_type("u").unwrap(), FieldType::UnsignedInteger);
        assert!(parse_field_type("U").is_err());
    }
}

fn read_template_schema(
    path: &PathBuf,
    v1_key_field_index: usize,
) -> Result<(Schema, Vec<String>, Option<String>)> {
    match detect_format(path)? {
        Format::V1 => {
            let mut reader = fwob_v1::Reader::open(path, v1_key_field_index)?;
            let strings = reader.read_string_table()?;
            Ok((
                reader.schema().clone(),
                strings,
                Some(reader.header().title.clone()),
            ))
        }
        Format::V2 => {
            let reader = fwob_v2::Reader::open(path)?;
            Ok((
                reader.header().schema.clone(),
                reader.header().string_table.clone(),
                Some(reader.header().title.clone()),
            ))
        }
    }
}

fn inspect_v1(args: V1FileArgs) -> Result<()> {
    let mut reader = fwob_v1::Reader::open(&args.path, args.key_field_index)?;
    let header = reader.header();
    let physical_bytes = std::fs::metadata(&args.path)?.len();
    let data_bytes = header.frame_count * u64::from(header.frame_length);

    toml_section("file");
    toml_kv_str("format", "fwob-v1");
    toml_kv_str("title", &header.title);
    toml_kv_str("frame_type", &header.frame_type);
    toml_kv_num("key_field_index", args.key_field_index);

    println!();
    toml_section("storage");
    toml_kv_num("physical_bytes", physical_bytes);
    toml_kv_num("frame_count", header.frame_count);
    toml_kv_num("frame_length", header.frame_length);
    toml_kv_num("data_bytes", data_bytes);

    println!();
    toml_section("strings");
    toml_kv_num("string_count", header.string_count);
    toml_kv_num("string_table_length", header.string_table_length);
    toml_kv_num(
        "string_table_preserved_length",
        header.string_table_preserved_length,
    );

    println!();
    toml_section("schema");
    toml_kv_num("field_count", reader.schema().fields.len());
    for field in &reader.schema().fields {
        println!();
        toml_array_section("schema.fields");
        toml_kv_str("name", &field.name);
        toml_kv_str("type", field_type_name(field.field_type));
        toml_kv_num("length", field.length);
        toml_kv_num("offset", field.offset);
    }
    let preview = frame_preview_v1_text(&mut reader)?;
    if !preview.is_empty() {
        println!();
        toml_section("frames");
        toml_kv_multiline("preview", &preview);
    }
    Ok(())
}

fn verify_v1(args: V1FileArgs) -> Result<()> {
    let report = fwob_v1::verify_file(&args.path, args.key_field_index)?;
    println!("ok");
    println!("frame_count: {}", comma_u64(report.frame_count));
    println!("string_count: {}", comma_u32(report.string_count));
    println!("file_length: {}", comma_u64(report.file_length));
    Ok(())
}

fn inspect_v2(args: V2FileArgs) -> Result<()> {
    let mut reader = fwob_v2::Reader::open(&args.path)?;
    let header = reader.header().clone();
    let metadata = collect_v2_metadata(&args.path, &mut reader)?;

    toml_section("file");
    toml_kv_str("format", "fwob-v2");
    toml_kv_str("title", &header.title);
    toml_kv_str("frame_type", &header.schema.frame_type);
    toml_kv_num("key_field_index", header.key_field_index);

    println!();
    toml_section("storage");
    toml_kv_num("physical_bytes", metadata.physical_bytes);
    toml_kv_num("expected_physical_bytes", metadata.expected_physical_bytes);
    if metadata.physical_bytes != metadata.expected_physical_bytes {
        toml_kv_str(
            "physical_size_warning",
            &format!(
                "file has {} trailing_or_missing bytes relative to header",
                metadata.physical_bytes as i128 - metadata.expected_physical_bytes as i128
            ),
        );
    }
    toml_kv_num("frame_count", header.frame_count);
    toml_kv_num("string_count", header.string_table.len());

    println!();
    toml_section("pages");
    toml_kv_num("page_size", header.page_size);
    toml_kv_num("page_count", header.page_count);
    toml_kv_num(
        "page_payload_capacity_bytes",
        metadata.payload_capacity_per_page,
    );
    if header.page_count > 0 {
        toml_kv_num("min_frames_per_page", metadata.min_frames);
        toml_kv_num("max_frames_per_page", metadata.max_frames);
    }
    if let Some(first_key) = metadata.first_key {
        toml_kv_key("first_key", first_key);
    }
    if let Some(last_key) = metadata.last_key {
        toml_kv_key("last_key", last_key);
    }

    println!();
    toml_section("compression");
    toml_kv_num("compressed_payload_bytes", metadata.compressed_total);
    toml_kv_num("uncompressed_payload_bytes", metadata.uncompressed_total);
    toml_kv_num("padding_bytes", metadata.padding_bytes);
    if metadata.uncompressed_total > 0 {
        toml_kv_num(
            "payload_ratio",
            format!(
                "{:.4}",
                metadata.compressed_total as f64 / metadata.uncompressed_total as f64
            ),
        );
        toml_kv_num(
            "physical_ratio",
            format!(
                "{:.4}",
                metadata.physical_bytes as f64 / metadata.uncompressed_total as f64
            ),
        );
    }
    if metadata.payload_capacity_total > 0 {
        toml_kv_num(
            "page_payload_utilization",
            format!(
                "{:.4}",
                metadata.compressed_total as f64 / metadata.payload_capacity_total as f64
            ),
        );
    }
    print_page_codec_encoding_stats_toml(&metadata);

    println!();
    toml_section("schema");
    toml_kv_num("field_count", header.schema.fields.len());
    for field in &header.schema.fields {
        println!();
        toml_array_section("schema.fields");
        toml_kv_str("name", &field.name);
        toml_kv_str("type", field_type_name(field.field_type));
        toml_kv_num("length", field.length);
        toml_kv_num("offset", field.offset);
    }
    let preview = frame_preview_v2_text(&mut reader)?;
    if !preview.is_empty() {
        println!();
        toml_section("frames");
        toml_kv_multiline("preview", &preview);
    }
    Ok(())
}

fn verify_v2(args: V2FileArgs) -> Result<()> {
    let mut reader = fwob_v2::Reader::open(&args.path)?;
    reader.verify()?;
    println!("ok");
    println!("page_count: {}", comma_u64(reader.header().page_count));
    println!("frame_count: {}", comma_u64(reader.header().frame_count));
    Ok(())
}

fn field_type_name(field_type: FieldType) -> &'static str {
    match field_type {
        FieldType::SignedInteger => "signed-integer",
        FieldType::UnsignedInteger => "unsigned-integer",
        FieldType::FloatingPoint => "floating-point",
        FieldType::Utf8String => "utf8-string",
        FieldType::StringTableIndex => "string-table-index",
    }
}

fn frame_preview_v1_text(reader: &mut fwob_v1::Reader<std::fs::File>) -> Result<String> {
    let frame_count = reader.header().frame_count;
    let schema = reader.schema().clone();
    let indices = preview_indices(frame_count);
    if indices.is_empty() {
        return Ok(String::new());
    }
    let mut rows = Vec::new();
    for item in indices {
        match item {
            PreviewIndex::Frame(index) => {
                let raw = reader.read_raw_frames_chunk(index, 1)?;
                rows.push(PreviewRow::Frame(index, raw));
            }
            PreviewIndex::Ellipsis => rows.push(PreviewRow::Ellipsis),
        }
    }
    Ok(format_frame_preview_rows(&schema, &rows))
}

fn frame_preview_v2_text(reader: &mut fwob_v2::Reader<std::fs::File>) -> Result<String> {
    let frame_count = reader.header().frame_count;
    let schema = reader.header().schema.clone();
    let indices = preview_indices(frame_count);
    if indices.is_empty() {
        return Ok(String::new());
    }
    let mut rows = Vec::new();
    for item in indices {
        match item {
            PreviewIndex::Frame(index) => {
                let raw = read_v2_frame_at(reader, index)?;
                rows.push(PreviewRow::Frame(index, raw));
            }
            PreviewIndex::Ellipsis => rows.push(PreviewRow::Ellipsis),
        }
    }
    Ok(format_frame_preview_rows(&schema, &rows))
}

enum PreviewIndex {
    Frame(u64),
    Ellipsis,
}

enum PreviewRow {
    Frame(u64, Vec<u8>),
    Ellipsis,
}

fn preview_indices(frame_count: u64) -> Vec<PreviewIndex> {
    let preview = FRAME_PREVIEW_COUNT as u64;
    if frame_count == 0 {
        return Vec::new();
    }
    if frame_count <= preview * 2 {
        return (0..frame_count).map(PreviewIndex::Frame).collect();
    }
    let mut out = Vec::with_capacity(FRAME_PREVIEW_COUNT * 2 + 1);
    for index in 0..preview {
        out.push(PreviewIndex::Frame(index));
    }
    out.push(PreviewIndex::Ellipsis);
    for index in frame_count - preview..frame_count {
        out.push(PreviewIndex::Frame(index));
    }
    out
}

fn read_v2_frame_at(reader: &mut fwob_v2::Reader<std::fs::File>, index: u64) -> Result<Vec<u8>> {
    let mut base = 0u64;
    for page_index in 0..reader.header().page_count {
        let page = reader.read_page_header(page_index)?;
        let page_frames = u64::from(page.frame_count);
        if index < base + page_frames {
            let raw = reader.read_page_raw_frames(page_index)?;
            let frame_len = reader.header().schema.frame_len as usize;
            let offset = (index - base) as usize * frame_len;
            return Ok(raw[offset..offset + frame_len].to_vec());
        }
        base += page_frames;
    }
    bail!("frame index {} is out of range", index);
}

fn format_frame_preview_rows(schema: &Schema, rows: &[PreviewRow]) -> String {
    let mut table = Vec::with_capacity(rows.len() + 1);
    let mut header = Vec::with_capacity(schema.fields.len() + 1);
    header.push("index".to_string());
    header.extend(schema.fields.iter().map(|field| field.name.clone()));
    table.push(header);

    let mut right_align = Vec::with_capacity(schema.fields.len() + 1);
    right_align.push(true);
    right_align.extend(
        schema
            .fields
            .iter()
            .map(|field| field.field_type != FieldType::Utf8String),
    );

    for row in rows {
        match row {
            PreviewRow::Ellipsis => {
                table.push(vec!["...".to_string(); schema.fields.len() + 1]);
            }
            PreviewRow::Frame(index, bytes) => {
                let mut values = Vec::with_capacity(schema.fields.len() + 1);
                values.push(comma_u64(*index));
                for field in &schema.fields {
                    values.push(format_field_value(field, bytes));
                }
                table.push(values);
            }
        }
    }

    let mut widths = vec![0usize; schema.fields.len() + 1];
    for row in &table {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.chars().count());
        }
    }

    let mut out = String::new();
    for (row_index, row) in table.iter().enumerate() {
        for (column_index, value) in row.iter().enumerate() {
            if column_index > 0 {
                out.push_str("  ");
            }
            let align_right = row_index > 0 && right_align[column_index];
            if align_right {
                out.push_str(&format!("{value:>width$}", width = widths[column_index]));
            } else {
                out.push_str(&format!("{value:<width$}", width = widths[column_index]));
            }
        }
        out.push('\n');
    }
    out
}

fn format_field_value(field: &fwob_core::Field, frame: &[u8]) -> String {
    let start = field.offset as usize;
    let end = start + field.length as usize;
    let bytes = &frame[start..end];
    match field.field_type {
        FieldType::SignedInteger => format_signed(bytes),
        FieldType::UnsignedInteger | FieldType::StringTableIndex => format_unsigned(bytes),
        FieldType::FloatingPoint => match bytes.len() {
            4 => format!("{:.6}", f32::from_le_bytes(bytes.try_into().unwrap())),
            8 => format!("{:.6}", f64::from_le_bytes(bytes.try_into().unwrap())),
            _ => format_hex(bytes),
        },
        FieldType::Utf8String => String::from_utf8_lossy(bytes)
            .trim_end_matches('\0')
            .trim_end()
            .to_string(),
    }
}

fn format_signed(bytes: &[u8]) -> String {
    let value = match bytes.len() {
        1 => bytes[0] as i8 as i128,
        2 => i16::from_le_bytes(bytes.try_into().unwrap()) as i128,
        4 => i32::from_le_bytes(bytes.try_into().unwrap()) as i128,
        8 => i64::from_le_bytes(bytes.try_into().unwrap()) as i128,
        _ => return format_hex(bytes),
    };
    comma_i128(value)
}

fn format_unsigned(bytes: &[u8]) -> String {
    let value = match bytes.len() {
        1 => bytes[0] as u128,
        2 => u16::from_le_bytes(bytes.try_into().unwrap()) as u128,
        4 => u32::from_le_bytes(bytes.try_into().unwrap()) as u128,
        8 => u64::from_le_bytes(bytes.try_into().unwrap()) as u128,
        _ => return format_hex(bytes),
    };
    comma_u128(value)
}

fn format_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2 + 2);
    out.push_str("0x");
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

struct V2Metadata {
    physical_bytes: u64,
    expected_physical_bytes: u64,
    payload_capacity_per_page: u64,
    payload_capacity_total: u64,
    compressed_total: u64,
    uncompressed_total: u64,
    padding_bytes: u64,
    min_frames: u32,
    max_frames: u32,
    first_key: Option<fwob_core::Key>,
    last_key: Option<fwob_core::Key>,
    codec_none_pages: u64,
    codec_zstd_pages: u64,
    codec_lz4_pages: u64,
    compressed_page_frames: u64,
    encoding_row_raw_v1_pages: u64,
    encoding_columnar_basic_v1_pages: u64,
    encoding_columnar_delta_v1_pages: u64,
}

fn collect_v2_metadata(
    path: &PathBuf,
    reader: &mut fwob_v2::Reader<std::fs::File>,
) -> Result<V2Metadata> {
    let page_count = reader.header().page_count;
    let page_size = u64::from(reader.header().page_size);
    let payload_capacity_per_page = page_size.saturating_sub(fwob_v2::PAGE_HEADER_LEN as u64);
    let payload_capacity_total = page_count * payload_capacity_per_page;

    let mut compressed_total = 0u64;
    let mut uncompressed_total = 0u64;
    let mut min_frames = u32::MAX;
    let mut max_frames = 0u32;
    let mut first_key = None;
    let mut last_key = None;
    let mut codec_none_pages = 0u64;
    let mut codec_zstd_pages = 0u64;
    let mut codec_lz4_pages = 0u64;
    let mut compressed_page_frames = 0u64;
    let mut encoding_row_raw_v1_pages = 0u64;
    let mut encoding_columnar_basic_v1_pages = 0u64;
    let mut encoding_columnar_delta_v1_pages = 0u64;

    for page_index in 0..page_count {
        let page = reader.read_page_header(page_index)?;
        compressed_total += u64::from(page.compressed_len);
        uncompressed_total += u64::from(page.uncompressed_len);
        min_frames = min_frames.min(page.frame_count);
        max_frames = max_frames.max(page.frame_count);
        if first_key.is_none() {
            first_key = Some(page.first_key);
        }
        last_key = Some(page.last_key);

        match page.codec {
            fwob_v2::Codec::None => codec_none_pages += 1,
            fwob_v2::Codec::Zstd => {
                codec_zstd_pages += 1;
                compressed_page_frames += u64::from(page.frame_count);
            }
            fwob_v2::Codec::Lz4 => {
                codec_lz4_pages += 1;
                compressed_page_frames += u64::from(page.frame_count);
            }
        }
        match page.encoding {
            fwob_v2::Encoding::RowRawV1 => encoding_row_raw_v1_pages += 1,
            fwob_v2::Encoding::ColumnarBasicV1 => encoding_columnar_basic_v1_pages += 1,
            fwob_v2::Encoding::ColumnarDeltaV1 => encoding_columnar_delta_v1_pages += 1,
        }
    }

    let physical_bytes = std::fs::metadata(path)?.len();
    let expected_physical_bytes = fwob_v2::FILE_HEADER_LEN + page_count * page_size;
    let padding_bytes = payload_capacity_total.saturating_sub(compressed_total);

    Ok(V2Metadata {
        physical_bytes,
        expected_physical_bytes,
        payload_capacity_per_page,
        payload_capacity_total,
        compressed_total,
        uncompressed_total,
        padding_bytes,
        min_frames,
        max_frames,
        first_key,
        last_key,
        codec_none_pages,
        codec_zstd_pages,
        codec_lz4_pages,
        compressed_page_frames,
        encoding_row_raw_v1_pages,
        encoding_columnar_basic_v1_pages,
        encoding_columnar_delta_v1_pages,
    })
}

fn bench_v2(args: BenchArgs) -> Result<()> {
    let args = resolve_bench_args(args)?;
    if matches!(args.mode, BenchMode::ConversionMatrix) {
        return bench_conversion_matrix(args);
    }

    let start = std::time::Instant::now();
    let total_frames = match args.mode {
        BenchMode::ConversionMatrix => unreachable!(),
        BenchMode::Range => bench_range_mode(&args)?,
        BenchMode::RandomPage => bench_random_page_mode(&args)?,
        BenchMode::Scan => bench_scan_mode(&args)?,
    };
    let elapsed = start.elapsed();
    let per_iter = elapsed.as_secs_f64() / args.iterations as f64;
    println!("mode: {}", args.mode.as_str());
    println!("iterations: {}", comma_u64(args.iterations));
    println!("total_frames: {}", comma_usize(total_frames));
    println!(
        "elapsed_ms: {}",
        comma_f64(elapsed.as_secs_f64() * 1000.0, 3)
    );
    println!("avg_iter_us: {}", comma_f64(per_iter * 1_000_000.0, 3));
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct ConversionBenchCase {
    page_size_label: &'static str,
    page_size: u32,
    codec: CodecArg,
    zstd_level: Option<i32>,
    encoding: EncodingArg,
    page_packing: PagePackingArg,
}

const CONVERSION_BENCH_PAGE_SIZES: [(&str, u32); 3] = [
    ("512KiB", 512 * 1024),
    ("1MiB", 1024 * 1024),
    ("2MiB", 2 * 1024 * 1024),
];
const CONVERSION_BENCH_CODECS: [CodecArg; 3] =
    [CodecArg::Zstd, CodecArg::Lz4, CodecArg::Uncompressed];
const CONVERSION_BENCH_ZSTD_LEVELS: [i32; 4] = [3, 6, 9, 12];
const CONVERSION_BENCH_ENCODINGS: [EncodingArg; 3] = [
    EncodingArg::RowRaw,
    EncodingArg::ColumnarBasic,
    EncodingArg::ColumnarDelta,
];
const CONVERSION_BENCH_PAGE_PACKINGS: [PagePackingArg; 2] =
    [PagePackingArg::EstimateShrink, PagePackingArg::TightFit];

struct ConversionBenchResult {
    case: ConversionBenchCase,
    elapsed_seconds: f64,
    read_performance: ReadPerformance,
    frame_count: u64,
    page_count: u64,
    physical_bytes: u64,
    compressed_payload_bytes: u64,
    uncompressed_payload_bytes: u64,
    payload_ratio: f64,
    physical_ratio: f64,
    average_frames_per_compressed_page: Option<f64>,
    first_page_attempts: u64,
    subsequent_average_attempts: f64,
    subsequent_min_attempts: u64,
    subsequent_max_attempts: u64,
}

#[derive(Debug, Clone, Copy)]
struct ReadPerformance {
    random_page_iterations: u64,
    random_page_avg_us: f64,
    random_page_frames: u64,
    scan_iterations: u64,
    scan_avg_ms: f64,
    scan_frames: u64,
    range_iterations: u64,
    range_avg_us: f64,
    range_frames: u64,
}

fn bench_conversion_matrix(args: ResolvedBenchArgs) -> Result<()> {
    let cases = conversion_bench_cases();
    let output_dir = args
        .output_dir
        .clone()
        .or_else(|| args.path.parent().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&output_dir)?;

    println!("mode: conversion-matrix");
    println!("input: {}", args.path.display());
    println!("output_dir: {}", output_dir.display());
    println!("cases: {}", comma_usize(cases.len()));
    println!();
    print_conversion_bench_dimensions(&cases, &args);
    println!();

    let mut results = Vec::with_capacity(cases.len());
    for (case_index, case) in cases.iter().copied().enumerate() {
        let output = conversion_bench_output_path(&output_dir, &args.path, case_index, case);
        eprintln!(
            "bench case {}/{}: page_size={} codec={} zstd_level={} encoding={} page_packing={}",
            comma_usize(case_index + 1),
            comma_usize(cases.len()),
            case.page_size_label,
            case.codec.as_str(),
            case.zstd_level
                .map(|level| level.to_string())
                .unwrap_or_else(|| "-".to_string()),
            case.encoding.as_str(),
            case.page_packing.as_str()
        );

        eprintln!("  test=conversion started");
        let conversion_start = std::time::Instant::now();
        let packing_stats = convert_input_to_v2_for_bench(&args, case, &output)
            .with_context(|| format!("benchmark conversion failed for {}", output.display()))?;
        let elapsed_seconds = conversion_start.elapsed().as_secs_f64();
        eprintln!(
            "  test=conversion completed elapsed_s={}",
            comma_f64(elapsed_seconds, 3)
        );

        eprintln!("  test=metadata started");
        let metadata_start = std::time::Instant::now();
        let mut inspect = fwob_v2::Reader::open(&output)?;
        let frame_count = inspect.header().frame_count;
        let page_count = inspect.header().page_count;
        let metadata = collect_v2_metadata(&output, &mut inspect)?;
        drop(inspect);
        eprintln!(
            "  test=metadata completed elapsed_s={}",
            comma_f64(metadata_start.elapsed().as_secs_f64(), 3)
        );

        let read_performance =
            measure_v2_read_performance(&output, args.iterations, args.scan_iterations)?;
        let compressed_pages = metadata.codec_zstd_pages + metadata.codec_lz4_pages;
        let result = ConversionBenchResult {
            case,
            elapsed_seconds,
            read_performance,
            frame_count,
            page_count,
            physical_bytes: metadata.physical_bytes,
            compressed_payload_bytes: metadata.compressed_total,
            uncompressed_payload_bytes: metadata.uncompressed_total,
            payload_ratio: ratio(metadata.compressed_total, metadata.uncompressed_total),
            physical_ratio: ratio(metadata.physical_bytes, metadata.uncompressed_total),
            average_frames_per_compressed_page: if compressed_pages == 0 {
                None
            } else {
                Some(metadata.compressed_page_frames as f64 / compressed_pages as f64)
            },
            first_page_attempts: packing_stats.first_page_attempts,
            subsequent_average_attempts: packing_stats.subsequent_average_attempts(),
            subsequent_min_attempts: packing_stats.subsequent_min_attempts,
            subsequent_max_attempts: packing_stats.subsequent_max_attempts,
        };
        println!(
            "case_done: {}/{} physical_bytes={} physical_ratio={:.4} elapsed_s={} random_page_avg_us={} scan_avg_ms={} range_avg_us={}",
            comma_usize(case_index + 1),
            comma_usize(cases.len()),
            comma_u64(result.physical_bytes),
            result.physical_ratio,
            comma_f64(result.elapsed_seconds, 3),
            comma_f64(result.read_performance.random_page_avg_us, 3),
            comma_f64(result.read_performance.scan_avg_ms, 3),
            comma_f64(result.read_performance.range_avg_us, 3)
        );
        results.push(result);

        if !args.keep_outputs {
            std::fs::remove_file(&output).with_context(|| {
                format!("failed to remove benchmark output {}", output.display())
            })?;
        }
    }

    results.sort_by(|left, right| {
        left.physical_bytes
            .cmp(&right.physical_bytes)
            .then_with(|| left.elapsed_seconds.total_cmp(&right.elapsed_seconds))
    });

    println!();
    println!("[conversion_matrix_summary]");
    print_aligned_table(
        &[
            "rank",
            "page",
            "codec",
            "level",
            "encoding",
            "packing",
            "convert_s",
            "random_ms",
            "scan_ms",
            "range_ms",
            "physical_bytes",
            "ratio",
        ],
        results
            .iter()
            .enumerate()
            .map(|(rank, result)| {
                vec![
                    comma_usize(rank + 1),
                    result.case.page_size_label.to_string(),
                    result.case.codec.as_str().to_string(),
                    result
                        .case
                        .zstd_level
                        .map(|level| level.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    result.case.encoding.as_str().to_string(),
                    result.case.page_packing.as_str().to_string(),
                    comma_f64(result.elapsed_seconds, 3),
                    comma_f64(result.read_performance.random_page_avg_us / 1000.0, 3),
                    comma_f64(result.read_performance.scan_avg_ms, 3),
                    comma_f64(result.read_performance.range_avg_us / 1000.0, 3),
                    comma_u64(result.physical_bytes),
                    format!("{:.4}", result.physical_ratio),
                ]
            })
            .collect(),
        &[
            true, false, false, true, false, false, true, true, true, true, true, true,
        ],
    );

    println!();
    println!("[conversion_matrix_storage]");
    print_aligned_table(
        &[
            "rank",
            "page",
            "codec",
            "level",
            "encoding",
            "packing",
            "frames",
            "pages",
            "compressed_bytes",
            "uncompressed_bytes",
            "payload_ratio",
            "avg_frames_compressed_page",
        ],
        results
            .iter()
            .enumerate()
            .map(|(rank, result)| {
                vec![
                    comma_usize(rank + 1),
                    result.case.page_size_label.to_string(),
                    result.case.codec.as_str().to_string(),
                    result
                        .case
                        .zstd_level
                        .map(|level| level.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    result.case.encoding.as_str().to_string(),
                    result.case.page_packing.as_str().to_string(),
                    comma_u64(result.frame_count),
                    comma_u64(result.page_count),
                    comma_u64(result.compressed_payload_bytes),
                    comma_u64(result.uncompressed_payload_bytes),
                    format!("{:.4}", result.payload_ratio),
                    result
                        .average_frames_per_compressed_page
                        .map(|average| comma_f64(average, 2))
                        .unwrap_or_else(|| "-".to_string()),
                ]
            })
            .collect(),
        &[
            true, false, false, true, false, false, true, true, true, true, true, true,
        ],
    );

    println!();
    println!("[conversion_matrix_read_samples]");
    print_aligned_table(
        &[
            "rank",
            "page",
            "codec",
            "level",
            "encoding",
            "packing",
            "random_iterations",
            "random_frames",
            "scan_iterations",
            "scan_frames",
            "range_iterations",
            "range_frames",
        ],
        results
            .iter()
            .enumerate()
            .map(|(rank, result)| {
                vec![
                    comma_usize(rank + 1),
                    result.case.page_size_label.to_string(),
                    result.case.codec.as_str().to_string(),
                    result
                        .case
                        .zstd_level
                        .map(|level| level.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    result.case.encoding.as_str().to_string(),
                    result.case.page_packing.as_str().to_string(),
                    comma_u64(result.read_performance.random_page_iterations),
                    comma_u64(result.read_performance.random_page_frames),
                    comma_u64(result.read_performance.scan_iterations),
                    comma_u64(result.read_performance.scan_frames),
                    comma_u64(result.read_performance.range_iterations),
                    comma_u64(result.read_performance.range_frames),
                ]
            })
            .collect(),
        &[
            true, false, false, true, false, false, true, true, true, true, true, true,
        ],
    );

    println!();
    println!("[conversion_matrix_packing]");
    print_aligned_table(
        &[
            "rank",
            "page",
            "codec",
            "level",
            "encoding",
            "packing",
            "first_attempts",
            "subseq_avg_attempts",
            "subseq_attempt_range",
        ],
        results
            .iter()
            .enumerate()
            .map(|(rank, result)| {
                vec![
                    comma_usize(rank + 1),
                    result.case.page_size_label.to_string(),
                    result.case.codec.as_str().to_string(),
                    result
                        .case
                        .zstd_level
                        .map(|level| level.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    result.case.encoding.as_str().to_string(),
                    result.case.page_packing.as_str().to_string(),
                    comma_u64(result.first_page_attempts),
                    comma_f64(result.subsequent_average_attempts, 2),
                    format!(
                        "{}..{}",
                        comma_u64(result.subsequent_min_attempts),
                        comma_u64(result.subsequent_max_attempts)
                    ),
                ]
            })
            .collect(),
        &[true, false, false, true, false, false, true, true, true],
    );
    Ok(())
}

fn print_aligned_table(headers: &[&str], rows: Vec<Vec<String>>, right_align: &[bool]) {
    debug_assert_eq!(headers.len(), right_align.len());
    debug_assert!(rows.iter().all(|row| row.len() == headers.len()));

    let mut widths: Vec<usize> = headers
        .iter()
        .map(|header| header.chars().count())
        .collect();
    for row in &rows {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.chars().count());
        }
    }

    for (index, header) in headers.iter().enumerate() {
        if index > 0 {
            print!("  ");
        }
        print!("{header:<width$}", width = widths[index]);
    }
    println!();

    for row in rows {
        for (index, value) in row.iter().enumerate() {
            if index > 0 {
                print!("  ");
            }
            if right_align[index] {
                print!("{value:>width$}", width = widths[index]);
            } else {
                print!("{value:<width$}", width = widths[index]);
            }
        }
        println!();
    }
}

fn print_conversion_bench_dimensions(cases: &[ConversionBenchCase], args: &ResolvedBenchArgs) {
    println!("[conversion_matrix_dimensions]");
    println!(
        "page_size ({}): {}",
        CONVERSION_BENCH_PAGE_SIZES.len(),
        CONVERSION_BENCH_PAGE_SIZES
            .iter()
            .map(|(label, _)| format!(
                "{} ({} cases)",
                label,
                cases
                    .iter()
                    .filter(|case| case.page_size_label == *label)
                    .count()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "codec ({}): {}",
        CONVERSION_BENCH_CODECS.len(),
        CONVERSION_BENCH_CODECS
            .iter()
            .map(|value| format!(
                "{} ({} cases)",
                value.as_str(),
                cases.iter().filter(|case| case.codec == *value).count()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "zstd_level ({}; zstd only): {}",
        CONVERSION_BENCH_ZSTD_LEVELS.len(),
        CONVERSION_BENCH_ZSTD_LEVELS
            .iter()
            .map(|value| format!(
                "{} ({} cases)",
                value,
                cases
                    .iter()
                    .filter(|case| case.zstd_level == Some(*value))
                    .count()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "encoding ({}): {}",
        CONVERSION_BENCH_ENCODINGS.len(),
        CONVERSION_BENCH_ENCODINGS
            .iter()
            .map(|value| format!(
                "{} ({} cases)",
                value.as_str(),
                cases.iter().filter(|case| case.encoding == *value).count()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "page_packing ({}): {}",
        CONVERSION_BENCH_PAGE_PACKINGS.len(),
        CONVERSION_BENCH_PAGE_PACKINGS
            .iter()
            .map(|value| format!(
                "{} ({} cases)",
                value.as_str(),
                cases
                    .iter()
                    .filter(|case| case.page_packing == *value)
                    .count()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("excluded: codec=uncompressed + page_packing=tight-fit");
    println!("conditional: zstd_level applies only to codec=zstd");

    println!();
    println!("[conversion_matrix_test_runs]");
    println!("conversion: {}", comma_usize(cases.len()));
    println!(
        "random_page: {} cases x {} iterations = {} reads",
        comma_usize(cases.len()),
        comma_u64(args.iterations),
        comma_u128(cases.len() as u128 * u128::from(args.iterations))
    );
    println!(
        "scan: {} cases x {} iterations = {} scans",
        comma_usize(cases.len()),
        comma_u64(args.scan_iterations),
        comma_u128(cases.len() as u128 * u128::from(args.scan_iterations))
    );
    println!(
        "range: {} cases x {} iterations = {} queries",
        comma_usize(cases.len()),
        comma_u64(args.iterations),
        comma_u128(cases.len() as u128 * u128::from(args.iterations))
    );
}

fn conversion_bench_cases() -> Vec<ConversionBenchCase> {
    let mut cases = Vec::new();
    for (page_size_label, page_size) in CONVERSION_BENCH_PAGE_SIZES {
        for codec in CONVERSION_BENCH_CODECS {
            for encoding in CONVERSION_BENCH_ENCODINGS {
                for page_packing in CONVERSION_BENCH_PAGE_PACKINGS {
                    if codec == CodecArg::Uncompressed && page_packing == PagePackingArg::TightFit {
                        continue;
                    }
                    if codec == CodecArg::Zstd {
                        for zstd_level in CONVERSION_BENCH_ZSTD_LEVELS {
                            cases.push(ConversionBenchCase {
                                page_size_label,
                                page_size,
                                codec,
                                zstd_level: Some(zstd_level),
                                encoding,
                                page_packing,
                            });
                        }
                    } else {
                        cases.push(ConversionBenchCase {
                            page_size_label,
                            page_size,
                            codec,
                            zstd_level: None,
                            encoding,
                            page_packing,
                        });
                    }
                }
            }
        }
    }
    cases
}

fn conversion_bench_output_path(
    output_dir: &Path,
    input: &Path,
    case_index: usize,
    case: ConversionBenchCase,
) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("fwob");
    let process_id = std::process::id();
    let zstd_level = case
        .zstd_level
        .map(|level| format!("zstd{level}"))
        .unwrap_or_else(|| "nozstd".to_string());
    output_dir.join(format!(
        "{stem}.bench.{process_id}.{case_index}.{}.{}.{}.{}.fwob",
        case.page_size_label,
        case.codec.as_str(),
        zstd_level,
        case.encoding.as_str(),
    ))
}

fn convert_input_to_v2_for_bench(
    args: &ResolvedBenchArgs,
    case: ConversionBenchCase,
    output: &Path,
) -> Result<fwob_v2::PackingStats> {
    match detect_format(&args.path)? {
        Format::V1 => convert_v1_input_to_v2_for_bench(args, case, output),
        Format::V2 => convert_v2_input_to_v2_for_bench(args, case, output),
    }
}

fn bench_writer_options(
    title: String,
    string_table: Vec<String>,
    case: ConversionBenchCase,
) -> fwob_v2::WriterOptions {
    let mut options = fwob_v2::WriterOptions::new(title);
    options.page_size = case.page_size;
    options.codec = case.codec.codec();
    options.codec_selection = case.codec.selection();
    options.zstd_level = case.zstd_level.unwrap_or(fwob_v2::DEFAULT_ZSTD_LEVEL);
    options.encoding = case.encoding.encoding();
    options.encoding_selection = case.encoding.selection();
    options.string_table = string_table;
    options.compress_partial_page = false;
    options.page_packing = case.page_packing.page_packing();
    options
}

fn v1_conversion_chunk_frames(
    codec: Codec,
    encoding_selection: EncodingSelection,
    page_size: u32,
    schema: &fwob_core::Schema,
) -> usize {
    let frame_len = schema.frame_len as usize;
    if codec != Codec::None {
        return ((page_size as usize * 16) / frame_len).max(1);
    }

    let payload_capacity = page_size as usize - fwob_v2::PAGE_HEADER_LEN;
    let encoding_overhead = match encoding_selection {
        EncodingSelection::Fixed(Encoding::ColumnarDeltaV1) => schema.fields.len(),
        EncodingSelection::Fixed(Encoding::RowRawV1 | Encoding::ColumnarBasicV1)
        | EncodingSelection::Smallest => 0,
    };
    if payload_capacity <= encoding_overhead {
        1
    } else {
        ((payload_capacity - encoding_overhead) / frame_len).max(1)
    }
}

fn convert_v1_input_to_v2_for_bench(
    args: &ResolvedBenchArgs,
    case: ConversionBenchCase,
    output: &Path,
) -> Result<fwob_v2::PackingStats> {
    let mut input = fwob_v1::Reader::open(&args.path, args.key_field_index)
        .with_context(|| format!("failed to open input v1 file {}", args.path.display()))?;
    let strings = input.read_string_table()?;
    let options = bench_writer_options(input.header().title.clone(), strings, case);
    let mut writer = fwob_v2::Writer::create(output, input.schema().clone(), options)
        .with_context(|| format!("failed to create benchmark output {}", output.display()))?;

    let frame_len = input.header().frame_length as usize;
    let chunk_frames = v1_conversion_chunk_frames(
        case.codec.codec(),
        case.encoding.selection(),
        case.page_size,
        input.schema(),
    );
    let mut frame_index = 0u64;
    while frame_index < input.header().frame_count {
        let chunk = input.read_raw_frames_chunk(frame_index, chunk_frames)?;
        writer.append_presorted_raw_frames(&chunk)?;
        frame_index += (chunk.len() / frame_len) as u64;
    }
    writer.finish_with_stats().map_err(Into::into)
}

fn convert_v2_input_to_v2_for_bench(
    args: &ResolvedBenchArgs,
    case: ConversionBenchCase,
    output: &Path,
) -> Result<fwob_v2::PackingStats> {
    let mut input = fwob_v2::Reader::open(&args.path)
        .with_context(|| format!("failed to open input v2 file {}", args.path.display()))?;
    let header = input.header().clone();
    let options = bench_writer_options(header.title.clone(), header.string_table.clone(), case);
    let mut writer = fwob_v2::Writer::create(output, header.schema.clone(), options)
        .with_context(|| format!("failed to create benchmark output {}", output.display()))?;

    for page_index in 0..header.page_count {
        for frame in input.read_page_frames(page_index)? {
            writer.append_frame(frame.bytes())?;
        }
    }
    writer.finish_with_stats().map_err(Into::into)
}

fn measure_v2_read_performance(
    path: &Path,
    iterations: u64,
    scan_iterations: u64,
) -> Result<ReadPerformance> {
    let random_page_iterations = iterations.max(1);
    let mut random_reader = fwob_v2::Reader::open(path)?;
    let page_count = random_reader.header().page_count;
    eprintln!(
        "  test=random-page started iterations={}",
        comma_u64(random_page_iterations)
    );
    let random_start = std::time::Instant::now();
    let mut random_page_frames = 0u64;
    if page_count > 0 {
        for i in 0..random_page_iterations {
            let page = (i.wrapping_mul(1_103_515_245).wrapping_add(12_345)) % page_count;
            random_page_frames += random_reader.read_page_frames(page)?.len() as u64;
        }
    }
    let random_page_avg_us =
        random_start.elapsed().as_secs_f64() * 1_000_000.0 / random_page_iterations as f64;
    eprintln!(
        "  test=random-page completed elapsed_s={} average_us={}",
        comma_f64(random_start.elapsed().as_secs_f64(), 3),
        comma_f64(random_page_avg_us, 3)
    );

    let scan_iterations = scan_iterations.max(1);
    eprintln!(
        "  test=scan started iterations={}",
        comma_u64(scan_iterations)
    );
    let scan_start = std::time::Instant::now();
    let mut scan_frames = 0u64;
    for _ in 0..scan_iterations {
        let mut scan_reader = fwob_v2::Reader::open(path)?;
        scan_frames += scan_reader.read_all_frames()?.len() as u64;
    }
    let scan_avg_ms = scan_start.elapsed().as_secs_f64() * 1000.0 / scan_iterations as f64;
    eprintln!(
        "  test=scan completed elapsed_s={} average_ms={}",
        comma_f64(scan_start.elapsed().as_secs_f64(), 3),
        comma_f64(scan_avg_ms, 3)
    );

    let range_iterations = iterations.max(1);
    let mut range_reader = fwob_v2::Reader::open(path)?;
    let range_page = (page_count > 0).then_some(page_count / 2);
    let range_keys = range_page
        .map(|page| range_reader.read_page_header(page))
        .transpose()?
        .map(|header| (header.first_key, header.last_key));
    if let (Some(page), Some((first_key, last_key))) = (range_page, range_keys) {
        eprintln!(
            "  test=range started iterations={} page={} first_key={} last_key={}",
            comma_u64(range_iterations),
            comma_u64(page),
            toml_key_value(first_key),
            toml_key_value(last_key)
        );
    } else {
        eprintln!(
            "  test=range started iterations={} empty_file=true",
            comma_u64(range_iterations)
        );
    }
    let range_start = std::time::Instant::now();
    let mut range_frames = 0u64;
    if let Some((first_key, last_key)) = range_keys {
        for _ in 0..range_iterations {
            range_frames += range_reader.frames_between(first_key, last_key)?.len() as u64;
        }
    }
    let range_avg_us = range_start.elapsed().as_secs_f64() * 1_000_000.0 / range_iterations as f64;
    eprintln!(
        "  test=range completed elapsed_s={} average_us={}",
        comma_f64(range_start.elapsed().as_secs_f64(), 3),
        comma_f64(range_avg_us, 3)
    );

    Ok(ReadPerformance {
        random_page_iterations,
        random_page_avg_us,
        random_page_frames,
        scan_iterations,
        scan_avg_ms,
        scan_frames,
        range_iterations,
        range_avg_us,
        range_frames,
    })
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn bench_range_mode(args: &ResolvedBenchArgs) -> Result<usize> {
    let first = args.first_key_i32.unwrap_or(i32::MIN);
    let last = args.last_key_i32.unwrap_or(i32::MAX);
    let mut total_frames = 0usize;
    for _ in 0..args.iterations {
        let mut reader = fwob_v2::Reader::open(&args.path)?;
        let frames =
            reader.frames_between(fwob_core::Key::I32(first), fwob_core::Key::I32(last))?;
        total_frames += frames.len();
    }
    println!(
        "range: [{}, {}]",
        comma_i128(first as i128),
        comma_i128(last as i128)
    );
    Ok(total_frames)
}

fn bench_random_page_mode(args: &ResolvedBenchArgs) -> Result<usize> {
    let mut reader = fwob_v2::Reader::open(&args.path)?;
    let page_count = reader.header().page_count.max(1);
    let mut total_frames = 0usize;
    for i in 0..args.iterations {
        let page = (i.wrapping_mul(1_103_515_245).wrapping_add(12_345)) % page_count;
        total_frames += reader.read_page_frames(page)?.len();
    }
    Ok(total_frames)
}

fn bench_scan_mode(args: &ResolvedBenchArgs) -> Result<usize> {
    let mut total_frames = 0usize;
    for _ in 0..args.iterations {
        let mut reader = fwob_v2::Reader::open(&args.path)?;
        total_frames += reader.read_all_frames()?.len();
    }
    Ok(total_frames)
}

fn convert(args: ConvertArgs) -> Result<()> {
    let (format, input, output, page_size, write) = parse_convert_target(&args.target, args.write)?;
    match format {
        TargetFormat::V1 => convert_v2_to_v1(input, output),
        TargetFormat::V2 => convert_v1_to_v2(input, output, args.key_field_index, page_size, write),
    }
}

fn convert_v1_to_v2(
    input: PathBuf,
    output: PathBuf,
    key_field_index: usize,
    page_size: u32,
    write: V2WriteOptions,
) -> Result<()> {
    validate_zstd_level(write.zstd_level)?;
    let mut v1 = fwob_v1::Reader::open(&input, key_field_index)
        .with_context(|| format!("failed to open v1 file {}", input.display()))?;
    let strings = v1.read_string_table()?;

    let mut options = fwob_v2::WriterOptions::new(v1.header().title.clone());
    options.page_size = page_size;
    options.codec = write.codec.codec();
    options.codec_selection = write.codec.selection();
    options.zstd_level = write.zstd_level;
    options.encoding = write.encoding.encoding();
    options.encoding_selection = write.encoding.selection();
    options.string_table = strings;
    options.compress_partial_page = write.compress_partial_page;
    options.page_packing = write.page_packing.page_packing();

    let mut writer = fwob_v2::Writer::create(&output, v1.schema().clone(), options)
        .with_context(|| format!("failed to create v2 file {}", output.display()))?;

    let frame_len = v1.header().frame_length as usize;
    let chunk_frames = v1_conversion_chunk_frames(
        write.codec.codec(),
        write.encoding.selection(),
        page_size,
        v1.schema(),
    );
    let mut frame_index = 0u64;
    let total_frames = v1.header().frame_count;
    let progress_step = 5_000_000u64;
    let mut next_progress = progress_step;
    let started = std::time::Instant::now();
    let conversion_started = started;
    while frame_index < v1.header().frame_count {
        let chunk = v1.read_raw_frames_chunk(frame_index, chunk_frames)?;
        writer.append_presorted_raw_frames(&chunk)?;
        frame_index += (chunk.len() / frame_len) as u64;
        if frame_index >= next_progress || frame_index == total_frames {
            let line = format!(
                "converted {}/{} frames ({:.1}%) in {:.1}s",
                comma_u64(frame_index),
                comma_u64(total_frames),
                frame_index as f64 * 100.0 / total_frames as f64,
                started.elapsed().as_secs_f64()
            );
            eprintln!("{line}");
            while next_progress <= frame_index {
                next_progress += progress_step;
            }
        }
    }
    let packing_stats = writer.finish_with_stats()?;

    let mut inspect = fwob_v2::Reader::open(&output)?;
    if write.verify {
        inspect.verify()?;
    }
    let metadata = collect_v2_metadata(&output, &mut inspect)?;
    print_convert_v2_toml(
        &input,
        &output,
        key_field_index,
        page_size,
        write,
        inspect.header().frame_count,
        inspect.header().page_count,
        packing_stats,
        &metadata,
        write.verify,
        conversion_started.elapsed().as_secs_f64(),
    );
    Ok(())
}

fn append_to_v2(args: AppendArgs) -> Result<()> {
    let parsed = parse_command_tokens(&args.target, false, true, false, false, false)?;
    let write = parsed.write_options(args.write);
    let [target, input] = parsed.paths.as_slice() else {
        bail!("append expects exactly target and input paths after tokens");
    };
    let target = PathBuf::from(target);
    let input = PathBuf::from(input);

    validate_zstd_level(write.zstd_level)?;
    if std::fs::canonicalize(&target).ok() == std::fs::canonicalize(&input).ok() {
        bail!("target and input must be different files");
    }

    let target_header = fwob_v2::Reader::open(&target)
        .with_context(|| format!("failed to open target v2 file {}", target.display()))?
        .header()
        .clone();
    let mut options = fwob_v2::WriterOptions::new(target_header.title.clone());
    options.page_size = target_header.page_size;
    options.codec = write.codec.codec();
    options.codec_selection = write.codec.selection();
    options.zstd_level = write.zstd_level;
    options.encoding = write.encoding.encoding();
    options.encoding_selection = write.encoding.selection();
    options.string_table = target_header.string_table.clone();
    options.compress_partial_page = write.compress_partial_page;
    options.page_packing = write.page_packing.page_packing();

    let mut writer = fwob_v2::Writer::open_append(&target, options)
        .with_context(|| format!("failed to open target for append {}", target.display()))?;

    match detect_format(&input)? {
        Format::V1 => append_v1_input(
            &input,
            args.key_field_index,
            write,
            &target_header,
            &mut writer,
        )?,
        Format::V2 => append_v2_input(&input, &target_header, &mut writer)?,
    }

    let packing_stats = writer.finish_with_stats()?;

    let mut inspect = fwob_v2::Reader::open(&target)?;
    if write.verify {
        inspect.verify()?;
    }
    let metadata = collect_v2_metadata(&target, &mut inspect)?;
    println!("appended {} -> {}", input.display(), target.display());
    println!("frames: {}", comma_u64(inspect.header().frame_count));
    println!("pages: {}", comma_u64(inspect.header().page_count));
    print_packing_stats(packing_stats);
    print_compression_stats(&metadata);
    print_page_codec_encoding_stats(&metadata);
    if !write.verify {
        println!("verification: skipped (run `fwob verify` or pass verify)");
    }
    Ok(())
}

fn print_packing_stats(stats: fwob_v2::PackingStats) {
    println!(
        "first_page_compression_attempts: {}",
        comma_u64(stats.first_page_attempts)
    );
    println!(
        "subsequent_page_average_compression_attempts: {}",
        comma_f64(stats.subsequent_average_attempts(), 2)
    );
    println!(
        "subsequent_page_compression_attempts_range: {}..{}",
        comma_u64(stats.subsequent_min_attempts),
        comma_u64(stats.subsequent_max_attempts)
    );
    println!(
        "subsequent_compressed_pages_measured: {}",
        comma_u64(stats.subsequent_pages)
    );
    println!(
        "average_initial_window_frame_span: {}, {}, {}, {}, {}, {}, {}, {}, {}, {}",
        comma_f64(stats.average_window_frame_span(0), 2),
        comma_f64(stats.average_window_frame_span(1), 2),
        comma_f64(stats.average_window_frame_span(2), 2),
        comma_f64(stats.average_window_frame_span(3), 2),
        comma_f64(stats.average_window_frame_span(4), 2),
        comma_f64(stats.average_window_frame_span(5), 2),
        comma_f64(stats.average_window_frame_span(6), 2),
        comma_f64(stats.average_window_frame_span(7), 2),
        comma_f64(stats.average_window_frame_span(8), 2),
        comma_f64(stats.average_window_frame_span(9), 2)
    );
    println!(
        "average_initial_window_attempts: {}",
        comma_f64(stats.average_initial_window_attempts(), 2)
    );
    println!(
        "average_initial_window_final_position: {:.4}, {:.4}, {:.4}, {:.4}, {:.4}, {:.4}, {:.4}, {:.4}, {:.4}, {:.4}",
        stats.average_window_final_position(0),
        stats.average_window_final_position(1),
        stats.average_window_final_position(2),
        stats.average_window_final_position(3),
        stats.average_window_final_position(4),
        stats.average_window_final_position(5),
        stats.average_window_final_position(6),
        stats.average_window_final_position(7),
        stats.average_window_final_position(8),
        stats.average_window_final_position(9)
    );
    println!(
        "initial_windows_measured: {}",
        comma_u64(stats.initial_windows)
    );
}

#[allow(clippy::too_many_arguments)]
fn print_convert_v2_toml(
    input: &Path,
    output: &Path,
    key_field_index: usize,
    page_size: u32,
    write: V2WriteOptions,
    frame_count: u64,
    page_count: u64,
    packing_stats: fwob_v2::PackingStats,
    metadata: &V2Metadata,
    verified: bool,
    elapsed_seconds: f64,
) {
    toml_section("conversion");
    toml_kv_str("target_format", "fwob-v2");
    toml_kv_str("input", &input.display().to_string());
    toml_kv_str("output", &output.display().to_string());
    toml_kv_num("frames", frame_count);
    toml_kv_num("pages", page_count);
    toml_kv_bool("verified", verified);
    if !verified {
        toml_kv_str("verification", "skipped (run `fwob verify` or pass verify)");
    }
    toml_kv_num("elapsed_seconds", format!("{elapsed_seconds:.3}"));

    println!();
    toml_section("parameters");
    toml_kv_str("target_format", "fwob-v2");
    toml_kv_num("key_field_index", key_field_index);
    toml_kv_num("page_size", page_size);
    toml_kv_str("codec", write.codec.as_str());
    toml_kv_str("encoding", write.encoding.as_str());
    toml_kv_num("zstd_level", write.zstd_level);
    toml_kv_bool("compress_partial_page", write.compress_partial_page);
    toml_kv_str("page_packing", write.page_packing.as_str());
    toml_kv_bool("verify", write.verify);

    println!();
    print_packing_stats_toml(packing_stats);

    println!();
    print_compression_stats_toml(metadata);

    println!();
    toml_section("page_stats");
    print_page_codec_encoding_stats_toml(metadata);
}

fn print_packing_stats_toml(stats: fwob_v2::PackingStats) {
    toml_section("packing");
    toml_kv_num("first_page_compression_attempts", stats.first_page_attempts);
    toml_kv_num(
        "subsequent_page_average_compression_attempts",
        format!("{:.2}", stats.subsequent_average_attempts()),
    );
    toml_kv_str(
        "subsequent_page_compression_attempts_range",
        &format!(
            "{}..{}",
            stats.subsequent_min_attempts, stats.subsequent_max_attempts
        ),
    );
    toml_kv_num(
        "subsequent_compressed_pages_measured",
        stats.subsequent_pages,
    );
    let spans = (0..10)
        .map(|index| stats.average_window_frame_span(index))
        .collect::<Vec<_>>();
    toml_kv_float_array("average_initial_window_frame_span", &spans, 2);
    toml_kv_num(
        "average_initial_window_attempts",
        format!("{:.2}", stats.average_initial_window_attempts()),
    );
    let positions = (0..10)
        .map(|index| stats.average_window_final_position(index))
        .collect::<Vec<_>>();
    toml_kv_float_array("average_initial_window_final_position", &positions, 4);
    toml_kv_num("initial_windows_measured", stats.initial_windows);
}

fn print_compression_stats_toml(metadata: &V2Metadata) {
    toml_section("compression");
    toml_kv_num("compressed_payload_bytes", metadata.compressed_total);
    toml_kv_num("uncompressed_payload_bytes", metadata.uncompressed_total);
    let compressed_pages = metadata.codec_zstd_pages + metadata.codec_lz4_pages;
    if compressed_pages > 0 {
        toml_kv_num(
            "average_frames_per_compressed_page",
            format!(
                "{:.2}",
                metadata.compressed_page_frames as f64 / compressed_pages as f64
            ),
        );
    }
    if metadata.uncompressed_total > 0 {
        toml_kv_num(
            "payload_ratio",
            format!(
                "{:.4}",
                metadata.compressed_total as f64 / metadata.uncompressed_total as f64
            ),
        );
        toml_kv_num(
            "physical_ratio",
            format!(
                "{:.4}",
                metadata.physical_bytes as f64 / metadata.uncompressed_total as f64
            ),
        );
    }
}

fn print_compression_stats(metadata: &V2Metadata) {
    println!(
        "compressed_payload_bytes: {}",
        comma_u64(metadata.compressed_total)
    );
    println!(
        "uncompressed_payload_bytes: {}",
        comma_u64(metadata.uncompressed_total)
    );
    let compressed_pages = metadata.codec_zstd_pages + metadata.codec_lz4_pages;
    if compressed_pages > 0 {
        println!(
            "average_frames_per_compressed_page: {}",
            comma_f64(
                metadata.compressed_page_frames as f64 / compressed_pages as f64,
                2
            )
        );
    }
    if metadata.uncompressed_total > 0 {
        println!(
            "payload_ratio: {:.4}",
            metadata.compressed_total as f64 / metadata.uncompressed_total as f64
        );
        println!(
            "physical_ratio: {:.4}",
            metadata.physical_bytes as f64 / metadata.uncompressed_total as f64
        );
    }
}

fn print_page_codec_encoding_stats(metadata: &V2Metadata) {
    println!("codec_none_pages: {}", comma_u64(metadata.codec_none_pages));
    println!("codec_zstd_pages: {}", comma_u64(metadata.codec_zstd_pages));
    println!("codec_lz4_pages: {}", comma_u64(metadata.codec_lz4_pages));
    println!(
        "encoding_row_raw_v1_pages: {}",
        comma_u64(metadata.encoding_row_raw_v1_pages)
    );
    println!(
        "encoding_columnar_basic_v1_pages: {}",
        comma_u64(metadata.encoding_columnar_basic_v1_pages)
    );
    println!(
        "encoding_columnar_delta_v1_pages: {}",
        comma_u64(metadata.encoding_columnar_delta_v1_pages)
    );
}

fn print_page_codec_encoding_stats_toml(metadata: &V2Metadata) {
    toml_kv_num("codec_none_pages", metadata.codec_none_pages);
    toml_kv_num("codec_zstd_pages", metadata.codec_zstd_pages);
    toml_kv_num("codec_lz4_pages", metadata.codec_lz4_pages);
    toml_kv_num(
        "encoding_row_raw_v1_pages",
        metadata.encoding_row_raw_v1_pages,
    );
    toml_kv_num(
        "encoding_columnar_basic_v1_pages",
        metadata.encoding_columnar_basic_v1_pages,
    );
    toml_kv_num(
        "encoding_columnar_delta_v1_pages",
        metadata.encoding_columnar_delta_v1_pages,
    );
}

fn append_v1_input(
    input_path: &Path,
    key_field_index: usize,
    write: V2WriteOptions,
    target_header: &fwob_v2::FileHeader,
    writer: &mut fwob_v2::Writer<std::fs::File>,
) -> Result<()> {
    let mut input = fwob_v1::Reader::open(input_path, key_field_index)
        .with_context(|| format!("failed to open input v1 file {}", input_path.display()))?;
    if input.schema() != &target_header.schema {
        bail!("input schema does not match target schema");
    }
    let input_strings = input.read_string_table()?;
    if input_strings != target_header.string_table {
        bail!("input string table does not match target string table");
    }

    let frame_len = input.header().frame_length as usize;
    let chunk_frames = v1_conversion_chunk_frames(
        write.codec.codec(),
        write.encoding.selection(),
        target_header.page_size,
        input.schema(),
    );
    let mut frame_index = 0u64;
    while frame_index < input.header().frame_count {
        let chunk = input.read_raw_frames_chunk(frame_index, chunk_frames)?;
        writer.append_presorted_raw_frames(&chunk)?;
        frame_index += (chunk.len() / frame_len) as u64;
    }
    Ok(())
}

fn append_v2_input(
    input_path: &Path,
    target_header: &fwob_v2::FileHeader,
    writer: &mut fwob_v2::Writer<std::fs::File>,
) -> Result<()> {
    let mut input = fwob_v2::Reader::open(input_path)
        .with_context(|| format!("failed to open input v2 file {}", input_path.display()))?;
    if input.header().schema != target_header.schema {
        bail!("input schema does not match target schema");
    }
    if input.header().string_table != target_header.string_table {
        bail!("input string table does not match target string table");
    }
    for frame in input.read_all_frames()? {
        writer.append_frame(frame.bytes())?;
    }
    Ok(())
}

fn convert_v2_to_v1(input: PathBuf, output: PathBuf) -> Result<()> {
    let mut v2 = fwob_v2::Reader::open(&input)
        .with_context(|| format!("failed to open v2 file {}", input.display()))?;
    let mut options = fwob_v1::WriterOptions::new(v2.header().title.clone());
    let estimated_string_bytes: usize = v2.header().string_table.iter().map(|s| s.len() + 5).sum();
    options.string_table_preserved_length = estimated_string_bytes.max(1834) as u32;
    let mut writer = fwob_v1::Writer::create(&output, v2.header().schema.clone(), options)?;
    for value in &v2.header().string_table {
        writer.append_string(value)?;
    }

    let total_pages = v2.header().page_count;
    let total_frames = v2.header().frame_count;
    let progress_step = 5_000_000u64;
    let mut next_progress = progress_step;
    let mut converted_frames = 0u64;
    let started = std::time::Instant::now();
    for page_index in 0..total_pages {
        let raw = v2.read_page_raw_frames(page_index)?;
        let frame_count = raw.len() / v2.header().schema.frame_len as usize;
        writer.append_presorted_raw_frames(&raw)?;
        converted_frames += frame_count as u64;
        if converted_frames >= next_progress || converted_frames == total_frames {
            let line = format!(
                "converted {}/{} frames ({:.1}%) in {:.1}s",
                comma_u64(converted_frames),
                comma_u64(total_frames),
                converted_frames as f64 * 100.0 / total_frames as f64,
                started.elapsed().as_secs_f64()
            );
            eprintln!("{line}");
            while next_progress <= converted_frames {
                next_progress += progress_step;
            }
        }
    }
    toml_section("conversion");
    toml_kv_str("target_format", "fwob-v1");
    toml_kv_str("input", &input.display().to_string());
    toml_kv_str("output", &output.display().to_string());
    toml_kv_num("frames", total_frames);
    toml_kv_num("pages", total_pages);
    toml_kv_num(
        "elapsed_seconds",
        format!("{:.3}", started.elapsed().as_secs_f64()),
    );

    println!();
    toml_section("parameters");
    toml_kv_str("target_format", "fwob-v1");
    toml_kv_num("source_page_count", total_pages);
    toml_kv_num("source_frame_count", total_frames);
    Ok(())
}

fn validate_zstd_level(level: i32) -> Result<()> {
    if !(1..=22).contains(&level) {
        bail!("--zstd-level must be between 1 and 22");
    }
    Ok(())
}

fn comma_u32(value: u32) -> String {
    comma_u64(u64::from(value))
}

fn comma_usize(value: usize) -> String {
    comma_u64(value as u64)
}

fn comma_i128(value: i128) -> String {
    if value < 0 {
        format!("-{}", comma_u128(value.unsigned_abs()))
    } else {
        comma_u128(value as u128)
    }
}

fn comma_u64(value: u64) -> String {
    comma_u128(u128::from(value))
}

fn comma_f64(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return value.to_string();
    }

    let rendered = format!("{:.*}", decimals, value);
    let (sign, body) = if let Some(unsigned) = rendered.strip_prefix('-') {
        ("-", unsigned)
    } else {
        ("", rendered.as_str())
    };
    let (integer, fraction) = body.split_once('.').unwrap_or((body, ""));
    let formatted_integer = integer
        .parse::<u128>()
        .map(comma_u128)
        .unwrap_or_else(|_| integer.to_string());
    if fraction.is_empty() {
        format!("{sign}{formatted_integer}")
    } else {
        format!("{sign}{formatted_integer}.{fraction}")
    }
}

fn comma_u128(value: u128) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn parse_page_size_token(value: &str) -> Option<Result<u32>> {
    const MIN_PAGE_SIZE: u64 = 1024;
    const MAX_PAGE_SIZE: u64 = 16 * 1024 * 1024;

    let (number, multiplier) = [
        ("KiB", 1024u64),
        ("MiB", 1024u64 * 1024),
        ("KB", 1000u64),
        ("MB", 1000u64 * 1000),
        ("B", 1u64),
    ]
    .into_iter()
    .find_map(|(suffix, multiplier)| {
        value
            .strip_suffix(suffix)
            .filter(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
            .map(|number| (number, multiplier))
    })?;

    Some((|| {
        let number: u64 = number.parse()?;
        let size = number
            .checked_mul(multiplier)
            .context("page size is too large")?;
        if !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&size) {
            bail!("page size must be between 1KiB and 16MiB");
        }
        Ok(size as u32)
    })())
}
