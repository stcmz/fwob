use std::{fmt::Write as _, io::Write};

use fwob_core::{decode_decimal, Field, FieldType, Schema};

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
    header_written: bool,
}

impl<'a> FrameFormatter<'a> {
    pub fn new(schema: &'a Schema, string_table: &'a [String], format: FrameFormat) -> Self {
        let widths = schema
            .fields
            .iter()
            .map(|field| field_width(field, string_table))
            .collect();
        Self {
            schema,
            decoder: FrameDecoder::new(schema, string_table),
            format,
            widths,
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
                write_padded_row(output, &values, &self.widths, false)?;
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
                let separators = self.schema.fields.iter().map(|_| "---").collect::<Vec<_>>();
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
                let rendered = values.iter().map(render_grouped).collect::<Vec<_>>();
                let borrowed = rendered.iter().map(String::as_str).collect::<Vec<_>>();
                write_padded_row(output, &borrowed, &self.widths, true)?;
            }
            FrameFormat::Markdown => {
                let rendered = values.iter().map(render_grouped).collect::<Vec<_>>();
                let borrowed = rendered.iter().map(String::as_str).collect::<Vec<_>>();
                write_markdown_row(output, &borrowed)?;
            }
            FrameFormat::Csv => {
                let rendered = values.iter().map(render_plain).collect::<Vec<_>>();
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

fn render_grouped(value: &FieldValue) -> String {
    match value {
        FieldValue::Signed(value) => group_number(&value.to_string()),
        FieldValue::Unsigned(value) => group_number(&value.to_string()),
        FieldValue::Float32(value) => group_number(&value.to_string()),
        FieldValue::Float64(value) => group_number(&value.to_string()),
        FieldValue::Decimal(value) => group_number(&value.to_string()),
        FieldValue::String(value) => value.clone(),
    }
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
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    grouped.push_str(&value[decimal..]);
    grouped
}

fn field_width(field: &Field, string_table: &[String]) -> usize {
    let value_width = match field.field_type {
        FieldType::SignedInteger => match field.length {
            1 => 4,
            2 => 7,
            4 => 14,
            8 => 26,
            _ => field.length as usize * 2,
        },
        FieldType::UnsignedInteger => match field.length {
            1 => 3,
            2 => 6,
            4 => 13,
            8 => 26,
            _ => field.length as usize * 2,
        },
        FieldType::FloatingPoint => match field.length {
            4 => 16,
            8 => 24,
            16 => 34,
            _ => field.length as usize * 2,
        },
        FieldType::Utf8String => field.length as usize,
        FieldType::StringTableIndex => string_table
            .iter()
            .map(|value| value.chars().count())
            .max()
            .unwrap_or(1),
    };
    value_width.max(field.name.chars().count())
}

fn write_padded_row(
    output: &mut impl Write,
    values: &[&str],
    widths: &[usize],
    right_align: bool,
) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            write!(output, "  ")?;
        }
        if right_align {
            write!(output, "{value:>width$}", width = widths[index])?;
        } else {
            write!(output, "{value:<width$}", width = widths[index])?;
        }
    }
    writeln!(output)?;
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
    fn formats_grouped_console_and_markdown_output() {
        let table = render(FrameFormat::Table);
        assert!(table.contains("1,234"));
        assert!(table.contains("5,678,900"));
        assert_eq!(
            render(FrameFormat::Markdown),
            "| key | price | symbol | name |\n| --- | --- | --- | --- |\n| 1,234 | 5,678,900 | AAPL | AB |\n"
        );
    }
}
