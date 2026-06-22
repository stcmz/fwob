use std::{fmt::Write as _, io::Write};

use fwob_core::{decode_decimal, Field, FieldSemantic, FieldType, Schema, TimestampUnit};

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFormat {
    Raw,
    Table,
    Markdown,
    Csv,
    JsonLines,
    Hex,
}

impl FrameFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "raw" => Some(Self::Raw),
            "table" => Some(Self::Table),
            "md" => Some(Self::Markdown),
            "csv" => Some(Self::Csv),
            "jsonl" => Some(Self::JsonLines),
            "hex" => Some(Self::Hex),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Table => "table",
            Self::Markdown => "md",
            Self::Csv => "csv",
            Self::JsonLines => "jsonl",
            Self::Hex => "hex",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Signed(i64),
    Unsigned(u64),
    Float32(f32),
    Float64(f64),
    Decimal(fwob_core::Decimal),
    String(String),
}

pub struct FrameDecoder<'a> {
    schema: &'a Schema,
    string_table: &'a [String],
}

impl<'a> FrameDecoder<'a> {
    pub fn new(schema: &'a Schema, string_table: &'a [String]) -> Self {
        Self {
            schema,
            string_table,
        }
    }

    pub fn decode(&self, frame: &[u8]) -> Result<Vec<FieldValue>> {
        self.schema.validate_frame_len(frame.len())?;
        self.schema
            .fields
            .iter()
            .map(|field| self.decode_field(field, frame))
            .collect()
    }

    fn decode_field(&self, field: &Field, frame: &[u8]) -> Result<FieldValue> {
        let start = field.offset as usize;
        let end = start + field.length as usize;
        let bytes = frame
            .get(start..end)
            .ok_or_else(|| Error::InvalidFieldSlice(field.name.clone()))?;
        match field.field_type {
            FieldType::SignedInteger => Ok(FieldValue::Signed(decode_signed(bytes)?)),
            FieldType::UnsignedInteger => Ok(FieldValue::Unsigned(decode_unsigned(bytes)?)),
            FieldType::FloatingPoint => match bytes.len() {
                4 => Ok(FieldValue::Float32(f32::from_le_bytes(
                    bytes.try_into().unwrap(),
                ))),
                8 => Ok(FieldValue::Float64(f64::from_le_bytes(
                    bytes.try_into().unwrap(),
                ))),
                16 => Ok(FieldValue::Decimal(decode_decimal(bytes)?)),
                _ => Err(Error::InvalidFieldSlice(field.name.clone())),
            },
            FieldType::Utf8String => Ok(FieldValue::String(
                String::from_utf8_lossy(bytes)
                    .trim_end_matches('\0')
                    .trim_end()
                    .to_string(),
            )),
            FieldType::StringTableIndex => {
                let index = decode_unsigned(bytes)?;
                let value = self
                    .string_table
                    .get(
                        usize::try_from(index)
                            .map_err(|_| Error::InvalidStringTableIndex(index))?,
                    )
                    .ok_or(Error::InvalidStringTableIndex(index))?;
                Ok(FieldValue::String(value.clone()))
            }
        }
    }
}

pub struct FrameFormatter<'a> {
    schema: &'a Schema,
    decoder: FrameDecoder<'a>,
    format: FrameFormat,
    widths: Vec<usize>,
    /// Per-column alignment: `true` = right-align (numeric columns), `false` = left (strings).
    right_aligned: Vec<bool>,
    header_written: bool,
}

impl<'a> FrameFormatter<'a> {
    pub fn new(schema: &'a Schema, string_table: &'a [String], format: FrameFormat) -> Self {
        let widths = schema
            .fields
            .iter()
            .map(|field| field_width(field, string_table))
            .collect();
        let right_aligned = schema
            .fields
            .iter()
            .map(|field| is_numeric(field.field_type))
            .collect();
        Self {
            schema,
            decoder: FrameDecoder::new(schema, string_table),
            format,
            widths,
            right_aligned,
            header_written: false,
        }
    }

