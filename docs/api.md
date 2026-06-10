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

## Typed Frames

Derive `fwob_core::FwobFrame` to map a Rust struct to a fixed-width schema:

```rust
use fwob_core::{FwobFrame, StringIndex};

#[derive(FwobFrame)]
struct Tick {
    #[fwob(key)]
    time: i64,
    price: u32,
    size: i32,
    symbol: [u8; 8],
    #[fwob(string_index)]
    venue: StringIndex,
    #[fwob(ignore)]
    transient_state: u8,
}
```

`TypedReader`, `TypedAppender`, and `TypedEditor` enforce exact schema
compatibility when opening a file and expose typed frame/key operations over
both v1 and v2. Stored fields support all signed and unsigned integer widths,
`f32`, `f64`, fixed `[u8; N]` data, and `StringIndex`. Keys are currently
restricted to integer fields because the common ordered-key representation does
not define floating-point ordering.

Ignored fields are not stored and are initialized with `Default::default()`
when a frame is decoded. Exactly one stored field must have `#[fwob(key)]`.

### Typed API Complexity

Typed operations have the same asymptotic I/O and memory complexity as their
untyped counterparts. Encoding and decoding add `O(F)` CPU work and `O(F)`
reusable buffer space per frame, where `F` is the fixed frame size. Streams
decode one frame at a time and remain bounded independently of file size.

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

### Reader Complexity

Let `N` be total frames, `P` the number of v2 storage units, and `D` the cost
of decoding one v2 unit.

| Operation | v1 time | v2 time | Extra memory |
| --- | --- | --- | --- |
| frame/key by index | `O(1)` | `O(log P + D)` | one frame / one decoded unit |
| first/last frame or key | `O(1)` | `O(log P + D)` | one frame / one decoded unit |
| lower/upper/equal range | `O(log N)` | `O(log N * (log P + D))`, with unit cache reuse | one decoded unit |
| stream `K` frames | `O(K)` | `O(P touched * D + K)` | one decoded unit |

## Validation

Shared schemas reject empty or duplicate names, invalid key indexes,
non-contiguous offsets, invalid field widths, frame-length overflow, and key
fields without a defined total ordering. Floating payload fields may be 4, 8,
or 16 bytes for compatibility with C# `float`, `double`, and `decimal`; they
cannot currently be keys.

V1 creation additionally enforces its fixed legacy header limits: at most 16
fields, 8-byte ASCII field names, 16-byte ASCII frame type and title, and
single-byte field lengths. V2 permits UTF-8 metadata, restricts page size to
`1KiB..16MiB`, parses metadata strictly as UTF-8, and parses all variable
metadata inside the fixed 4 KiB file-header boundary.

## Bounded-Memory Editing

`AnyEditor` supports deletion by:

- one global frame index
- a half-open global frame-index range
- one key
- an inclusive key range
- all frames

Deletion uses copy-on-write:

1. Open the original file read-only.
2. Create a sibling temporary file.
3. Stream retained frames through a 4 MiB bounded copy buffer.
4. Finish and verify the replacement.
5. Atomically persist the temporary file over the original.

The original file remains intact if streaming, writing, or verification fails.
Memory usage is independent of total file size. V2 rewrites automatically
regenerate contiguous `first_frame_index` values.

The previous v1 whole-file implementation remains available as
`fwob_v1::InMemoryEditor`. Its name explicitly identifies that it loads every
frame and is not suitable for large production files.

Title and string-table changes use the same atomic bounded-memory rewrite.
`AnyEditor::update_metadata` can apply both changes in one pass. V1 rewrites
retain at least the original reserved string-table capacity and grow it when
needed.

### Editing Complexity

Let `N` be total frames, `D` deleted frames, `F` fixed frame size, and `B` the
bounded copy buffer.

| Operation | Time | Extra memory | I/O |
| --- | --- | --- | --- |
| delete one frame | `O(N)` | `O(B)` | rewrite `N - 1` frames |
| delete index range | `O(N)` | `O(B)` | rewrite `N - D` frames |
| delete one key | `O(log N + N)` | `O(B)` | rewrite `N - D` frames |
| delete key range | `O(log N + N)` | `O(B)` | rewrite `N - D` frames |
| delete all frames | `O(1)` logical selection, format rewrite | `O(B)` | metadata-only or empty-file rewrite |
| title/string-table update | `O(N)` | `O(B + S)` | rewrite all frames and `S` metadata bytes |

## File Organization

`split_by_keys` emits same-format files at the lower bound of each supplied
first key. `concat_files` accepts same-format inputs with identical schema and
title, globally ordered keys, and compatible string tables. String tables are
compatible when one is a matching prefix of the other; the longest table is
written, matching the original C# behavior.

Both operations stream through a 4 MiB frame buffer, verify each completed
output, and atomically publish it. Splitting `N` frames into `M` parts takes
`O(M log N + N)` time. Concatenation takes `O(N)` time after `O(M)` metadata
and boundary checks. Extra frame-copy memory is `O(B)` and independent of file
size.
