// SPDX-License-Identifier: GPL-3.0-or-later
//! `wranglarr auth` (`wranglarr-auth`): recover the login from a shell.
//!
//! `docker compose exec wranglarr wranglarr-auth <command>`. Bad usage exits
//! with 2, which scripts may rely on.

use std::io::{BufRead, Write};

use clap::{Parser, Subcommand};

use super::{Auth, password_problem};
use crate::config;
use crate::deployment::Deployment;

const PROG: &str = "wranglarr-auth";

/// Recover wranglarr's login from a shell.
#[derive(Parser, Debug)]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
enum Cmd {
    /// Show the account, two-factor and passkeys
    Status,
    /// Set a new password (asks for it)
    SetPassword,
    /// Turn off two-factor (TOTP) and delete the recovery codes
    #[command(name = "disable-2fa")]
    Disable2fa,
    /// Delete every passkey
    RemovePasskeys,
    /// End every session
    SignOutAll,
    /// Delete the account; the next visit sets it up again (new setup code in the log)
    Reset,
}

/// Echo off on a terminal for as long as this lives; the terminal's settings
/// come back when it drops, whatever happens in between (an error, a panic).
struct NoEcho<'a> {
    tty: &'a std::fs::File,
    old: rustix::termios::Termios,
}

impl<'a> NoEcho<'a> {
    fn new(tty: &'a std::fs::File) -> std::io::Result<NoEcho<'a>> {
        use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};
        let old = tcgetattr(tty)?;
        let mut quiet = old.clone();
        quiet.local_modes.remove(LocalModes::ECHO);
        tcsetattr(tty, OptionalActions::Flush, &quiet)?;
        Ok(NoEcho { tty, old })
    }
}

impl Drop for NoEcho<'_> {
    fn drop(&mut self) {
        use rustix::termios::{OptionalActions, tcsetattr};
        if let Err(e) = tcsetattr(self.tty, OptionalActions::Flush, &self.old) {
            eprintln!("{PROG}: couldn't turn the terminal's echo back on ({e}); run `stty echo`");
        }
    }
}

/// Read a line without echo from the terminal, or from stdin when there is no
/// terminal (with a warning that it may be shown).
fn read_password(prompt: &str) -> Option<String> {
    if let Ok(tty) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty")
        && let Ok(quiet) = NoEcho::new(&tty)
    {
        let mut w = &tty;
        let _ = write!(w, "{prompt}");
        let _ = w.flush();
        let mut line = String::new();
        let got = std::io::BufReader::new(&tty).read_line(&mut line);
        drop(quiet);
        let _ = writeln!(w);
        return match got {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
        };
    }
    eprintln!("Warning: Password input may be echoed.");
    eprint!("{prompt}");
    read_line()
}

fn read_line() -> Option<String> {
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
    }
}

/// The message on stderr; the exit code for a command that didn't do its job (1).
fn exit_with(msg: &str) -> i32 {
    eprintln!("{msg}");
    1
}

pub fn main(args: &[String]) -> i32 {
    let cmd = match crate::cli::parse::<Args>(PROG, args) {
        Ok(a) => a.cmd,
        Err(code) => return code,
    };
    let deployment = Deployment::read(config::process_env);
    let path = &deployment.config_path;
    let cfg = match crate::service::apps::load(path, config::process_env) {
        Ok(c) => c,
        Err(e) => return exit_with(&format!("{PROG}: {}: {e}", path.display())),
    };
    let au = match Auth::open(&cfg.state_db, deployment.auth) {
        Ok(a) => a,
        Err(e) => return exit_with(&format!("{PROG}: {e}")),
    };
    match run(&au, cmd) {
        Ok(code) => code,
        Err(e) => exit_with(&format!("{PROG}: {e}")),
    }
}

