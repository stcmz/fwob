use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use fwob_core::{Field, FieldType, Key, Schema};
use fwob_v2::{Codec, CodecSelection, Encoding, EncodingSelection, PagePacking};

mod bench;
mod file_info;
mod inspect;
mod mutate;
mod output;
mod query;
mod transfer;

use output::*;

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
    /// Summarize multiple FWOB files in table, Markdown, CSV, or JSON Lines form.
    Info(InfoArgs),
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
#[command(override_usage = "fwob info [OPTIONS] [PATH...] [FORMAT]")]
#[command(after_help = "Plain tokens:
  formats: table (default), md, csv, jsonl

PATH may name a file or directory. Directories contribute their immediate *.fwob files.
With no PATH, the command lists immediate *.fwob files in the current directory.
Format tokens win on exact match; use ./table for a file or directory named table.")]
struct InfoArgs {
    /// Files, directories, and one optional output-format token.
    #[arg(value_name = "PATH_OR_FORMAT")]
    target: Vec<String>,

    /// Key field index for v1 files. Ignored for v2 because v2 stores it.
    #[arg(long, default_value_t = 0)]
    key_field_index: usize,
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

INPUT and OUTPUT may be file paths. INPUT may also be a directory, in which case immediate
*.fwob files are converted into the OUTPUT directory. A file input targets OUTPUT directly when it
has a file extension; an existing directory or nonexistent extensionless OUTPUT is a directory.
Directory conversion creates OUTPUT when needed.

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

    /// Maximum files converted concurrently. Defaults to the logical CPU count.
    #[arg(long)]
    parallelism: Option<std::num::NonZeroUsize>,

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
#[command(override_usage = "fwob append [OPTIONS] TARGET INPUT... [TOKENS]")]
#[command(
    after_help = "Appends one or more inputs (v1 or v2) into an existing v1 or v2 target, in
order. For a v2 target the write tokens below tune the appended pages (e.g. a
different codec or --zstd-level).

Plain tokens:
  codecs: zstd, lz4, smallest, uncompressed
  encodings: row-raw, columnar-basic, columnar-delta, smallest
  page packing: estimate-shrink, tight-fit
  switches: verify, compress-partial-page

Tokens may appear anywhere. Reserved tokens win on exact match; use ./row-raw for a file named row-raw."
)]
struct AppendArgs {
    /// Existing v1 or v2 target, one or more input files, and plain tokens such as
    /// zstd, row-raw, tight-fit, verify, and compress-partial-page.
    #[arg(value_name = "TARGET", num_args = 2..)]
    target: Vec<String>,

    /// Key field index for v1 inputs/targets. v1 does not store this in metadata.
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
    /// Overwrite OUTPUT if it already exists. Without this, concat refuses to clobber an
    /// existing output so its contents are never silently discarded.
    #[arg(long, visible_alias = "overwrite")]
    force: bool,
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
    /// Set a field's semantic as NAME=VALUE, where VALUE is one of none, unix-seconds,
    /// unix-milliseconds, unix-microseconds, unix-nanoseconds. v2 only. May be repeated.
    #[arg(long = "set-semantic", value_name = "NAME=VALUE")]
    set_semantics: Vec<String>,
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

/// Prints an error and its cause chain to stderr, colorized in red (TTY-gated).
pub fn print_error(error: &anyhow::Error) {
    log_error(error);
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Create(args) => create_blank(args),
        Command::Inspect(args) => inspect_auto(args),
        Command::Verify(args) => verify_auto(args),
        Command::Info(args) => file_info::print_file_info(args),
        Command::Bench(args) => bench::bench_v2(args),
        Command::Convert(args) => transfer::convert(args),
        Command::Append(args) => transfer::append_to_v2(args),
        Command::Split(args) => mutate::split_file(args),
        Command::Concat(args) => mutate::concat_file(args),
        Command::Edit(args) => mutate::edit_file(args),
        Command::Find(args) => query::find_frames(args),
        Command::Dump(args) => query::dump_frames(args),
        Command::Delete(args) => mutate::delete_frames(args),
    }
}

fn inspect_auto(args: AutoFileArgs) -> Result<()> {
    match detect_format(&args.path)? {
        Format::V1 => inspect::inspect_v1(V1FileArgs {
            path: args.path,
            key_field_index: args.key_field_index,
        }),
        Format::V2 => inspect::inspect_v2(V2FileArgs { path: args.path }),
    }
}

fn verify_auto(args: AutoFileArgs) -> Result<()> {
    match detect_format(&args.path)? {
        Format::V1 => inspect::verify_v1(V1FileArgs {
            path: args.path,
            key_field_index: args.key_field_index,
        }),
        Format::V2 => inspect::verify_v2(V2FileArgs { path: args.path }),
    }
}

#[derive(Debug, Clone, Copy)]
enum Format {
    V1,
    V2,
}

fn detect_format(path: &Path) -> Result<Format> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    match &magic {
        b"FWOB" => Ok(Format::V1),
        b"FWB2" => Ok(Format::V2),
        _ => bail!("unrecognized FWOB file signature"),
    }
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

    toml_section("create");
    toml_kv_str("output", &output.display().to_string());
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
    path: &Path,
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

fn validate_zstd_level(level: i32) -> Result<()> {
    if !(1..=22).contains(&level) {
        bail!("--zstd-level must be between 1 and 22");
    }
    Ok(())
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
