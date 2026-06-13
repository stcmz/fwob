# FWOB v1 Format

FWOB v1 is a fixed-width ordered binary format with a fixed metadata header,
reserved string-table region, and contiguous frames.

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
30      128   field_names, 16 names * 8 bytes
158     4     string_count
162     4     string_table_length
166     4     string_table_preserved_length
170     8     frame_count
178     4     frame_length
182     16    frame_type
198     16    title
```

Readers enforce strict schema-length and physical file-length validation.

## Field Types

The header stores one 4-bit field type value per logical field.

```text
0 = signed integer
1 = unsigned integer
2 = floating point
3 = UTF-8/fixed string
4 = string-table index
```

The exact primitive size is inferred from the field length and type family.

## String Table

Strings use a 7-bit encoded byte length followed by UTF-8 bytes.

## Frames

Frames are fixed width. Primitive fields are stored as little-endian binary
values. Fixed string fields occupy their declared length in bytes.

## Range Semantics

Key-range reads and deletions are inclusive at both ends.

## Key Field Compatibility

FWOB v1 files do not store the key field index, so key-aware operations accept
it as an open option. The CLI defaults to field `0`.

## UTF-8 Compatibility

String-table values are UTF-8. Fixed-width string frame fields are stored as
their declared raw bytes.