fn run(au: &Auth, cmd: Cmd) -> crate::error::Result<i32> {
    if cmd == Cmd::Status {
        let s = au.summary()?;
        let name = s["username"].as_str().filter(|u| !u.is_empty()).unwrap_or("(none)");
        println!(
            "account: {name}\ntwo-factor: {} ({} recovery codes left)\npasskeys: {}\nsessions: {}\nAPI keys: {}",
            if s["totp"] == true { "on" } else { "off" },
            s["recovery_left"],
            s["passkeys"].as_array().map_or(0, Vec::len),
            au.sessions("")?.len(),
            au.api_keys()?.len()
        );
        return Ok(0);
    }
    let Some(acct) = au.account()? else {
        return Ok(exit_with("no account yet: open wranglarr and create one"));
    };
    match cmd {
        Cmd::Status => {}
        Cmd::SetPassword => {
            let pw = read_password("new password: ").unwrap_or_default();
            if Some(pw.as_str()) != read_password("again: ").as_deref() {
                return Ok(exit_with("they don't match"));
            }
            if let Some(p) = password_problem(&pw, &acct.username) {
                return Ok(exit_with(&p));
            }
            let ended = au.set_password(&pw)?;
            println!("password changed; {ended} session(s) ended");
        }
        Cmd::Disable2fa => {
            au.force_totp_off()?;
            println!("two-factor is off");
        }
        Cmd::RemovePasskeys => println!("{} passkey(s) removed", au.remove_passkeys()?),
        Cmd::SignOutAll => println!("{} session(s) ended", au.end_other_sessions("")?),
        Cmd::Reset => {
            print!("delete the account {:?}? type yes: ", acct.username);
            let _ = std::io::stdout().flush();
            if read_line().unwrap_or_default().trim() != "yes" {
                return Ok(exit_with("nothing changed"));
            }
            au.reset()?;
            println!(
                "account deleted. Open wranglarr to create it again: the new setup code is in its log \
                 (docker compose logs wranglarr)"
            );
        }
    }
    Ok(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::auth::Options;

    fn s(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| x.to_string()).collect()
    }

    fn parse(args: &[&str]) -> Result<Cmd, i32> {
        crate::cli::parse::<Args>(PROG, &s(args)).map(|a| a.cmd)
    }

    #[test]
    fn commands_and_exit_codes() {
        assert_eq!(parse(&["status"]), Ok(Cmd::Status));
        assert_eq!(parse(&["disable-2fa"]), Ok(Cmd::Disable2fa));
        assert_eq!(parse(&["sign-out-all"]), Ok(Cmd::SignOutAll));
        assert_eq!(parse(&[]), Err(2));
        assert_eq!(parse(&["bogus"]), Err(2));
        assert_eq!(parse(&["status", "x"]), Err(2));
        assert_eq!(parse(&["-h"]), Err(0));
    }

    #[test]
    fn commands_on_a_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("wranglarr.db");
        let au = Auth::open(&db, Options::default()).unwrap();
        assert_eq!(run(&au, Cmd::Status).unwrap(), 0);
        assert_eq!(run(&au, Cmd::Disable2fa).unwrap(), 1, "no account yet");
        let code = au.ensure_setup_code().unwrap();
        au.create_account(&code, "admin", "correct horse battery").unwrap();
        au.new_session("password", "ip", "", false).unwrap();
        assert_eq!(run(&au, Cmd::SignOutAll).unwrap(), 0);
        assert!(au.sessions("").unwrap().is_empty());
        assert_eq!(run(&au, Cmd::RemovePasskeys).unwrap(), 0);
        assert_eq!(run(&au, Cmd::Disable2fa).unwrap(), 0);
    }

    #[test]
    fn reset_leaves_no_way_in() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("wranglarr.db");
        // the server, with an account, a key and a session ...
        let server = Auth::open(&db, Options::default()).unwrap();
        let code = server.ensure_setup_code().unwrap();
        server.create_account(&code, "admin", "correct horse battery").unwrap();
        let ok = server.reconfirm("ip", "correct horse battery").unwrap();
        let key = server.create_api_key(&ok, "ha").unwrap()["key"].as_str().unwrap().to_string();
        let (token, _) = server.new_session("password", "ip", "", false).unwrap();
        // ... and `wranglarr-auth reset` in another process
        let cli = Auth::open(&db, Options::default()).unwrap();
        cli.reset().unwrap();
        assert!(server.check_api_key(&key).unwrap().is_none(), "the key went with the account");
        assert_eq!(server.identify(&token, &key).unwrap(), crate::auth::Identity::default());
        assert!(cli.sessions("").unwrap().is_empty());
    }
}
