# Logical Rust API

The `fwob` crate provides version-neutral logical file APIs over FWOB v1 and
v2. Consumers work with files, schemas, frames, keys, and strings. Storage
details such as v2 pages, codecs, encodings, and append tails remain inside the
format implementation.

## Core Contracts

`fwob-core` owns the format-neutral contracts and handles:

- `FileInfo` exposes common metadata.
- `ReaderBackend` and `WriterBackend` are object-safe format implementation
  contracts.
- `WriterFactory` preserves format-specific rewrite settings without making
  writer construction a reader-backend responsibility.
- `Reader` and `Writer` own boxed backends and expose logical operations without
  a version match on each call.
- `Editor`, `Maintenance`, and `Organizer` separate mutation, physical
  validation/recovery, and file organization from ordinary reads and writes.

`fwob-v1` and `fwob-v2` implement the backend and maintenance contracts. Their
`open_core_reader`, `create_core_writer`, and `open_core_writer` functions are
available to applications that already know the format version.

The `fwob` crate is the normal consumer entry point. Its `Reader`, `Writer`, and
`Editor` types detect v1 or v2 while opening a file, then delegate through the
core contracts. `Maintenance` groups light verification, full verification, and
repair. `Organizer` groups split and concatenation.

The facade source is organized by responsibility: `reader.rs`, `writer.rs`,
`editor.rs`, `maintenance.rs`, `organization.rs`, and `typed.rs`. `lib.rs`
contains only shared errors, format detection, module declarations, and public
re-exports.

V1 files do not store their key-field index. `Reader::open_with_options` and
`ReaderOptions::v1_key_field_index` allow callers to supply it; field zero is
the default. `OperationOptions` carries the reader settings and optional v2
write settings used by append, deletion, split, and concatenation.

V2 writer options are accepted when opening an appender or editor and when
constructing an organizer. They remain outside the common reader/writer traits
because compression and packing are format implementation details.

Reader, writer, editor, maintenance, and organization conformance tests execute
the same logical assertions against both formats.

## Examples

All snippets use the version-neutral `fwob` facade unless physical v1/v2
details are explicitly needed.

### Read Frames and Ranges

```rust
use fwob::Reader;
use fwob_core::Key;

let mut reader = Reader::open("ticks.fwob")?;
println!("frames: {}", reader.frame_count());

let first = reader.first_frame()?;
let frame = reader.read_frame(100)?;
let matching = reader.equal_range(Key::I64(123))?;

for frame in reader.frames(matching)? {
    println!("{:?}", frame?.bytes());
}

for frame in reader.frames_by_keys(&[
    Key::I64(123),
    Key::I64(456),
    Key::I64(789),
])? {
    println!("{:?}", frame?.bytes());
}
# Ok::<(), fwob::Error>(())
```

`frames` and `frames_by_key` are lazy iterators. V1 reads by direct offset; v2
retains one decoded internal storage unit as a reusable cache.
`frames_by_keys` accepts sorted keys, skips missing keys, and returns duplicate
query keys only once.
`frames_before(last_key)` and `frames_after(first_key)` provide inclusive
one-sided key ranges.

### Create and Write

```rust
use fwob::Writer;
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
# Ok::<(), fwob::Error>(())
```

### Append

```rust
use fwob::{OperationOptions, Writer};

let mut writer = Writer::open("ticks.fwob", OperationOptions::default())?;
writer.append_frame(&456_i64.to_le_bytes())?;
writer.finish()?;
# Ok::<(), fwob::Error>(())
```

The same `OperationOptions` value can be supplied to
`Editor::open_with_operation_options`. Its `v2` field selects explicit
compression settings for both append and deletion; `None` inherits from the
file.

Use `append_frames_transactional` when the entire raw batch must be validated
before any frame is accepted:

```rust
let mut frames = Vec::new();
frames.extend_from_slice(&456_i64.to_le_bytes());
frames.extend_from_slice(&789_i64.to_le_bytes());
writer.append_frames_transactional(&frames)?;
```

`TypedWriter::append_all_transactional` provides the same behavior for typed
frames. Transactionality covers frame validation and key ordering. An operating
system I/O failure after validation can still leave an interrupted write, which
`Maintenance::repair` handles.

### Edit

```rust
use fwob::Editor;
use fwob_core::Key;

let mut editor = Editor::open("ticks.fwob")?;
editor.delete_frame(10)?;
editor.delete_frames(20..30)?;
editor.delete_indices(&[3, 8, 13])?;
editor.delete_ranges(&[20..30, 40..45])?;
editor.delete_key(Key::I64(123))?;
editor.delete_keys(&[Key::I64(123), Key::I64(456)])?;
editor.delete_key_range(Key::I64(200)..=Key::I64(300))?;
editor.delete_before(Key::I64(100))?;
editor.delete_after(Key::I64(1_000))?;
editor.set_title("updated prices")?;
# Ok::<(), fwob::Error>(())
```

### Verify and Repair

