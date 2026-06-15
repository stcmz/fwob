use std::io::IsTerminal;

use fwob_core::Key;

const GITHUB_BLUE: &str = "38;2;121;192;255";
const GITHUB_GREEN: &str = "38;2;165;214;255";
const GITHUB_ORANGE: &str = "38;2;255;166;87";
const GITHUB_PURPLE: &str = "38;2;210;168;255";

fn color_enabled() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn colorize(value: impl AsRef<str>, code: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{}\x1b[0m", value.as_ref())
    } else {
        value.as_ref().to_string()
    }
}

pub(super) fn toml_section(name: &str) {
    println!("{}", colorize(format!("[{name}]"), GITHUB_PURPLE));
}

pub(super) fn toml_array_section(name: &str) {
    println!("{}", colorize(format!("[[{name}]]"), GITHUB_PURPLE));
}

fn toml_key(key: &str) -> String {
    colorize(key, GITHUB_BLUE)
}

fn toml_string(value: &str) -> String {
    colorize(format!("\"{}\"", escape_toml_string(value)), GITHUB_GREEN)
}

fn toml_value(value: impl ToString) -> String {
    colorize(value.to_string(), GITHUB_ORANGE)
}

pub(super) fn toml_kv_str(key: &str, value: &str) {
    println!("{} = {}", toml_key(key), toml_string(value));
}

pub(super) fn toml_kv_num(key: &str, value: impl ToString) {
    println!("{} = {}", toml_key(key), toml_value(value));
}

pub(super) fn toml_kv_bool(key: &str, value: bool) {
    println!("{} = {}", toml_key(key), toml_value(value));
}

pub(super) fn toml_kv_key(key: &str, value: Key) {
    println!("{} = {}", toml_key(key), toml_value(toml_key_value(value)));
}

pub(super) fn toml_key_value(key: Key) -> String {
    match key {
        Key::I8(value) => value.to_string(),
        Key::I16(value) => value.to_string(),
        Key::I32(value) => value.to_string(),
        Key::I64(value) => value.to_string(),
        Key::U8(value) => value.to_string(),
        Key::U16(value) => value.to_string(),
        Key::U32(value) => value.to_string(),
        Key::U64(value) => value.to_string(),
        Key::F32(value) => value.to_string(),
        Key::F64(value) => value.to_string(),
        Key::Decimal(value) => value.to_string(),
    }
}

pub(super) fn toml_kv_multiline(key: &str, value: &str) {
    println!("{} = \"\"\"", toml_key(key));
    print!("{}", escape_toml_multiline(value));
    if !value.ends_with('\n') {
        println!();
    }
    println!("\"\"\"");
}

pub(super) fn toml_kv_float_array(key: &str, values: &[f64], precision: usize) {
    let values = values
        .iter()
        .map(|value| format!("{value:.precision$}", precision = precision))
        .collect::<Vec<_>>()
        .join(", ");
    println!("{} = {}", toml_key(key), toml_value(format!("[{values}]")));
}

fn escape_toml_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn escape_toml_multiline(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace("\"\"\"", "\\\"\\\"\\\"")
}

pub(super) fn print_aligned_table(headers: &[&str], rows: Vec<Vec<String>>, right_align: &[bool]) {
    debug_assert_eq!(headers.len(), right_align.len());
    debug_assert!(rows.iter().all(|row| row.len() == headers.len()));

    let mut widths: Vec<usize> = headers
        .iter()
        .map(|header| header.chars().count())
        .collect();
    for row in &rows {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.chars().count());
        }
    }

    for (index, header) in headers.iter().enumerate() {
        if index > 0 {
            print!("  ");
        }
        print!("{header:<width$}", width = widths[index]);
    }
    println!();

    for row in rows {
        for (index, value) in row.iter().enumerate() {
            if index > 0 {
                print!("  ");
            }
            if right_align[index] {
                print!("{value:>width$}", width = widths[index]);
            } else {
                print!("{value:<width$}", width = widths[index]);
            }
        }
        println!();
    }
}

pub(super) fn comma_u32(value: u32) -> String {
    comma_u64(u64::from(value))
}

pub(super) fn comma_usize(value: usize) -> String {
    comma_u64(value as u64)
}

pub(super) fn comma_i128(value: i128) -> String {
    if value < 0 {
        format!("-{}", comma_u128(value.unsigned_abs()))
    } else {
        comma_u128(value as u128)
    }
}

pub(super) fn comma_u64(value: u64) -> String {
    comma_u128(u128::from(value))
}

pub(super) fn comma_f64(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return value.to_string();
    }

    let rendered = format!("{:.*}", decimals, value);
    let (sign, body) = if let Some(unsigned) = rendered.strip_prefix('-') {
        ("-", unsigned)
    } else {
        ("", rendered.as_str())
    };
    let (integer, fraction) = body.split_once('.').unwrap_or((body, ""));
    let formatted_integer = integer
        .parse::<u128>()
        .map(comma_u128)
        .unwrap_or_else(|_| integer.to_string());
    if fraction.is_empty() {
        format!("{sign}{formatted_integer}")
    } else {
        format!("{sign}{formatted_integer}.{fraction}")
    }
}

pub(super) fn comma_u128(value: u128) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
