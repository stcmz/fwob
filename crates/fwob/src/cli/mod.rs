use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use fwob_core::{Field, FieldType, Key, Schema};
use fwob_v2::{Codec, CodecSelection, Encoding, EncodingSelection, PagePacking};

mod bench;
mod create;
mod file_info;
mod format;
mod inspect;
mod metadata;
mod mutate;
mod operation_summary;
mod output;
mod query;
mod tokens;
mod transfer;

use create::*;
use format::*;
use metadata::*;
use operation_summary::*;
use output::*;
use tokens::*;

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

    /// Overwrite OUTPUT if it already exists. Without this, create refuses to
    /// clobber an existing file.
    #[arg(long, visible_alias = "overwrite")]
    force: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TargetFormat {
    V1,
    V2,
}

const DEFAULT_TARGET_FORMAT: TargetFormat = TargetFormat::V2;

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
  page size: INTEGER{B|KB|KiB|MB|MiB} (1KiB..16MiB; default 512KiB)
  switches: verify, compress-partial-page

V2 parts default to zstd level 6, columnar-basic, estimate-shrink, and 512KiB pages.
Tokens may appear anywhere.")]
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
#[command(after_help = "Output format defaults to v2.

V2 output tokens:
  codecs: zstd, lz4, smallest, uncompressed
  encodings: row-raw, columnar-basic, columnar-delta, smallest
  page packing: estimate-shrink, tight-fit
  page size: INTEGER{B|KB|KiB|MB|MiB} (1KiB..16MiB; default 512KiB)
  switches: verify, compress-partial-page

V2 output defaults to zstd level 6, columnar-basic, estimate-shrink, and 512KiB pages.
Tokens may appear anywhere.")]
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
pub(super) struct V2WriteOptions {
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
