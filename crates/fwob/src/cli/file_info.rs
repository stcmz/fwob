use std::{
    collections::HashSet,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use fwob_core::FormatVersion;
use unicode_width::UnicodeWidthStr;

use super::{comma_u64, log_error, InfoArgs};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum InfoFormat {
    #[default]
    Table,
    Markdown,
    Csv,
    JsonLines,
}

impl InfoFormat {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "table" => Some(Self::Table),
            "md" => Some(Self::Markdown),
            "csv" => Some(Self::Csv),
            "jsonl" => Some(Self::JsonLines),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct FileInfo {
    display_path: PathBuf,
    format: FormatVersion,
    title: String,
    frame_type: String,
    key_field_index: usize,
    field_count: usize,
    frame_length: u32,
    frame_count: u64,
    first_key: Option<String>,
    last_key: Option<String>,
    data_bytes: u64,
    physical_ratio: Option<f64>,
}

pub(super) fn print_file_info(args: InfoArgs) -> Result<()> {
    let (paths, format) = parse_targets(&args.target)?;
    let files = discover_files(&paths)?;
    let current_dir = fs::canonicalize(std::env::current_dir()?)?;
    let mut rows = Vec::with_capacity(files.len());
    for path in files {
        match read_file_info(path, &current_dir, args.key_field_index) {
            Ok(info) => rows.push(info),
            Err(error) => log_error(&error),
        }
    }

    let stdout = io::stdout();
    let mut output = stdout.lock();
    match format {
        InfoFormat::Table => write_table(&mut output, &rows),
        InfoFormat::Markdown => write_markdown(&mut output, &rows),
        InfoFormat::Csv => write_csv(&mut output, &rows),
        InfoFormat::JsonLines => write_json_lines(&mut output, &rows),
    }
}

fn parse_targets(values: &[String]) -> Result<(Vec<PathBuf>, InfoFormat)> {
    let mut format = None;
    let mut paths = Vec::new();
    for value in values {
        if let Some(parsed) = InfoFormat::parse(value) {
            if format.replace(parsed).is_some() {
                bail!("info accepts at most one output format token");
            }
        } else {
            paths.push(PathBuf::from(value));
        }
    }
    if paths.is_empty() {
        paths.push(PathBuf::from("."));
    }
    Ok((paths, format.unwrap_or_default()))
}

fn discover_files(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for input in inputs {
        let metadata = fs::metadata(input)
            .with_context(|| format!("failed to inspect {}", input.display()))?;
        if metadata.is_file() {
            push_unique(&mut files, &mut seen, input.clone())?;
        } else if metadata.is_dir() {
            for entry in fs::read_dir(input)
                .with_context(|| format!("failed to read directory {}", input.display()))?
            {
                let entry = entry?;
                let path = entry.path();
                if entry.file_type()?.is_file() && has_fwob_extension(&path) {
                    push_unique(&mut files, &mut seen, path)?;
                }
            }
        } else {
            bail!("{} is not a regular file or directory", input.display());
        }
    }
    files.sort_by_cached_key(|path| path.to_string_lossy().to_lowercase());
    Ok(files)
}

fn push_unique(files: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) -> Result<()> {
    let identity =
        fs::canonicalize(&path).with_context(|| format!("failed to resolve {}", path.display()))?;
    if seen.insert(identity) {
        files.push(path);
    }
    Ok(())
}

fn has_fwob_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fwob"))
}

fn read_file_info(
    path: PathBuf,
    current_dir: &Path,
    v1_key_field_index: usize,
) -> Result<FileInfo> {
    let mut reader =
        fwob::Reader::open_with_options(&path, fwob::ReaderOptions { v1_key_field_index })
            .with_context(|| format!("failed to read FWOB metadata from {}", path.display()))?;
    let schema = reader.schema().clone();
    let frame_count = reader.frame_count();
    let data_bytes = frame_count
        .checked_mul(u64::from(schema.frame_len))
        .context("raw data byte count overflows u64")?;
    let physical_bytes = fs::metadata(&path)?.len();
    let physical_ratio = (data_bytes != 0).then(|| physical_bytes as f64 / data_bytes as f64);
    let absolute_path = fs::canonicalize(&path)?;
    let display_path = pathdiff::diff_paths(&absolute_path, current_dir).unwrap_or(absolute_path);
    Ok(FileInfo {
        display_path,
        format: reader.format_version(),
        title: reader.title().to_owned(),
        frame_type: schema.frame_type,
        key_field_index: schema.key_field_index,
        field_count: schema.fields.len(),
        frame_length: schema.frame_len,
        frame_count,
        first_key: reader.first_key()?.map(|key| key.to_string()),
        last_key: reader.last_key()?.map(|key| key.to_string()),
        data_bytes,
        physical_ratio,
    })
}

const HEADERS: [&str; 12] = [
    "file",
    "format",
    "title",
    "frame_type",
    "key_field_index",
    "field_count",
    "frame_length",
    "frame_count",
    "first_key",
    "last_key",
    "data_bytes",
    "physical_ratio",
];

const RIGHT_ALIGNED: [bool; 12] = [
    false, false, false, false, true, true, true, true, true, true, true, true,
];

fn display_cells(info: &FileInfo, grouped: bool) -> [String; 12] {
    let integer = |value: u64| {
        if grouped {
            comma_u64(value)
        } else {
            value.to_string()
        }
    };
    let usize_value = |value: usize| integer(value as u64);
    [
        display_text(&info.display_path.display().to_string()),
        format_name(info.format).to_owned(),
        display_text(&info.title),
        display_text(&info.frame_type),
        usize_value(info.key_field_index),
        usize_value(info.field_count),
        integer(u64::from(info.frame_length)),
        integer(info.frame_count),
        info.first_key.clone().unwrap_or_else(|| "-".to_owned()),
        info.last_key.clone().unwrap_or_else(|| "-".to_owned()),
        integer(info.data_bytes),
        info.physical_ratio
            .map(|ratio| format!("{ratio:.4}"))
            .unwrap_or_else(|| "-".to_owned()),
    ]
}

fn write_table(output: &mut impl Write, rows: &[FileInfo]) -> Result<()> {
    let cells: Vec<_> = rows.iter().map(|row| display_cells(row, true)).collect();
    let mut widths = HEADERS.map(UnicodeWidthStr::width);
    for row in &cells {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(UnicodeWidthStr::width(value.as_str()));
        }
    }
    write_padded_row(output, &HEADERS.map(str::to_owned), &widths)?;
    for row in &cells {
        write_padded_row(output, row, &widths)?;
    }
    Ok(())
}