    pub fn write_header(&mut self, output: &mut impl Write) -> Result<()> {
        if self.header_written {
            return Ok(());
        }
        match self.format {
            FrameFormat::Table => {
                let values = self
                    .schema
                    .fields
                    .iter()
                    .map(|field| field.name.as_str())
                    .collect::<Vec<_>>();
                write_padded_row(output, &values, &self.widths, &self.right_aligned)?;
            }
            FrameFormat::Markdown => {
                write_markdown_row(
                    output,
                    &self
                        .schema
                        .fields
                        .iter()
                        .map(|field| field.name.as_str())
                        .collect::<Vec<_>>(),
                )?;
                let separators = self
                    .schema
                    .fields
                    .iter()
                    .map(|field| {
                        if is_numeric(field.field_type) {
                            "---:"
                        } else {
                            "---"
                        }
                    })
                    .collect::<Vec<_>>();
                write_markdown_row(output, &separators)?;
            }
            FrameFormat::Csv => {
                let values = self
                    .schema
                    .fields
                    .iter()
                    .map(|field| field.name.as_str())
                    .collect::<Vec<_>>();
                write_csv_row(output, &values)?;
            }
            FrameFormat::Raw | FrameFormat::JsonLines | FrameFormat::Hex => {}
        }
        self.header_written = true;
        Ok(())
    }

    pub fn write_frame(&mut self, output: &mut impl Write, frame: &[u8]) -> Result<()> {
        self.write_header(output)?;
        if self.format == FrameFormat::Hex {
            writeln!(output, "{}", hex(frame))?;
            return Ok(());
        }

        let values = self.decoder.decode(frame)?;
        match self.format {
            FrameFormat::Raw => {
                let rendered = values.iter().map(render_plain).collect::<Vec<_>>();
                writeln!(output, "{}", rendered.join(" "))?;
            }
            FrameFormat::Table => {
                let rendered = self
                    .schema
                    .fields
                    .iter()
                    .zip(&values)
                    .map(|(field, value)| render_display(field, value))
                    .collect::<Vec<_>>();
                let borrowed = rendered.iter().map(String::as_str).collect::<Vec<_>>();
                write_padded_row(output, &borrowed, &self.widths, &self.right_aligned)?;
            }
            FrameFormat::Markdown => {
                let rendered = self
                    .schema
                    .fields
                    .iter()
                    .zip(&values)
                    .map(|(field, value)| render_display(field, value))
                    .collect::<Vec<_>>();
                let borrowed = rendered.iter().map(String::as_str).collect::<Vec<_>>();
                write_markdown_row(output, &borrowed)?;
            }
            FrameFormat::Csv => {
                let rendered = self
                    .schema
                    .fields
                    .iter()
                    .zip(&values)
                    .map(|(field, value)| {
                        if fixed_point_null(field, value) {
                            String::new()
                        } else {
                            render_plain(value)
                        }
                    })
                    .collect::<Vec<_>>();
                let borrowed = rendered.iter().map(String::as_str).collect::<Vec<_>>();
                write_csv_row(output, &borrowed)?;
            }
            FrameFormat::JsonLines => {
                write_json_line(output, self.schema, &values)?;
            }
            FrameFormat::Hex => unreachable!(),
        }
        Ok(())
    }
}

fn decode_signed(bytes: &[u8]) -> Result<i64> {
    match bytes.len() {
        1 => Ok(bytes[0] as i8 as i64),
        2 => Ok(i16::from_le_bytes(bytes.try_into().unwrap()) as i64),
        4 => Ok(i32::from_le_bytes(bytes.try_into().unwrap()) as i64),
        8 => Ok(i64::from_le_bytes(bytes.try_into().unwrap())),
        _ => Err(Error::InvalidIntegerWidth(bytes.len())),
    }
}

fn decode_unsigned(bytes: &[u8]) -> Result<u64> {
    match bytes.len() {
        1 => Ok(u64::from(bytes[0])),
        2 => Ok(u64::from(u16::from_le_bytes(bytes.try_into().unwrap()))),
        4 => Ok(u64::from(u32::from_le_bytes(bytes.try_into().unwrap()))),
        8 => Ok(u64::from_le_bytes(bytes.try_into().unwrap())),
        _ => Err(Error::InvalidIntegerWidth(bytes.len())),
    }
}

fn render_plain(value: &FieldValue) -> String {
    match value {
        FieldValue::Signed(value) => value.to_string(),
        FieldValue::Unsigned(value) => value.to_string(),
        FieldValue::Float32(value) => value.to_string(),
        FieldValue::Float64(value) => value.to_string(),
        FieldValue::Decimal(value) => value.to_string(),
        FieldValue::String(value) => value.clone(),
    }
}

fn numeric_i128(value: &FieldValue) -> Option<i128> {
    match value {
        FieldValue::Signed(value) => Some(i128::from(*value)),
        FieldValue::Unsigned(value) => Some(i128::from(*value)),
        _ => None,
    }
}