```rust
use fwob::{Maintenance, ReaderOptions};

let report = Maintenance::verify("ticks.fwob", ReaderOptions::default())?;
println!("version: {:?}", report.format_version);
println!("frames: {}", report.frame_count);

Maintenance::repair("ticks.fwob", ReaderOptions::default())?;
# Ok::<(), fwob::Error>(())
```

### Split and Concatenate

```rust
use fwob::{OperationOptions, Organizer};
use fwob_core::Key;

let organizer = Organizer {
    operation_options: OperationOptions::default(),
    ..Default::default()
};
let parts = organizer.split(
    "ticks.fwob",
    "parts",
    &[Key::I64(1_000), Key::I64(2_000)],
)?;
organizer.concat("joined.fwob", &parts)?;
# Ok::<(), fwob::Error>(())
```

### Typed Frames

```rust
use fwob_core::FwobFrame;

#[derive(Debug, FwobFrame)]
struct Tick {
    #[fwob(key)]
    time: i64,
    price: u32,
    size: i32,
}
```

#### Typed Writer

```rust
use fwob::TypedWriter;

let mut writer = TypedWriter::<Tick>::create_v2(
    "ticks.fwob",
    fwob_v2::WriterOptions::new("prices"),
)?;
writer.append(&Tick {
    time: 123,
    price: 500,
    size: 10,
})?;
writer.finish()?;
```

#### Typed Reader

```rust
use fwob::TypedReader;

let mut reader = TypedReader::<Tick>::open("ticks.fwob")?;
let tick = reader.read_frame(0)?;
let matches = reader.frames_by_key(100..=200)?;
```

#### Typed Editor

```rust
use fwob::TypedEditor;

let mut editor = TypedEditor::<Tick>::open("ticks.fwob")?;
editor.delete_indices(&[3, 8, 13])?;
editor.delete_ranges(&[20..30, 40..45])?;
editor.delete_key(123)?;
# Ok::<(), fwob::Error>(())
```

### Format-Specific Access

Use a format crate directly only when physical details matter:

```rust
let mut reader = fwob_v2::Reader::open("ticks.fwob")?;
let header = reader.header();
let page = reader.read_page_header(0)?;
let frames = reader.read_page_frames(0)?;
# Ok::<(), fwob_v2::V2Error>(())
```

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

`TypedReader`, `TypedWriter`, and `TypedEditor` enforce exact schema
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

The v1 and v2 implementations use a shared-window equal-range algorithm rather
than running independent full-range lower- and upper-bound searches.

Streams hold only their logical next/end indexes. V1 reads frames by direct
offset. V2 locates the internal storage unit using `first_frame_index` and
retains one decoded unit as a reusable cache. Total stream memory is bounded
independently of file size.

### Reader Complexity

Let `N` be total frames, `P` the number of v2 pages, `Q` the frames in one
page, and `D` the cost of reading, decompressing, and decoding one page.

| Operation | v1 time | v2 time | Extra memory |
| --- | --- | --- | --- |
| frame/key by index | `O(1)` | `O(log P + D)` | one frame / one decoded unit |
| first/last key | `O(1)` | `O(1)` | `O(1)` |
| first/last frame | `O(1)` | `O(log P + D)` | one frame / one decoded unit |
| lower/upper bound | `O(log N)` | `O(log P + D + log Q)` | one decoded unit |
| equal range | `O(log N)` | currently `O(log N * (log P + D))` worst case | one decoded unit |
| stream `K` frames | `O(K)` | `O(P touched * D + K)` | one decoded unit |

V2 lower and upper bounds first binary-search page headers, decode one boundary
page, and then binary-search frames within that page. Their costs are additive.
The current shared-window `equal_range` instead binary-searches global frame
indexes and calls indexed key lookup at each step, so its worst case remains
multiplicative despite page-cache reuse.

## Validation

Shared schemas reject empty or duplicate names, invalid key indexes,
non-contiguous offsets, invalid field widths, frame-length overflow, and key
fields without a defined total ordering. Floating payload fields may be 4, 8,
or 16 bytes; they cannot currently be keys.

V1 creation additionally enforces its fixed legacy header limits: at most 16
fields, 8-byte ASCII field names, 16-byte ASCII frame type and title, and
single-byte field lengths. V2 permits UTF-8 metadata, restricts page size to
`1KiB..16MiB`, parses metadata strictly as UTF-8, and parses all variable
metadata inside the fixed 4 KiB file-header boundary.

## Bounded-Memory Editing

`Editor` supports deletion by:

- one global frame index
- a half-open global frame-index range
- one key
- an ordered set of keys
- an inclusive key range
- an inclusive upper or lower key bound
- all frames

V2 deletion localizes decoding and recompression to the physical pages from
the first deleted frame through the last deleted frame. Retained frames from
that interval stream through the normal page packer into replacement pages.
Later physical pages are moved forward without decoding or recompressing their
payloads; only their `first_frame_index` and header CRC are updated.

`OperationOptions::deletion_packing` selects one of two v2 strategies:

- `DeletionPacking::LocalRepack` rebuilds the affected interval and finishes
  its partial replacement page with the selected codec. Later page payloads
  remain intact. If the selected representation needs more pages, the interval
  expands forward only until the replacement fits.
