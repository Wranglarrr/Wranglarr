// SPDX-License-Identifier: GPL-3.0-or-later
//! Command-line parsing for the subcommands (`wranglarr setup`, `auth`, `apps-auth`).
//!
//! Exit codes: 0 after `--help`, 2 for bad usage (scripts may rely on it), and
//! each command's own codes otherwise.

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};

/// Parse `args` (without the program name) as `prog` takes them. On `--help` or
/// a usage error, prints it and gives the code to exit with.
pub fn parse<T: Parser>(prog: &str, args: &[String]) -> Result<T, i32> {
    T::try_parse_from(std::iter::once(prog).chain(args.iter().map(String::as_str))).map_err(|e| {
        // help on stdout, errors on stderr: clap picks
        let _ = e.print();
        e.exit_code()
    })
}

/// A usage error found after parsing (a folder the arguments point at that
/// isn't right, say): printed with the usage line; the code to exit with (2).
pub fn usage_error<T: CommandFactory>(prog: &'static str, msg: impl std::fmt::Display) -> i32 {
    let e = T::command().name(prog).error(ErrorKind::ValueValidation, msg);
    let _ = e.print();
    e.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Args {
        #[arg(long)]
        port: Option<u16>,
    }

    fn s(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn exit_codes() {
        assert_eq!(parse::<Args>("x", &s(&["--port", "80"])).map(|a| a.port).ok(), Some(Some(80)));
        assert_eq!(parse::<Args>("x", &s(&["--port", "x"])).err(), Some(2));
        assert_eq!(parse::<Args>("x", &s(&["extra"])).err(), Some(2));
        assert_eq!(parse::<Args>("x", &s(&["--help"])).err(), Some(0));
        assert_eq!(usage_error::<Args>("x", "no such folder"), 2);
    }
}
