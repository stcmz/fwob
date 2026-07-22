use std::io::IsTerminal;
use std::{sync::mpsc, thread, time::Duration};

const LOG_RED: &str = "38;2;248;81;73";
const LOG_YELLOW: &str = "38;2;227;179;65";
#[allow(dead_code)]
const LOG_GREEN: &str = "38;2;63;185;80";
const LOG_DIM: &str = "38;2;139;148;158";

pub(super) use fwob::toml::{key_value, TomlWriter};

pub(super) fn color_enabled() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn stderr_color_enabled() -> bool {
    std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn colorize_stderr(value: &str, code: &str) -> String {
    if stderr_color_enabled() {
        format!("\x1b[{code}m{value}\x1b[0m")
    } else {
        value.to_string()
    }
}

/// Diagnostic/progress line (e.g. conversion progress). Goes to stderr so it never pollutes the
/// command's structured stdout output.
pub(super) fn log_info(message: impl AsRef<str>) {
    eprintln!("{}", colorize_stderr(message.as_ref(), LOG_DIM));
}

pub(super) fn log_warn(message: impl AsRef<str>) {
    eprintln!("{}", colorize_stderr(message.as_ref(), LOG_YELLOW));
}

/// Asks the user to confirm a destructive operation. `summary` lines describe the impact (one per
/// line) and are printed to stderr; `question` is the yes/no prompt. Returns `Ok(true)` to proceed.
///
/// With `assume_yes` the prompt is skipped. When stdin is not a terminal and `assume_yes` is unset,
/// this refuses (returns an error) rather than hanging on a read or silently proceeding, so scripted
/// use must opt in with `--yes`.
pub(super) fn confirm_destructive(
    summary: &[String],
    question: &str,
    assume_yes: bool,
) -> anyhow::Result<bool> {
    use std::io::{IsTerminal, Write};

    for line in summary {
        eprintln!("{}", colorize_stderr(line, LOG_YELLOW));
    }
    if assume_yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "refusing to proceed without confirmation because stdin is not a terminal; pass --yes to confirm"
        );
    }
    eprint!("{} [y/N] ", colorize_stderr(question, LOG_YELLOW));
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let answer = line.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

pub(super) struct ProgressTicker {
    stop: mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
}

impl ProgressTicker {
    pub(super) fn start(operation: &'static str) -> Self {
        let (stop, receiver) = mpsc::channel();
        let started = std::time::Instant::now();
        let worker = thread::spawn(move || loop {
            match receiver.recv_timeout(Duration::from_secs(5)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => log_info(format!(
                    "{operation} in progress: elapsed={:.1}s",
                    started.elapsed().as_secs_f64()
                )),
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for ProgressTicker {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[allow(dead_code)]
pub(super) fn log_success(message: impl AsRef<str>) {
    eprintln!("{}", colorize_stderr(message.as_ref(), LOG_GREEN));
}

/// Prints an error and its full cause chain to stderr, colorized in red.
pub(super) fn log_error(error: &anyhow::Error) {
    eprintln!("{}", colorize_stderr(&format!("error: {error}"), LOG_RED));
    for cause in error.chain().skip(1) {
        eprintln!(
            "{}",
            colorize_stderr(&format!("  caused by: {cause}"), LOG_RED)
        );
    }
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
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