/// The most-negative value of a signed integer of `length` bytes.
fn signed_min(length: u16) -> i128 {
    match length {
        1 => i128::from(i8::MIN),
        2 => i128::from(i16::MIN),
        4 => i128::from(i32::MIN),
        8 => i128::from(i64::MIN),
        other if (1..=16).contains(&other) => -(1i128 << (u32::from(other) * 8 - 1)),
        _ => i128::MIN,
    }
}

/// A fixed-point / percentage column over a *signed* integer treats that integer type's
/// minimum value as NULL (there is no NaN for integers). Such cells render as `-` / `null` /
/// empty in the display, json, and csv formats; `raw`/`hex` keep the exact stored integer.
fn fixed_point_null(field: &Field, value: &FieldValue) -> bool {
    matches!(field.field_type, FieldType::SignedInteger)
        && matches!(
            field.semantic,
            FieldSemantic::FixedPoint(_) | FieldSemantic::Percentage(_)
        )
        && numeric_i128(value) == Some(signed_min(field.length))
}

fn render_display(field: &Field, value: &FieldValue) -> String {
    if fixed_point_null(field, value) {
        return "-".to_string();
    }
    match field.semantic {
        FieldSemantic::UnixTimestamp(unit) => {
            if let Some(rendered) =
                numeric_i128(value).and_then(|value| format_unix_timestamp(value, unit))
            {
                return rendered;
            }
        }
        FieldSemantic::FixedPoint(points) => {
            if let Some(value) = numeric_i128(value) {
                return format_fixed_point(value, points, false);
            }
        }
        FieldSemantic::Percentage(points) => {
            if let Some(value) = numeric_i128(value) {
                return format_fixed_point(value, points, true);
            }
        }
        FieldSemantic::None => {}
    }
    render_plain(value)
}

/// Render an integer as `value / 10^points` with exactly `points` fractional digits, comma-grouping
/// the integer part. `percent` appends a trailing `%`; `points == 0` yields a grouped integer.
fn format_fixed_point(value: i128, points: u8, percent: bool) -> String {
    let scale = 10u128.pow(u32::from(points));
    let magnitude = value.unsigned_abs();
    let integer = magnitude / scale;
    let fraction = magnitude % scale;

    let mut out = String::new();
    if value < 0 {
        out.push('-');
    }
    out.push_str(&group_number(&integer.to_string()));
    if points > 0 {
        out.push('.');
        out.push_str(&format!("{fraction:0width$}", width = usize::from(points)));
    }
    if percent {
        out.push('%');
    }
    out
}

fn group_number(value: &str) -> String {
    let exponent = value.find(['e', 'E']).unwrap_or(value.len());
    let decimal = value[..exponent].find('.').unwrap_or(exponent);
    let integer = &value[..decimal];
    let (sign, digits) = integer
        .strip_prefix('-')
        .map_or(("", integer), |digits| ("-", digits));
    let mut grouped = String::with_capacity(value.len() + digits.len() / 3);
    grouped.push_str(sign);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    grouped.push_str(&value[decimal..]);
    grouped
}

fn is_numeric(field_type: FieldType) -> bool {
    matches!(
        field_type,
        FieldType::SignedInteger | FieldType::UnsignedInteger | FieldType::FloatingPoint
    )
}

/// Maximum decimal digit count of the magnitude representable in `length` integer bytes.
fn integer_digits(length: u16) -> usize {
    match length {
        1 => 3,
        2 => 5,
        4 => 10,
        8 => 20,
        other => (other as usize) * 3,
    }
}

fn timestamp_width(unit: TimestampUnit) -> usize {
    match unit {
        TimestampUnit::Seconds => 20,
        TimestampUnit::Milliseconds => 24,
        TimestampUnit::Microseconds => 27,
        TimestampUnit::Nanoseconds => 30,
    }
}

/// Worst-case rendered width of a fixed-point / percentage integer field: the integer part is
/// `digits - points` wide (with comma separators), plus the decimal point, fractional digits, an
/// optional `%`, and a sign for signed types.
fn fixed_point_width(field: &Field, points: u8, percent: bool) -> usize {
    let digits = integer_digits(field.length);
    let points = usize::from(points);
    let int_digits = digits.saturating_sub(points).max(1);
    let commas = int_digits.saturating_sub(1) / 3;
    let mut width = int_digits + commas;
    if points > 0 {
        width += 1 + points;
    }
    if percent {
        width += 1;
    }
    if matches!(field.field_type, FieldType::SignedInteger) {
        width += 1;
    }
    width
}

