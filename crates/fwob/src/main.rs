mod cli;

fn main() {
    if let Err(error) = cli::run() {
        // Colorized (TTY-gated) error + cause chain on stderr, then a non-zero exit. Replaces
        // anyhow's plain `Error: …` Termination output.
        cli::print_error(&error);
        std::process::exit(1);
    }
}
