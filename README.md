# FWOB

[![CI](https://github.com/stcmz/fwob/actions/workflows/ci.yml/badge.svg)](https://github.com/stcmz/fwob/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/fwob.svg)](https://crates.io/crates/fwob)

FWOB is a Rust implementation of the Fixed-Width Ordered Binary format family.

The project has two compatibility goals:

1. Fully support FWOB v1 files produced by the original C# library.
2. Provide FWOB v2, a fixed-page compressed format for high-performance
   random access, range queries, and bulk append workloads.

FWOB v2 keeps page addresses arithmetic while allowing each page to contain a
variable number of fixed-width frames. A page is a fixed-size on-disk container
with a 64-byte header, compressed payload, and zero padding.

## Workspace

- `fwob-core`: shared schema, frame, key, and error types.
- `fwob-v1`: FWOB v1 reader, writer, verifier, and compatibility tests.
- `fwob-v2`: compressed fixed-page FWOB v2 reader and writer.
- `fwob`: command-line tools for conversion, inspection, verification, and
  benchmarking.

## Installation

Install the command-line tool from crates.io:

```bash
cargo install fwob
```

Library crates are available separately as `fwob-core`, `fwob-v1`, and
`fwob-v2`.

## Command Examples

```bash
fwob verify ticks.fwob
fwob inspect ticks.fwob
fwob convert ticks.fwob ticks-v2.fwob smallest 1MiB --zstd-level 9
fwob convert ticks.fwob ticks-columnar.fwob columnar-basic zstd
fwob convert v2 ticks.fwob ticks-delta.fwob columnar-delta zstd verify
fwob append ticks-v2.fwob new-ticks.fwob verify
fwob verify ticks-v2.fwob
fwob bench range ticks-v2.fwob --first-key-i32 100 --last-key-i32 200
```

## Tuning Parameters

| Parameter | What It Controls | Typical Values |
| --- | --- | --- |
| page-size token | Fixed physical page size. Integer with `B`, `KB`, `KiB`, `MB`, or `MiB`; range `1KiB..16MiB`. | `512KiB` (default), `1MB`, `1MiB`, `2MiB` |
| codec token | Page compression codec. | `zstd` (default), `lz4`, `smallest`, `none` |
| `--zstd-level` | zstd compression level. Affects write/convert speed heavily, read speed lightly. | `3`, `6` (default), `9`, `12`, `15`, `19` |
| encoding token | Page payload layout before compression. `smallest` tries columnar-basic and columnar-delta per page and stores the winning concrete encoding in page metadata. | `row-raw`, `columnar-basic` (default), `columnar-delta`, `smallest` |
| page-packing token | Packing strategy for compressed pages. | `estimate-shrink` (default), `tight-fit` |
| `compress-partial-page` token | Compress the final partial page instead of leaving the final non-overflowing remainder raw for append. | omitted (default), present |

## Status

This repository is intentionally separate from the original C# implementation:
<https://github.com/stcmz/Mozo.Fwob>. The v1 crate is designed as a production
compatibility layer rather than a one-off converter so conversions can be
verified byte-for-byte against existing files.