- `DeletionPacking::RepackToEnd` consumes pages through EOF and maximally
  repacks the remainder. Its `v2.compress_partial_page` setting controls whether
  the final EOF page is compressed or left raw.

The CLI tokens are `local-repack` and `repack-to-end`.

Append, deletion, split, and concatenation accept the same `OperationOptions`.
`v2: None` inherits codec and encoding from the existing file;
`v2: Some(...)` supplies explicit per-operation compression and packing
settings. Schema, page size, title, and string table come from the existing
file. The CLI codec token for raw pages is `uncompressed`.

V1 compacts in place beginning at the first deleted frame, preserving the file
prefix byte-for-byte and moving each retained suffix block at most once. This
avoids temporary space proportional to the file, but an interrupted in-place
compaction is not copy-on-write transactional.

`delete_indices`, `delete_ranges`, and `delete_keys` remove all selected
disjoint ranges in one mutation. Indexes must be strictly increasing; ranges
must be ordered and non-overlapping; keys must be nondecreasing. Invalid input
is rejected before the original file is modified.

Readers expose both directions of string-table lookup:

```rust
assert_eq!(reader.string_at(3), Some("NASDAQ"));
assert_eq!(reader.string_index("NASDAQ"), Some(3));
assert!(reader.contains_string("NASDAQ"));
```

`string_at` is `O(1)`. The first `string_index` or `contains_string` call lazily
builds an `O(S)` reverse index; later lookups are `O(1)` average. Duplicate
values resolve to their last table index. `TypedReader` exposes the same
methods.

The previous v1 whole-file implementation remains available as
`fwob_v1::InMemoryEditor`. Its name explicitly identifies that it loads every
frame and is not suitable for large production files.

`Editor::update_metadata` can apply title and string-table changes together.
V1 updates its fixed metadata prefix in place without reading or rewriting
frames; replacement strings must fit the capacity reserved when the file was
created. For v2, a title-only replacement with the same UTF-8 byte length
overwrites only the title bytes. A different title length moves all following
length-prefixed metadata, so that case and string-table changes reserialize the
fixed 4 KiB header. Pages are never touched. An interrupted in-place metadata
write is not a copy-on-write transaction.

### Editing Complexity

Let `N` be total frames, `E` the end of a contiguous deleted range, `A` the
beginning of the first deleted range, `P` the affected v2 pages, `T` the later
v2 pages physically moved, and `B` the bounded copy buffer.

| Operation | Time | Extra memory | I/O |
| --- | --- | --- | --- |
| v1 delete one frame/range | `O(N - E)` after selection | `O(B)` | compact only retained frames after the deleted range |
| v1 delete ordered indices/ranges | `O(N - A)` | `O(B)` | compact once from the first deletion through the old end |
| v1 delete key/range | `O(log N + N - E)` | `O(B)` | binary search, then suffix compaction |
| v1 delete all frames | `O(1)` | `O(1)` | truncate to the fixed metadata prefix |
| v2 deletion | `O(P decode/encode + T)`; `O(N)` worst case | `O(B + one decoded page)` | rebuild affected pages; byte-move later pages and rewrite only their headers |
| v1 title update | `O(1)` | `O(1)` | overwrite the fixed 16-byte title field |
| v1 string-table update | `O(S)` | `O(S)` | overwrite `S` encoded metadata bytes; frames are untouched |
| v2 same-length title update | `O(1)` | `O(1)` | overwrite only the UTF-8 title bytes |
| v2 resized title/string-table update | `O(1)` | `O(1)` | reserialize and rewrite the format-bounded 4 KiB header |

V2 deletion reaches the `O(N)` worst case when deletion starts near the
beginning and either `repack-to-end` is selected or local output occupies fewer
physical pages. The implementation must then process or move nearly every
following page. Even unchanged page payloads need updated `first_frame_index`
and header CRC values; a page-count reduction also shifts those pages forward.

## File Organization

`Organizer::split` emits same-format files at the lower bound of each supplied
first key. `Organizer::concat` accepts same-format inputs with identical schema
and title, globally ordered keys, and compatible string tables. String tables
are compatible when one is a matching prefix of the other; the longest table
is written.

`Organizer` is operation-stateful but not bound to one open file. It owns
`OperationOptions` and applies them to every split or concat call. This matches
the configuration model of `Writer` and `Editor` without retaining file handles
or making a multi-source concat pretend to belong to one source file. V2 output
preserves the source page size; explicit options control codec, encoding,
compression level, packing, and partial-page handling.

Both operations stream through a 4 MiB buffer, verify each completed output,
and atomically publish it. V1 copies raw frame byte ranges without decoding or
re-encoding frames. V2 streams logical frames because page boundaries,
compression, checksums, and `first_frame_index` values must be rebuilt.
Splitting `N` frames into `M` parts takes `O(M log N + N)` time. Concatenation
takes `O(N)` time after `O(M)` metadata and boundary checks. Extra copy memory
is `O(B)` and independent of file size.