fn write_padded_row(
    output: &mut impl Write,
    row: &[String; 12],
    widths: &[usize; 12],
) -> Result<()> {
    for (index, value) in row.iter().enumerate() {
        if index > 0 {
            write!(output, "  ")?;
        }
        let padding = widths[index].saturating_sub(UnicodeWidthStr::width(value.as_str()));
        if RIGHT_ALIGNED[index] {
            write!(output, "{}{}", " ".repeat(padding), value)?;
        } else {
            write!(output, "{}{}", value, " ".repeat(padding))?;
        }
    }
    writeln!(output)?;
    Ok(())
}

fn write_markdown(output: &mut impl Write, rows: &[FileInfo]) -> Result<()> {
    writeln!(output, "| {} |", HEADERS.join(" | "))?;
    let separators: Vec<_> = RIGHT_ALIGNED
        .iter()
        .map(|right| if *right { "---:" } else { "---" })
        .collect();
    writeln!(output, "| {} |", separators.join(" | "))?;
    for row in rows {
        let cells = display_cells(row, true).map(|value| value.replace('|', "\\|"));
        writeln!(output, "| {} |", cells.join(" | "))?;
    }
    Ok(())
}

fn write_csv(output: &mut impl Write, rows: &[FileInfo]) -> Result<()> {
    writeln!(output, "{}", HEADERS.join(","))?;
    for row in rows {
        let cells = display_cells(row, false).map(|value| csv_field(&value));
        writeln!(output, "{}", cells.join(","))?;
    }
    Ok(())
}

fn write_json_lines(output: &mut impl Write, rows: &[FileInfo]) -> Result<()> {
    for row in rows {
        let value = serde_json::json!({
            "file": row.display_path.display().to_string(),
            "format": format_name(row.format),
            "title": row.title,
            "frame_type": row.frame_type,
            "key_field_index": row.key_field_index,
            "field_count": row.field_count,
            "frame_length": row.frame_length,
            "frame_count": row.frame_count,
            "first_key": row.first_key,
            "last_key": row.last_key,
            "data_bytes": row.data_bytes,
            "physical_ratio": row.physical_ratio,
        });
        serde_json::to_writer(&mut *output, &value)?;
        writeln!(output)?;
    }
    Ok(())
}

fn format_name(format: FormatVersion) -> &'static str {
    match format {
        FormatVersion::V1 => "fwob-v1",
        FormatVersion::V2 => "fwob-v2",
    }
}

fn display_text(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\r' | '\n' | '\t' => ' ',
            other => other,
        })
        .collect()
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_and_explicit_formats() {
        let (paths, format) = parse_targets(&[]).unwrap();
        assert_eq!(paths, [PathBuf::from(".")]);
        assert_eq!(format, InfoFormat::Table);

        let (paths, format) = parse_targets(&["data".into(), "csv".into()]).unwrap();
        assert_eq!(paths, [PathBuf::from("data")]);
        assert_eq!(format, InfoFormat::Csv);
    }

    #[test]
    fn csv_quotes_structural_characters() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
    }
}