/// Width of an unformatted (no-semantic) value: raw integer digits (no commas), float estimate, or
/// max string length.
fn plain_width(field: &Field, string_table: &[String]) -> usize {
    match field.field_type {
        FieldType::SignedInteger => integer_digits(field.length) + 1,
        FieldType::UnsignedInteger => integer_digits(field.length),
        FieldType::FloatingPoint => match field.length {
            4 => 16,
            8 => 24,
            16 => 34,
            other => other as usize * 2,
        },
        FieldType::Utf8String => field.length as usize,
        FieldType::StringTableIndex => string_table
            .iter()
            .map(|value| value.chars().count())
            .max()
            .unwrap_or(1),
    }
}

fn field_width(field: &Field, string_table: &[String]) -> usize {
    let width = match field.semantic {
        FieldSemantic::UnixTimestamp(unit) => timestamp_width(unit),
        FieldSemantic::FixedPoint(points) => fixed_point_width(field, points, false),
        FieldSemantic::Percentage(points) => fixed_point_width(field, points, true),
        FieldSemantic::None => plain_width(field, string_table),
    };
    width.max(field.name.chars().count())
}

fn format_unix_timestamp(value: i128, unit: TimestampUnit) -> Option<String> {
    let divisor = match unit {
        TimestampUnit::Seconds => 1i128,
        TimestampUnit::Milliseconds => 1_000,
        TimestampUnit::Microseconds => 1_000_000,
        TimestampUnit::Nanoseconds => 1_000_000_000,
    };
    let seconds = value.div_euclid(divisor);
    let remainder = value.rem_euclid(divisor);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let days = i64::try_from(days).ok()?;
    let (year, month, day) = civil_from_days(days)?;
    let hour = seconds_of_day / 3_600;
    let minute = seconds_of_day % 3_600 / 60;
    let second = seconds_of_day % 60;
    let fraction = match unit {
        TimestampUnit::Seconds => String::new(),
        TimestampUnit::Milliseconds => format!(".{remainder:03}"),
        TimestampUnit::Microseconds => format!(".{remainder:06}"),
        TimestampUnit::Nanoseconds => format!(".{remainder:09}"),
    };
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{fraction}Z"
    ))
}

fn civil_from_days(days: i64) -> Option<(i64, i64, i64)> {
    let shifted = days.checked_add(719_468)?;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    Some((year, month, day))
}

fn write_padded_row(
    output: &mut impl Write,
    values: &[&str],
    widths: &[usize],
    right_aligned: &[bool],
) -> Result<()> {
    // Build the row, then drop end-of-line padding so rows never carry trailing whitespace.
    // Inter-column padding is preserved because it is never at the line end.
    let mut line = String::new();
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            line.push_str("  ");
        }
        if right_aligned[index] {
            write!(line, "{value:>width$}", width = widths[index]).unwrap();
        } else {
            write!(line, "{value:<width$}", width = widths[index]).unwrap();
        }
    }
    writeln!(output, "{}", line.trim_end())?;
    Ok(())
}

fn write_markdown_row(output: &mut impl Write, values: &[&str]) -> Result<()> {
    write!(output, "|")?;
    for value in values {
        write!(output, " {} |", value.replace('|', "\\|"))?;
    }
    writeln!(output)?;
    Ok(())
}

fn write_csv_row(output: &mut impl Write, values: &[&str]) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            write!(output, ",")?;
        }
        if value.contains([',', '"', '\r', '\n']) {
            write!(output, "\"{}\"", value.replace('"', "\"\""))?;
        } else {
            write!(output, "{value}")?;
        }
    }
    writeln!(output)?;
    Ok(())
}

fn write_json_line(output: &mut impl Write, schema: &Schema, values: &[FieldValue]) -> Result<()> {
    let mut line = String::new();
    line.push('{');
    for (index, (field, value)) in schema.fields.iter().zip(values).enumerate() {
        if index > 0 {
            line.push(',');
        }
        write_json_string(&mut line, &field.name);
        line.push(':');
        if fixed_point_null(field, value) {
            line.push_str("null");
            continue;
        }
        match value {
            FieldValue::Signed(value) => write!(line, "{value}").unwrap(),
            FieldValue::Unsigned(value) => write!(line, "{value}").unwrap(),
            FieldValue::Float32(value) if value.is_finite() => write!(line, "{value}").unwrap(),
            FieldValue::Float64(value) if value.is_finite() => write!(line, "{value}").unwrap(),
            FieldValue::Float32(value) => write_json_string(&mut line, &value.to_string()),
            FieldValue::Float64(value) => write_json_string(&mut line, &value.to_string()),
            FieldValue::Decimal(value) => write!(line, "{value}").unwrap(),
            FieldValue::String(value) => write_json_string(&mut line, value),
        }
    }
    line.push('}');
    writeln!(output, "{line}")?;
    Ok(())
}

