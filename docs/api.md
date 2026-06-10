# Logical Rust API

The `fwob` crate provides version-neutral logical file APIs over FWOB v1 and
v2. Consumers work with files, schemas, frames, keys, and strings. Storage
details such as v2 pages, codecs, encodings, and append tails remain inside the
format implementation.

## Facades

- `AnyReader` detects v1 or v2 when opening a file.
- `AnyAppender` detects v1 or v2 and appends through one interface.
- `FwobFile` exposes common metadata.
- `FwobReader` exposes frame/key access, bounds, equal range, and lazy streams.
- `FwobAppender` exposes ordered append and explicit finalization.

V1 files do not store their key-field index. `AnyReader::open_with_v1_key` and
`AppendOptions::v1_key_field_index` allow callers to supply it; field zero is
the default.

V2 writer options are accepted only while opening a v2 appender. They do not
become part of the common appender trait because compression and packing are
format implementation details.

Reader and appender conformance tests execute the same logical assertions
against both formats.

## Reader Semantics

Frame index ranges are half-open (`start..end`). Key ranges are inclusive
(`first..=last`). `equal_range(key)` returns the half-open frame-index range
containing all frames equal to the key.

The v1 and v2 implementations use the original C# shared-window equal-range
algorithm rather than running independent full-range lower- and upper-bound
searches.

Streams hold only their logical next/end indexes. V1 reads frames by direct
offset. V2 locates the internal storage unit using `first_frame_index` and
retains one decoded unit as a reusable cache. Total stream memory is bounded
independently of file size.
