//! Command-line arguments.

use std::ffi::OsString;
use std::path::PathBuf;

pub const USAGE: &str = "\
Usage: copiale [--toggle | --quit | --check-config [FILE]]

Starts Copiale, or shows the window if it's already running.

  --toggle               Show the window and focus the composer, or hide it
                         if focused
  --quit                 Quit the running instance
  --check-config [FILE]  List problems in FILE, or in the config in effect;
                         fails if there are any
  --help                 Show this help
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cli {
    Start,
    Toggle,
    Quit,
    /// `None`: the platform config, over its defaults.
    CheckConfig(Option<PathBuf>),
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
            Some("--check-config") => Ok(Cli::CheckConfig(None)),
            Some("--help" | "-h") => Ok(Cli::Help),
            _ => Err(arg.to_string_lossy().into_owned()),
        },
        [flag, file] if flag == "--check-config" => Ok(Cli::CheckConfig(Some(file.into()))),
        [flag, _, extra, ..] if flag == "--check-config" => {
            Err(extra.to_string_lossy().into_owned())
        }
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
        assert_eq!(parse_strs(&["--check-config"]), Ok(Cli::CheckConfig(None)));
        assert_eq!(
            parse_strs(&["--check-config", "my.toml"]),
            Ok(Cli::CheckConfig(Some("my.toml".into())))
        );
        assert_eq!(
            parse_strs(&["--check-config", "a.toml", "b.toml"]),
            Err("b.toml".into())
        );
    }
}