fn write_json_string(output: &mut String, value: &str) {
    output.push('"');
    for ch in value.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            ch if ch <= '\u{1f}' => write!(output, "\\u{:04x}", ch as u32).unwrap(),
            ch => output.push(ch),
        }
    }
    output.push('"');
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").unwrap();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use fwob_core::{Field, FieldType, Schema};

    fn schema() -> Schema {
        Schema::new(
            "Row",
            vec![
                Field::new("key", FieldType::SignedInteger, 4, 0),
                Field::new("price", FieldType::UnsignedInteger, 4, 4),
                Field::new("symbol", FieldType::StringTableIndex, 1, 8),
                Field::new("name", FieldType::Utf8String, 4, 9),
            ],
            0,
        )
        .unwrap()
    }

    fn frame() -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(&1234i32.to_le_bytes());
        frame.extend_from_slice(&5_678_900u32.to_le_bytes());
        frame.push(1);
        frame.extend_from_slice(b"AB  ");
        frame
    }

    fn render(format: FrameFormat) -> String {
        let schema = schema();
        let strings = vec!["MSFT".to_string(), "AAPL".to_string()];
        let mut formatter = FrameFormatter::new(&schema, &strings, format);
        let mut output = Vec::new();
        formatter.write_frame(&mut output, &frame()).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn formats_raw_csv_json_and_hex_without_grouping() {
        assert_eq!(render(FrameFormat::Raw), "1234 5678900 AAPL AB\n");
        assert_eq!(
            render(FrameFormat::Csv),
            "key,price,symbol,name\n1234,5678900,AAPL,AB\n"
        );
        assert_eq!(
            render(FrameFormat::JsonLines),
            "{\"key\":1234,\"price\":5678900,\"symbol\":\"AAPL\",\"name\":\"AB\"}\n"
        );
        assert_eq!(render(FrameFormat::Hex), "d204000034a756000141422020\n");
    }

    #[test]
    fn unformatted_numbers_print_raw_and_markdown_aligns_by_type() {
        // No semantic => raw integers, no comma grouping.
        let table = render(FrameFormat::Table);
        assert!(table.contains("1234"));
        assert!(table.contains("5678900"));
        assert!(!table.contains("1,234"));
        // Numeric columns get right-aligned markers (---:), strings get left (---).
        assert_eq!(
            render(FrameFormat::Markdown),
            "| key | price | symbol | name |\n| ---: | ---: | --- | --- |\n| 1234 | 5678900 | AAPL | AB |\n"
        );
    }

    #[test]
    fn signed_fixed_point_min_is_null_but_unsigned_is_not() {
        let schema = Schema::new(
            "Calc",
            vec![
                Field::new("a", FieldType::SignedInteger, 4, 0)
                    .with_semantic(FieldSemantic::FixedPoint(8)),
                Field::new("b", FieldType::UnsignedInteger, 4, 4)
                    .with_semantic(FieldSemantic::FixedPoint(4)),
            ],
            0,
        )
        .unwrap();
        let mut frame = Vec::new();
        frame.extend_from_slice(&i32::MIN.to_le_bytes()); // a: null sentinel
        frame.extend_from_slice(&1_500_000u32.to_le_bytes()); // b: 150.0000 (unsigned never null)

        let render = |format| {
            let mut formatter = FrameFormatter::new(&schema, &[], format);
            let mut out = Vec::new();
            formatter.write_frame(&mut out, &frame).unwrap();
            String::from_utf8(out).unwrap()
        };

        let table = render(FrameFormat::Table);
        assert!(table.contains('-'), "{table}");
        assert!(table.contains("150.0000"), "{table}");
        assert!(
            !table.contains("2147"),
            "sentinel must not render as a number: {table}"
        );
        assert_eq!(
            render(FrameFormat::JsonLines),
            "{\"a\":null,\"b\":1500000}\n"
        );
        assert_eq!(render(FrameFormat::Csv), "a,b\n,1500000\n");
        // raw keeps the exact stored integer (no null concept).
        assert_eq!(render(FrameFormat::Raw), "-2147483648 1500000\n");
    }

    #[test]
    fn fixed_point_and_percentage_render_only_in_display_formats() {
        let schema = Schema::new(
            "Quote",
            vec![
                Field::new("price", FieldType::UnsignedInteger, 4, 0)
                    .with_semantic(FieldSemantic::FixedPoint(4)),
                Field::new("ret", FieldType::SignedInteger, 4, 4)
                    .with_semantic(FieldSemantic::Percentage(2)),
            ],
            0,
        )
        .unwrap();
        let mut frame = Vec::new();
        frame.extend_from_slice(&1_500_000u32.to_le_bytes()); // 150.0000
        frame.extend_from_slice(&3527i32.to_le_bytes()); // 35.27%

        let mut table = FrameFormatter::new(&schema, &[], FrameFormat::Table);
        let mut out = Vec::new();
        table.write_frame(&mut out, &frame).unwrap();
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("150.0000"), "{rendered}");
        assert!(rendered.contains("35.27%"), "{rendered}");

        // raw keeps the underlying integers.
        let mut raw = FrameFormatter::new(&schema, &[], FrameFormat::Raw);
        let mut raw_out = Vec::new();
        raw.write_frame(&mut raw_out, &frame).unwrap();
        assert_eq!(String::from_utf8(raw_out).unwrap(), "1500000 3527\n");
    }

    #[test]
    fn fixed_point_formatting_covers_grouping_zero_and_negative() {
        assert_eq!(format_fixed_point(1_500_000, 4, false), "150.0000");
        assert_eq!(format_fixed_point(1_111_111_111, 0, false), "1,111,111,111");
        assert_eq!(format_fixed_point(3527, 0, true), "3,527%");
        assert_eq!(format_fixed_point(3527, 4, true), "0.3527%");
        assert_eq!(format_fixed_point(-12_345, 2, false), "-123.45");
    }

    #[test]
    fn field_width_reflects_max_possible_value() {
        let u32_field =
            |semantic| Field::new("v", FieldType::UnsignedInteger, 4, 0).with_semantic(semantic);
        assert_eq!(field_width(&u32_field(FieldSemantic::None), &[]), 10);
        assert_eq!(
            field_width(&u32_field(FieldSemantic::FixedPoint(0)), &[]),
            13
        );
        assert_eq!(
            field_width(&u32_field(FieldSemantic::FixedPoint(1)), &[]),
            13
        );
        assert_eq!(
            field_width(&u32_field(FieldSemantic::FixedPoint(4)), &[]),
            12
        );
        assert_eq!(
            field_width(&u32_field(FieldSemantic::Percentage(4)), &[]),
            13
        );
        assert_eq!(
            field_width(
                &Field::new("t", FieldType::UnsignedInteger, 4, 0)
                    .with_semantic(FieldSemantic::UnixTimestamp(TimestampUnit::Seconds)),
                &[]
            ),
            20
        );
    }

    #[test]
    fn field_width_is_at_least_the_header_width() {
        // A header wider than the type's max value width must widen the column.
        let name = "a_very_long_header_name";
        let field = Field::new(name, FieldType::UnsignedInteger, 1, 0); // u8 value width is 3
        assert_eq!(field_width(&field, &[]), name.chars().count());
    }

    #[test]
    fn timestamp_semantics_are_human_readable_only_in_display_formats() {
        let schema = Schema::new(
            "Event",
            vec![Field::new("time", FieldType::SignedInteger, 8, 0)
                .with_semantic(FieldSemantic::UnixTimestamp(TimestampUnit::Milliseconds))],
            0,
        )
        .unwrap();
        let frame = 1_522_742_400_125i64.to_le_bytes();
        let mut table = FrameFormatter::new(&schema, &[], FrameFormat::Table);
        let mut table_output = Vec::new();
        table.write_frame(&mut table_output, &frame).unwrap();
        assert!(String::from_utf8(table_output)
            .unwrap()
            .contains("2018-04-03T08:00:00.125Z"));

        let mut raw = FrameFormatter::new(&schema, &[], FrameFormat::Raw);
        let mut raw_output = Vec::new();
        raw.write_frame(&mut raw_output, &frame).unwrap();
        assert_eq!(String::from_utf8(raw_output).unwrap(), "1522742400125\n");
    }
}
