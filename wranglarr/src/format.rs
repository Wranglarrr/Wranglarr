// SPDX-License-Identifier: GPL-3.0-or-later
//! Text formats shared by the pages, the login and the notifications: sizes in
//! bytes, HTML escaping and percent-encoding.

/// A size in bytes for people: `512 B`, `1.5 GB`, up to petabytes.
pub fn bytes(mut n: f64) -> String {
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if n < 1024.0 {
            return if unit == "B" { format!("{n:.0} {unit}") } else { format!("{n:.1} {unit}") };
        }
        n /= 1024.0;
    }
    format!("{n:.1} PB")
}

/// `& < > " '` escaped, for text in HTML (also inside attribute values).
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&#34;").replace('\'', "&#39;")
}

/// Percent-encode everything but RFC 3986's unreserved characters, for a value
/// inside a URL (a `/` is encoded too).
pub fn quote(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Percent-encode a path for a URL's query, keeping its `/`s.
pub fn quote_path(s: &str) -> String {
    quote(s).replace("%2F", "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(bytes(0.0), "0 B");
        assert_eq!(bytes(1023.0), "1023 B");
        assert_eq!(bytes(1536.0), "1.5 KB");
        assert_eq!(bytes(5.0 * 1024.0 * 1024.0), "5.0 MB");
        assert_eq!(bytes(3.0 * 1024f64.powi(5)), "3.0 PB");
        assert_eq!(bytes(2048.0 * 1024f64.powi(5)), "2048.0 PB");
    }

    #[test]
    fn escaping() {
        assert_eq!(
            html_escape(r#"<a href="x">Tom & 'Jerry'</a>"#),
            "&lt;a href=&#34;x&#34;&gt;Tom &amp; &#39;Jerry&#39;&lt;/a&gt;"
        );
    }

    #[test]
    fn percent_encoding() {
        assert_eq!(quote("a b/c~é"), "a%20b%2Fc~%C3%A9");
        assert_eq!(quote_path("/suites/a b?x"), "/suites/a%20b%3Fx");
    }
}
