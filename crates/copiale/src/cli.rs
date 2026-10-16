//! Command-line arguments.

use std::ffi::OsString;

pub const USAGE: &str = "\
Usage: copiale [--toggle | --quit]

Starts Copiale, or shows the window if it's already running.

  --toggle  Show the window and focus the composer, or hide it if focused
  --quit    Quit the running instance
  --help    Show this help
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cli {
    Start,
    Toggle,
    Quit,
    Help,
}

/// Parses the arguments after the program name; `Err` names the first
/// one not understood.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Cli, String> {
    let args: Vec<_> = args.into_iter().collect();
    match args.as_slice() {
        [] => Ok(Cli::Start),
        [arg] => match arg.to_str() {
            Some("--toggle") => Ok(Cli::Toggle),
            Some("--quit") => Ok(Cli::Quit),
            Some("--help" | "-h") => Ok(Cli::Help),
            _ => Err(arg.to_string_lossy().into_owned()),
        },
        [_, extra, ..] => Err(extra.to_string_lossy().into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Cli, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_one_flag_at_most() {
        assert_eq!(parse_strs(&[]), Ok(Cli::Start));
        assert_eq!(parse_strs(&["--toggle"]), Ok(Cli::Toggle));
        assert_eq!(parse_strs(&["--quit"]), Ok(Cli::Quit));
        assert_eq!(parse_strs(&["-h"]), Ok(Cli::Help));
        assert_eq!(parse_strs(&["--show"]), Err("--show".into()));
        assert_eq!(parse_strs(&["--quit", "--toggle"]), Err("--toggle".into()));
    }
}
