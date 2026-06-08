# FWOB v1 Format

FWOB v1 is the format implemented by the original C# `Mozo.Fwob` library.

## Layout

```text
Header:       214 bytes
String table: StringTablePreservedLength bytes
Frames:       FrameCount * FrameLength bytes
```

The default header plus preserved string-table region is 2048 bytes.

## Header

All numeric values are little-endian.

```text
offset  size  field
0       4     signature: "FWOB"
4       1     version: 1
5       1     field_count
6       16    field_lengths
22      8     field_types, 4 bits per field
30      128   field_names, 16 names * 8 bytes/chars in original writer
158     4     string_count
162     4     string_table_length
166     4     string_table_preserved_length
170     8     frame_count
178     4     frame_length
182     16    frame_type
198     16    title
```

The Rust implementation parses v1 headers with the same compatibility
constraints as the C# implementation, including strict schema length checks and
file-length validation.

## Field Types

The C# implementation stores one 4-bit field type value per logical field.

```text
0 = signed integer
1 = unsigned integer
2 = floating point
3 = UTF-8/fixed string
4 = string-table index
```

The exact primitive size is inferred from the field length and type family.

## String Table

Strings are stored using .NET `BinaryWriter.Write(string)`: a 7-bit encoded byte
length followed by UTF-8 bytes.

## Frames

Frames are fixed width. Primitive fields are stored as little-endian binary
values. Fixed string fields occupy their declared length in bytes for Rust
compatibility; conversion preserves raw frame bytes.

## Range Semantics

The C# README describes ranges as `[firstKey, lastKey)`, but the implementation
uses `UpperBound(lastKey)`. Therefore v1 compatibility treats
`GetFramesBetween(firstKey, lastKey)` and `DeleteFramesBetween(firstKey,
lastKey)` as inclusive of `lastKey`.

## Key Field Compatibility

FWOB v1 files do not store the key field index. The original C# API receives the
key through `FwobFile<TFrame, TKey>` and optional `[Key]` metadata on the user
type. The Rust v1 compatibility layer operates dynamically from the file header,
so key-aware operations accept a key field index. Existing production files are
expected to use field `0` as the key, and the CLI defaults to field `0`.

## UTF-8 Compatibility

String-table values are UTF-8 because v1 uses .NET `BinaryWriter.Write(string)`.
FWOB v2 is explicitly UTF-8 for schema text and string-table text.

For v1 fixed-width string frame fields, the original C# implementation writes
`char[]` through `BinaryWriter`. Existing production files use ASCII text, which
is fully supported. Non-ASCII fixed-width v1 frame strings are intentionally not
used as a compatibility requirement until a real fixture demonstrates the exact
legacy byte layout needed.
