//! Standard output shared by both binaries. A reader that closes the pipe
//! early (`papertiger list | head`) ends the output; that is not a failure
//! of the command, and a mutation it follows has already committed.

use std::fmt;
use std::io::{self, Write};

/// Output stopped because the reader closed standard output.
#[derive(Debug)]
pub struct OutputClosed;

impl fmt::Display for OutputClosed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("standard output was closed by its reader")
    }
}

impl std::error::Error for OutputClosed {}

fn classify(error: io::Error) -> anyhow::Error {
    if error.kind() == io::ErrorKind::BrokenPipe {
        OutputClosed.into()
    } else {
        anyhow::Error::new(error).context("write standard output")
    }
}

/// Write formatted text to standard output.
pub fn print_stdout(args: fmt::Arguments<'_>) -> anyhow::Result<()> {
    io::stdout().lock().write_fmt(args).map_err(classify)
}

/// Write raw bytes to standard output.
pub fn write_stdout(bytes: &[u8]) -> anyhow::Result<()> {
    io::stdout().lock().write_all(bytes).map_err(classify)
}

/// Whether `error` only reports that the reader closed standard output.
pub fn is_output_closed(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.is::<OutputClosed>())
}

/// `println!` for commands: returns the write error from the enclosing
/// function instead of panicking when standard output is closed.
#[macro_export]
macro_rules! outln {
    () => {
        $crate::print_stdout(format_args!("\n"))?
    };
    ($($arg:tt)*) => {
        $crate::print_stdout(format_args!("{}\n", format_args!($($arg)*)))?
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_pipe_is_output_closed_and_other_errors_are_not() {
        let closed = classify(io::Error::from(io::ErrorKind::BrokenPipe));
        assert!(is_output_closed(&closed));
        let full = classify(io::Error::from(io::ErrorKind::StorageFull));
        assert!(!is_output_closed(&full));
        assert_eq!(full.to_string(), "write standard output");
    }
}
