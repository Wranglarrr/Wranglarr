// SPDX-License-Identifier: GPL-3.0-or-later
//! Looking up a failure's GitHub repo and the issues already filed there.

use serde_json::{Value, json};

/// Where GitHub's API is: `WRANGLARR_GITHUB_API` points the tests at a stand-in.
pub(super) fn github_api() -> String {
    std::env::var("WRANGLARR_GITHUB_API")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://api.github.com".into())
}

/// The oldest issue in a GitHub search answer, pull requests left out, and
/// only one in `repo`: its link is opened in the browser.
pub(super) fn first_issue(v: &Value, repo: &str) -> Option<Value> {
    let prefix = format!("https://github.com/{repo}/issues/");
    v["items"]
        .as_array()?
        .iter()
        .filter(|i| i.get("pull_request").is_none())
        .filter(|i| i["html_url"].as_str().is_some_and(|u| u.starts_with(&prefix)))
        .min_by_key(|i| i["number"].as_i64().unwrap_or(i64::MAX))
        .map(|i| json!({"number": i["number"], "title": i["title"], "state": i["state"], "url": i["html_url"]}))
}

/// `https://github.com/owner/name(.git)(/...)` -> `owner/name`
pub(super) fn github_repo(url: &str) -> Option<String> {
    let rest =
        url.trim().strip_prefix("https://github.com/").or_else(|| url.trim().strip_prefix("http://github.com/"))?;
    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let (owner, name) = (parts.next()?, parts.next()?.trim_end_matches(".git"));
    let ok = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    (ok(owner) && ok(name)).then(|| format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_github_source_label_names_the_repo() {
        assert_eq!(github_repo("https://github.com/owner/app.git").as_deref(), Some("owner/app"));
        assert_eq!(github_repo("https://github.com/owner/app/tree/main").as_deref(), Some("owner/app"));
        assert_eq!(github_repo("https://gitlab.com/owner/app"), None);
        assert_eq!(github_repo("https://github.com/owner"), None);
    }

    #[test]
    fn the_oldest_issue_in_the_repo_is_the_one() {
        let v = json!({"items": [
            {"number": 9, "title": "later", "state": "open", "html_url": "https://github.com/o/r/issues/9"},
            {"number": 3, "title": "a pr", "state": "open", "html_url": "https://github.com/o/r/pull/3", "pull_request": {}},
            {"number": 5, "title": "first", "state": "closed", "html_url": "https://github.com/o/r/issues/5"},
            {"number": 1, "title": "elsewhere", "state": "open", "html_url": "https://evil.example/o/r/issues/1"},
        ]});
        let i = first_issue(&v, "o/r").expect("an issue");
        assert_eq!(i["number"], 5);
        assert_eq!(i["url"], "https://github.com/o/r/issues/5");
        assert!(first_issue(&json!({"items": []}), "o/r").is_none());
    }
}
