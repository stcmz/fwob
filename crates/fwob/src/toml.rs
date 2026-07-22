//! Colored-TOML output.
//!
//! A small writer that renders structured, human-readable key/value output (schema dumps, run
//! summaries, …) as valid TOML, optionally painted with the GitHub-style truecolor palette used by
//! `fwob inspect`. It is deliberately generic — no domain concepts — so other tools built on FWOB
//! (e.g. `mdfwob`) can render their own summaries in the same style instead of reimplementing it.
//!
//! ```
//! use fwob::toml::TomlWriter;
//!
//! let mut buf = Vec::new();
//! let mut w = TomlWriter::new(&mut buf, false); // color off (e.g. not a terminal)
//! w.section("summary").unwrap();
//! w.kv_num("bars", 1552).unwrap();
//! w.kv_float("mean", 0.000479, 6).unwrap();
//! assert_eq!(String::from_utf8(buf).unwrap(), "[summary]\nbars = 1552\nmean = 0.000479\n");
//! ```

use std::fmt::Display;
use std::io::{self, Write};

use fwob_core::Key;

// GitHub-style truecolor palette (matches `fwob inspect`).
const SECTION: &str = "38;2;210;168;255"; // purple
const KEY: &str = "38;2;121;192;255"; // blue
const STRING: &str = "38;2;165;214;255"; // green
const VALUE: &str = "38;2;255;166;87"; // orange

fn paint(color: bool, code: &str, text: &str) -> String {
    if color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

mod sealed {
    pub trait Sealed {}
}

/// The integer types [`TomlWriter::kv_num`] accepts.
///
/// Sealed: implemented for every primitive integer and not implementable downstream. The bound
/// exists so a pre-formatted `String` cannot be written as an unquoted TOML value.
pub trait Integer: sealed::Sealed + Display {}

macro_rules! impl_integer {
    ($($ty:ty),* $(,)?) => {$(
        impl sealed::Sealed for $ty {}
        impl Integer for $ty {}
    )*};
}

impl_integer!(u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize);

/// The TOML literal for a [`Key`] (an unquoted numeric/decimal value).
pub fn key_value(key: Key) -> String {
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

fn escape_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn escape_multiline(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace("\"\"\"", "\\\"\\\"\\\"")
}

/// A colored-TOML writer over any [`Write`] sink.
///
/// `color` selects whether ANSI truecolor escapes are emitted; callers typically pass
/// `stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()`. The bytes written are always
/// valid TOML (the color escapes wrap whole tokens), so redirected output stays parseable.
pub struct TomlWriter<W> {
    out: W,
    color: bool,
}

impl<W: Write> TomlWriter<W> {
    pub fn new(out: W, color: bool) -> Self {
        Self { out, color }
    }

    /// Consumes the writer, returning the underlying sink.
    pub fn into_inner(self) -> W {
        self.out
    }

    /// A `[name]` table header.
    pub fn section(&mut self, name: &str) -> io::Result<()> {
        writeln!(
            self.out,
            "{}",
            paint(self.color, SECTION, &format!("[{name}]"))
        )
    }

    /// A `[[name]]` array-of-tables header.
    pub fn array_section(&mut self, name: &str) -> io::Result<()> {
        writeln!(
            self.out,
            "{}",
            paint(self.color, SECTION, &format!("[[{name}]]"))
        )
    }

    /// A blank separating line.
    pub fn blank(&mut self) -> io::Result<()> {
        writeln!(self.out)
    }

    /// `key = "value"` (a quoted, escaped string).
    pub fn kv_str(&mut self, key: &str, value: &str) -> io::Result<()> {
        writeln!(
            self.out,
            "{} = {}",
            paint(self.color, KEY, key),
            paint(self.color, STRING, &format!("\"{}\"", escape_string(value)))
        )
    }

    /// `key = value` for any integer.
    ///
    /// Restricted to [`Integer`] on purpose: the value is written *unquoted*, so accepting an
    /// arbitrary `Display` type let pre-formatted strings (e.g. comma-grouped `43,329,300`) through
    /// and produce unparseable TOML. Use [`kv_str`](Self::kv_str) for text.
    pub fn kv_num(&mut self, key: &str, value: impl Integer) -> io::Result<()> {
        self.kv_raw(key, &value.to_string())
    }

    /// `key = value` for a boolean.
    pub fn kv_bool(&mut self, key: &str, value: bool) -> io::Result<()> {
        self.kv_raw(key, &value.to_string())
    }

    /// `key = value` for a [`Key`].
    pub fn kv_key(&mut self, key: &str, value: Key) -> io::Result<()> {
        self.kv_raw(key, &key_value(value))
    }

    /// `key = value` for a float at fixed `precision`.
    pub fn kv_float(&mut self, key: &str, value: f64, precision: usize) -> io::Result<()> {
        self.kv_raw(key, &format!("{value:.precision$}"))
    }

    /// `key = [v0, v1, …]` for a float array at fixed `precision`.
    pub fn kv_float_array(
        &mut self,
        key: &str,
        values: &[f64],
        precision: usize,
    ) -> io::Result<()> {
        let joined = values
            .iter()
            .map(|value| format!("{value:.precision$}"))
            .collect::<Vec<_>>()
            .join(", ");
        self.kv_raw(key, &format!("[{joined}]"))
    }

    /// `key = """…"""` (a multi-line basic string). Only the key is colored; the body is emitted
    /// verbatim (escaped) so large previews stay readable.
    pub fn kv_multiline(&mut self, key: &str, value: &str) -> io::Result<()> {
        writeln!(self.out, "{} = \"\"\"", paint(self.color, KEY, key))?;
        write!(self.out, "{}", escape_multiline(value))?;
        if !value.ends_with('\n') {
            writeln!(self.out)?;
        }
        writeln!(self.out, "\"\"\"")
    }

    /// `key = value` where `value` is a pre-rendered, unquoted (non-string) literal.
    fn kv_raw(&mut self, key: &str, value: &str) -> io::Result<()> {
        writeln!(
            self.out,
            "{} = {}",
            paint(self.color, KEY, key),
            paint(self.color, VALUE, value)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(color: bool, f: impl FnOnce(&mut TomlWriter<&mut Vec<u8>>)) -> String {
        let mut buf = Vec::new();
        let mut w = TomlWriter::new(&mut buf, color);
        f(&mut w);
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn plain_output_is_valid_toml() {
        let out = render(false, |w| {
            w.section("summary").unwrap();
            w.kv_num("bars", 1552).unwrap();
            w.kv_str("method", "log").unwrap();
            w.kv_bool("annualized", true).unwrap();
            w.kv_float("mean", 0.000479, 6).unwrap();
            w.blank().unwrap();
            w.array_section("field").unwrap();
            w.kv_float_array("weights", &[0.5, 0.25], 2).unwrap();
        });
        let expected = "[summary]\nbars = 1552\nmethod = \"log\"\nannualized = true\n\
             mean = 0.000479\n\n[[field]]\nweights = [0.50, 0.25]\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn color_wraps_whole_tokens() {
        let out = render(true, |w| w.kv_num("n", 3).unwrap());
        assert_eq!(
            out,
            "\x1b[38;2;121;192;255mn\x1b[0m = \x1b[38;2;255;166;87m3\x1b[0m\n"
        );
    }

    #[test]
    fn strings_are_escaped() {
        let out = render(false, |w| w.kv_str("path", "a\"b\\c").unwrap());
        assert_eq!(out, "path = \"a\\\"b\\\\c\"\n");
    }
}
