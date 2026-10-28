// SPDX-License-Identifier: GPL-3.0-or-later
//! Browser URL prefixes. Bare routes remain available to internal callers.

use crate::error::{Error, Result};

/// Namespaces owned by bare routes: a base must not shadow internal endpoints.
pub(crate) const ROOT_NAMESPACES: &[&str] = &[
    "api",
    "static",
    "setup",
    "login",
    "account",
    "suites",
    "activity",
    "runs",
    "system",
    "notifications",
    "connections",
    "updates",
    "indexers",
    "proxy",
    "configs",
    "keys",
];

/// Normalize the configured base; only unambiguous, literal path segments are allowed.
pub fn check(value: &str) -> Result<String> {
    let base = value.trim().trim_end_matches('/');
    if base.is_empty() {
        return Ok(String::new());
    }
    let segments: Vec<_> = base.strip_prefix('/').unwrap_or("").split('/').collect();
    if !base.starts_with('/')
        || segments.iter().any(|s| {
            s.is_empty()
                || *s == "."
                || *s == ".."
                || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
        })
        || ROOT_NAMESPACES.contains(&segments[0])
    {
        return Err(Error::invalid(
            "WRANGLARR_URL_BASE: use a path like /wranglarr, or leave it blank; it must not shadow a wranglarr route such as /api",
        ));
    }
    Ok(base.to_string())
}

/// Strip exactly one base, on a segment boundary. `/w` also serves the home page.
pub fn strip<'a>(base: &str, path: &'a str) -> Option<&'a str> {
    if base.is_empty() {
        return None;
    }
    let rest = path.strip_prefix(base)?;
    if rest.is_empty() { Some("/") } else { rest.starts_with('/').then_some(rest) }
}

/// Prefix a local URL once. Absolute and protocol-relative URLs stay unchanged.
pub fn prefix(base: &str, url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    if base.is_empty() || !url.starts_with('/') || url.starts_with("//") || strip(base, path).is_some() {
        url.to_string()
    } else {
        format!("{base}{url}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bases_and_boundaries() {
        assert_eq!(check(" /wranglarr/ ").expect("base"), "/wranglarr");
        assert_eq!(check("/").expect("root"), "");
        assert_eq!(check("/tools/w").expect("segments"), "/tools/w");
        for bad in ["w", "//w", "/api", "/api/tools", "/w/../api", "/w//x", "/w?x", "/w%2fx", "/w\\x"] {
            assert!(check(bad).is_err(), "{bad}");
        }
        assert_eq!(strip("/w", "/w"), Some("/"));
        assert_eq!(strip("/w", "/w/api/health"), Some("/api/health"));
        assert_eq!(strip("/w", "/whatever"), None);
        assert_eq!(prefix("/w", "/w/suites?q=1"), "/w/suites?q=1");
        assert_eq!(prefix("/w", "/suites?q=1"), "/w/suites?q=1");
        assert_eq!(prefix("/w", "https://example.com/radarr"), "https://example.com/radarr");
    }
}
