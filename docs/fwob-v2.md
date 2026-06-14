# FWOB v2 Format

FWOB v2 is a fixed-page compressed format optimized for:

- high-performance range query
- random access
- bulk append
- mostly immutable datasets
- strong page-local compression

## Layout

```text
File header
Page 0
Page 1
Page 2
...
```

Every page has the same on-disk size. Page offsets are arithmetic:

```text
page_offset = file_header_length + page_index * page_size
```

## Tuning Parameters

| Parameter | What It Controls | Typical Values |
| --- | --- | --- |
| page-size token | Fixed physical page size. Integer with `B`, `KB`, `KiB`, `MB`, or `MiB`; range `1KiB..16MiB`. | `512KiB` (default), `1MB`, `1MiB`, `2MiB` |
| codec token | Page compression codec. | `zstd` (default), `lz4`, `smallest`, `uncompressed` |
| `--zstd-level` | zstd compression level. Affects write/convert speed heavily, read speed lightly. | `3`, `6` (default), `9`, `12`, `15`, `19` |
| encoding token | Page payload layout before compression. `smallest` tries columnar-basic and columnar-delta per page and records the winning concrete encoding in page metadata. | `row-raw`, `columnar-basic` (default), `columnar-delta`, `smallest` |
| `compress-partial-page` token | Compress the final partial output page instead of leaving the non-overflowing remainder raw. | omitted (default), present |

## Page

```text
64-byte page header
compressed payload
zero padding
```

The page header stores `first_frame_index`, `frame_count`, `first_key`,
`last_key`, `uncompressed_len`, `compressed_len`, `codec`, `encoding`, flags,
and CRCs.

`first_frame_index` is the global logical index of the first frame in the page.
It permits binary search by frame index without a separate index table. Page
indexes must be contiguous:

```text
page[0].first_frame_index = 0
page[n].first_frame_index =
    page[n - 1].first_frame_index + page[n - 1].frame_count
```

Verification and interrupted-write repair enforce this invariant.

## Codecs

```text
0 = uncompressed
1 = zstd
2 = lz4
```

Zstd pages are written with a configurable compression level. The CLI default is
level `3`; higher levels can improve density at the cost of slower conversion or
compaction. The CLI accepts zstd levels `1..=22`. LZ4 support currently uses the
fast block compressor and does not expose high-compression levels.

## Encodings

```text
0 = row_raw_v1
1 = columnar_basic_v1
2 = columnar_delta_v1
```

`row_raw_v1` stores raw fixed-width frames before page compression. It is the
first implementation target because it enables lossless v1 conversion.

`columnar_basic_v1` stores the same fixed-width frame bytes transposed by field
inside each page:

```text
row_raw_v1:
frame0.field0, frame0.field1, frame0.field2
frame1.field0, frame1.field1, frame1.field2

columnar_basic_v1:
frame0.field0, frame1.field0
frame0.field1, frame1.field1
frame0.field2, frame1.field2
```

It is intentionally simple: no deltas, bit packing, or RLE. It exists to
measure the compression potential of page-local columnar layout while preserving
independent random page reads.

`columnar_delta_v1` uses the same field-major layout, but integer fields are
stored as `first value + fixed-width deltas` within each page. Floating-point
and byte/string fields remain raw field-major bytes. It is still page-local and
does not depend on prior pages.

## Compression Model

Compression is transparent to readers. Callers request records or ranges; the
reader locates pages via arithmetic offsets, reads page headers, decompresses
matching pages into reusable buffers, and scans or binary-searches inside the
decoded page.

No separate index table is required.

## Logical Reader

The public logical API does not expose pages. It supports:

- global frame and key access by index
- first and last frame/key access
- lower bound, upper bound, and equal range
- lazy index-range and inclusive key-range iteration

`equal_range` uses a shared-window search: the lower-bound search records the
smallest known greater-key position, and the upper-bound search is restricted
to that window.

## Raw Tail Buffer for Append

FWOB v2 supports uncompressed pages with `codec = 0`. The writer treats trailing
raw pages as an on-disk append buffer:

```text
compressed pages...
raw tail page 0
raw tail page 1
...
possibly incomplete final raw page
```

With a compression codec, the writer defers compaction until the logical raw
tail reaches at least four raw-page capacities and overflows its current
physical allocation. It then pulls back all trailing raw pages, appends the new
frames, and tries to compress the full raw tail. If the compressed tail
overflows a page, the writer emits as many maximally packed compressed pages as
possible. Whatever does not overflow one compressed page remains raw unless
`compress-partial-page` is set. Existing compressed prefix pages remain in
place. With the uncompressed codec, a full raw page is flushed when the next
frame would overflow it.

Compressed pages are always built from the largest available frame prefix that
fits in one fixed-size page. The final remainder is raw instead of becoming a
poorly utilized compressed page unless the user asks to compress partial pages.
