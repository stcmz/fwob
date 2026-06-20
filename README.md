# FWOB

[![CI](https://github.com/stcmz/fwob/actions/workflows/ci.yml/badge.svg)](https://github.com/stcmz/fwob/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/fwob.svg)](https://crates.io/crates/fwob)

FWOB is a Rust implementation of the Fixed-Width Ordered Binary format family.

The project provides two format versions:

1. FWOB v1 for compact fixed-width ordered files.
2. FWOB v2, a fixed-page compressed format for high-performance
   random access, range queries, and bulk append workloads.

FWOB v2 keeps page addresses arithmetic while allowing each page to contain a
variable number of fixed-width frames. A page is a fixed-size on-disk container
with an 80-byte header, compressed payload, and zero padding.

## Workspace

- `fwob-core`: shared schema, frame, key, reader/writer handles, service traits,
  and error types.
- `fwob-derive`: derive macro for strongly typed fixed-width frames.
- `fwob-v1`: FWOB v1 reader, writer, verifier, and compatibility tests.
- `fwob-v2`: compressed fixed-page FWOB v2 reader and writer.
- `fwob`: the primary library facade with auto-detecting `Reader`, `Writer`,
  `Editor`, `Maintenance`, and `Organizer` APIs, plus the command-line tool.

The logical Rust API is documented in [`docs/api.md`](docs/api.md).
Repair can promote complete ordered frames or pages left beyond the committed
count by an interrupted write, while truncating partial or invalid tails.

## Installation

Install the command-line tool from crates.io:

```bash
cargo install fwob
```

Library crates are available separately as `fwob`, `fwob-core`, `fwob-derive`,
`fwob-v1`, and `fwob-v2`.

## Library Quick Start

```rust
use fwob::{Reader, Writer};
use fwob_core::{Field, FieldType, Schema};

let schema = Schema::new(
    "Tick",
    vec![Field::new("time", FieldType::SignedInteger, 8, 0)],
    0,
)?;

let mut writer = Writer::create_v2(
    "ticks.fwob",
    schema,
    fwob_v2::WriterOptions::new("prices"),
)?;
writer.append_frame(&123_i64.to_le_bytes())?;
writer.finish()?;

let mut reader = Reader::open("ticks.fwob")?;
let first = reader.first_frame()?.expect("one frame");
assert_eq!(first.bytes(), 123_i64.to_le_bytes());
# Ok::<(), Box<dyn std::error::Error>>(())
```

See [`docs/api.md`](docs/api.md) for reading, appending, editing, maintenance,
organization, typed-frame, and format-specific examples.

Typed frames map ordinary Rust structs directly to the stored schema:

```rust
use fwob::{TypedEditor, TypedReader, TypedWriter};
use fwob_core::FwobFrame;

#[derive(Debug, PartialEq, FwobFrame)]
struct Tick {
    #[fwob(key)]
    time: i64,
    price: u32,
    size: i32,
}

let mut writer = TypedWriter::<Tick>::create_v2(
    "ticks.fwob",
    fwob_v2::WriterOptions::new("prices"),
)?;
writer.append(&Tick { time: 1, price: 500, size: 10 })?;
writer.finish()?;

let mut reader = TypedReader::<Tick>::open("ticks.fwob")?;
assert_eq!(reader.read_frame(0)?.unwrap().price, 500);

let mut editor = TypedEditor::<Tick>::open("ticks.fwob")?;
editor.delete_ranges(&[0..1])?;
# Ok::<(), fwob::Error>(())
```

Fixed-width UTF-8 fields can use `fwob_core::FixedString<N>`. Values are
space-padded to exactly `N` bytes and rejected when their encoded byte length
exceeds the declared width.
The typed API also re-exports `fwob_core::Decimal` with the legacy 16-byte
decimal representation.
Ordered keys may be integers, `f32`, `f64`, or `Decimal`.
String-table fields may use `StringIndex8`, `StringIndex16`, `StringIndex`
(32-bit), or `StringIndex64`.
On v2, integer fields may declare Unix epoch semantics with
`#[fwob(timestamp = "seconds")]` or the millisecond, microsecond, and
nanosecond variants. Table and Markdown output render those fields as UTC.

## Command Examples

```bash
fwob verify ticks.fwob
fwob inspect ticks.fwob
fwob info
fwob info data archive/ticks.fwob md
fwob create ticks-empty.fwob --template ticks.fwob
fwob convert ticks.fwob ticks-v2.fwob smallest 1MiB --zstd-level 9
fwob convert ticks.fwob ticks-columnar.fwob columnar-basic zstd
fwob convert v2 ticks.fwob ticks-delta.fwob columnar-delta zstd verify
fwob convert raw-files converted-files columnar-delta zstd --parallelism 8
fwob append ticks-v2.fwob new-ticks-1.fwob new-ticks-2.fwob verify
fwob split ticks.fwob parts 1000 2000 3000 zstd columnar-basic
fwob concat ticks-joined.fwob parts/ticks.part0.fwob parts/ticks.part1.fwob zstd
fwob concat v1 ticks-v1.fwob ticks-old.fwob ticks-new.fwob
fwob edit ticks-joined.fwob --title Renamed --append-string NASDAQ --set-semantic Time=unix-milliseconds
fwob find ticks-v2.fwob 100..200
fwob find ticks-v2.fwob 100 200..300 500.. ..50
fwob dump ticks-v2.fwob 100 200..300 csv
fwob dump ticks-v2.fwob raw > ticks.txt
fwob delete ticks-v2.fwob 100..200 local-repack verify
fwob delete ticks-v2.fwob 100.. 250 300..400 repack-to-end zstd columnar-basic compress-partial-page
fwob delete GOOGL.fwob 1772563641.. verify
fwob verify ticks-v2.fwob
fwob bench range ticks-v2.fwob --first-key-i32 100 --last-key-i32 200
```

`fwob create` and `fwob concat` refuse to overwrite an existing output. Pass
`--force` (or `--overwrite`) to replace it explicitly.

Append and concat assume every input file is internally valid. They validate
cross-file schema, string-table, and key-boundary compatibility without rescanning
each complete input. Run `fwob verify FILE` first when input corruption is a
concern. Mixed v1/v2 concat warns when v1's missing semantic metadata requires a
relaxed comparison. V2 output preserves available v2 semantics; forced v1 output
drops them because v1 has no semantic metadata slot.

`fwob info` summarizes FWOB files as a padded table. With no paths it lists
immediate `*.fwob` files in the current directory. Each supplied path may be a
file or directory; directory discovery is non-recursive. Add `table`, `md`,
`csv`, or `jsonl` to select the output format. The summary includes path, format,
title, frame type, key-field index, field count, frame length/count, boundary
keys, raw frame bytes, and physical-to-raw ratio.

`fwob convert` accepts file-to-file, file-to-directory, and
directory-to-directory conversion. Directory input discovers immediate
`*.fwob` files, preserves each filename, and creates the output directory when
needed. For file input, a nonexistent extensionless output is treated as a
directory; an explicit output filename should use the `.fwob` extension. Files
are converted concurrently; `--parallelism N` sets the worker
limit and defaults to the logical CPU count. Progress lines may interleave on
stderr and include the input filename, while each file's structured stdout
summary is printed atomically.

V2 writes default consistently across convert, append, concat, delete, and
split: zstd level 6, columnar-basic encoding, and estimate-shrink packing.
New v2 outputs use 512 KiB pages unless another page size is supplied; append
and delete retain the existing file's fixed page size. Create, convert, and
concat default to v2 output; pass `v1` explicitly when v1 output is required.

Convert, append, concat, split, and delete write progress diagnostics to stderr
and keep structured TOML on stdout. Mutation summaries contain one
operation-specific section followed by `[parameters]`, `[packing]`,
`[compression]`, and `[page_stats]`; the operation section includes
`elapsed_seconds`.

Positional tokens are case-sensitive. For example, `v2`, `zstd`, and `1MiB`
are tokens; `V2`, `ZSTD`, and `1MIB` are treated as paths or values rather than
their lowercase token forms.

## Tuning Parameters

| Parameter | What It Controls | Typical Values |
| --- | --- | --- |
| page-size token | Fixed physical page size. Integer with `B`, `KB`, `KiB`, `MB`, or `MiB`; range `1KiB..16MiB`. | `512KiB` (default), `1MB`, `1MiB`, `2MiB` |
| codec token | Page compression codec. | `zstd` (default), `lz4`, `smallest`, `uncompressed` |
| `--zstd-level` | zstd compression level. Affects write/convert speed heavily, read speed lightly. | `3`, `6` (default), `9`, `12`, `15`, `19` |
| encoding token | Page payload layout before compression. `smallest` tries columnar-basic and columnar-delta per page and stores the winning concrete encoding in page metadata. | `row-raw`, `columnar-basic` (default), `columnar-delta`, `smallest` |
| page-packing token | Packing strategy for compressed pages. | `estimate-shrink` (default), `tight-fit` |
| `compress-partial-page` token | Compress the final partial output page instead of leaving the final non-overflowing remainder raw. | omitted (default), present |
