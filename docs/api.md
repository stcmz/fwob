# Logical Rust API

The `fwob` crate provides version-neutral logical file APIs over FWOB v1 and
v2. Consumers work with files, schemas, frames, keys, and strings. Storage
details such as v2 pages, codecs, encodings, and append tails remain inside the
format implementation.

## Facades

- `AnyReader` detects v1 or v2 when opening a file.
- `AnyAppender` detects v1 or v2 and appends through one interface.
- `FwobFile` exposes common metadata.
- `FwobReader` exposes logical reads.
- `FwobAppender` exposes ordered append and explicit finalization.

V1 files do not store their key-field index. `AnyReader::open_with_v1_key` and
`AppendOptions::v1_key_field_index` allow callers to supply it; field zero is
the default.

V2 writer options are accepted only while opening a v2 appender. They do not
become part of the common appender trait because compression and packing are
format implementation details.

Reader and appender conformance tests execute the same logical assertions
against both formats.
