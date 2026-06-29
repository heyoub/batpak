//! CLI output macros for the xtask developer tool.
//!
//! `xtask` is the repository-owned command-line tool; its subcommands write
//! human and CI status messages to stdout/stderr. Rather than lean on the
//! `clippy::print_stdout` / `clippy::print_stderr` lints being globally
//! silenced, every call site goes through these macros, which are nothing more
//! than ergonomic `writeln!`/`write!` to a freshly locked standard handle. The
//! denied `print*!` macros are therefore never used anywhere in the crate.
//!
//! Locking per call (rather than holding a lock across calls) is intentional
//! for a CLI tool: it is the simplest correct option and avoids any chance of a
//! held-lock deadlock through nested helper calls. The write result is
//! deliberately ignored — a closed stdout/stderr pipe is not actionable for a
//! best-effort status line.

/// Write a line to stdout (drop-in for `println!`).
macro_rules! outln {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stdout().lock(), $($arg)*);
    }};
}

/// Write to stdout without a trailing newline (drop-in for `print!`).
macro_rules! out {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::write!(::std::io::stdout().lock(), $($arg)*);
    }};
}

/// Write a line to stderr (drop-in for `eprintln!`).
macro_rules! errln {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stderr().lock(), $($arg)*);
    }};
}
