// SPDX-License-Identifier: GPL-3.0-or-later
//! What a failure looks like: the image without its tag, the log lines worth
//! reading, and the fingerprint made from them.

use std::sync::LazyLock;

use regex::Regex;
use sha2::{Digest, Sha256};

use super::LOG_LINES;
use crate::value::head;

/// `lscr.io/linuxserver/radarr:latest` -> `lscr.io/linuxserver/radarr`
pub(super) fn repo_of(reference: &str) -> String {
    let r = reference.split('@').next().unwrap_or(reference);
    match r.rfind(':') {
        Some(i) if !r[i..].contains('/') => r[..i].to_owned(),
        _ => r.to_owned(),
    }
}

pub(super) static ERROR_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(error|fatal|exception|panic|panicked|traceback|critical|failed|failure|unhandled)\b")
        .expect("static regex")
});
/// A stack frame or the continuation of one.
static FRAME_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s+(at |File "|\.\.\.|---|Caused by|[0-9]+: )"#).expect("static regex"));

/// The lines worth reading: errors and their stack frames, at most
/// [`LOG_LINES`]; the last lines when nothing looks like an error.
pub(super) fn pick_lines(log: &str) -> Vec<String> {
    let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut keep: Vec<String> = Vec::new();
    let mut in_error = false;
    for l in &lines {
        if ERROR_LINE.is_match(l) {
            in_error = true;
        } else if !(in_error && FRAME_LINE.is_match(l)) {
            in_error = false;
            continue;
        }
        keep.push(head(l.trim_end(), 500));
    }
    if keep.is_empty() {
        return lines[lines.len().saturating_sub(20)..].iter().map(|l| head(l.trim_end(), 500)).collect();
    }
    // the last ones: the first errors are often noise from before the crash
    keep.split_off(keep.len().saturating_sub(LOG_LINES))
}

static NUMBERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|\b0x[0-9a-f]+|\b[0-9a-f]{8,}\b|[0-9]+",
    )
    .expect("static regex")
});
static SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("static regex"));

/// The parts of a line that stay the same from one machine to the next:
/// numbers, ids and timestamps go.
fn normalize(s: &str) -> String {
    let s = NUMBERS.replace_all(s, "#");
    SPACE.replace_all(s.trim(), " ").to_lowercase()
}

/// `wf-` and ten hex digits of what this failure looks like.
pub(super) fn fingerprint(kind: &str, image: &str, new_version: &str, error: &str, first_error: &str) -> String {
    // the first error line without its timestamp and log level prefix
    let first = first_error.rsplit('|').next().unwrap_or(first_error);
    let src = format!("wf1|{kind}|{image}|{new_version}|{}|{}", normalize(error), normalize(first));
    let h = Sha256::digest(src.as_bytes());
    format!("wf-{}", head(&hex::encode(h), 10))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_keeps_errors_and_their_stacks() {
        let log = "info: starting\n[Error] boom\n   at A.B()\n   at C.D()\ninfo: more\nwarn: meh\n";
        assert_eq!(pick_lines(log), vec!["[Error] boom", "   at A.B()", "   at C.D()"]);
        let quiet = (0..30).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let got = pick_lines(&quiet);
        assert_eq!(got.len(), 20);
        assert_eq!(got[0], "line 10");
    }

    #[test]
    fn the_same_failure_gets_the_same_fingerprint() {
        let a = fingerprint(
            "radarr",
            "lscr.io/linuxserver/radarr",
            "5.15.0",
            "exited with code 139",
            "2026-10-29 12:00:01.5|Fatal|ConsoleApp|Migration 231 failed at 0x7ff3a2b1c0",
        );
        let b = fingerprint(
            "radarr",
            "lscr.io/linuxserver/radarr",
            "5.15.0",
            "exited with code 139",
            "2026-11-02 08:13:44.9|Fatal|ConsoleApp|Migration 231 failed at 0x7ff3a2b1ff",
        );
        assert_eq!(a, b);
        assert!(a.starts_with("wf-") && a.len() == 13);
        let other = fingerprint("radarr", "lscr.io/linuxserver/radarr", "5.16.0", "exited with code 139", "");
        assert_ne!(a, other);
    }

    #[test]
    fn images_lose_their_tag() {
        assert_eq!(repo_of("lscr.io/linuxserver/radarr:latest"), "lscr.io/linuxserver/radarr");
        assert_eq!(repo_of("localhost:5000/radarr"), "localhost:5000/radarr");
        assert_eq!(repo_of("radarr@sha256:abc"), "radarr");
    }
}
