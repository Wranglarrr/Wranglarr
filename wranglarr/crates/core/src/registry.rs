// SPDX-License-Identifier: GPL-3.0-or-later
//! Ask a registry for a tag's current digest (OCI distribution API, anonymous).
//!
//! Used when the engine can't do it for us: Docker answers
//! `GET /distribution/{ref}/json`, Podman's compatible API does not.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

const ACCEPT: &str = "application/vnd.oci.image.index.v1+json, \
                      application/vnd.docker.distribution.manifest.list.v2+json, \
                      application/vnd.docker.distribution.manifest.v2+json, \
                      application/vnd.oci.image.manifest.v1+json";

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct RegistryError(pub String);

/// `postgres:17` -> (`registry-1.docker.io`, `library/postgres`, `17`)
pub fn parse(reference: &str) -> (String, String, String) {
    let (mut name, mut tag) = (reference.to_string(), "latest".to_string());
    if let Some((n, d)) = reference.split_once('@') {
        (name, tag) = (n.to_string(), d.to_string());
    } else if reference.rsplit('/').next().is_some_and(|last| last.contains(':'))
        && let Some((n, t)) = reference.rsplit_once(':')
    {
        (name, tag) = (n.to_string(), t.to_string());
    }
    let (host, mut repo) = match name.split_once('/') {
        Some((first, rest))
            if !rest.is_empty() && (first.contains('.') || first.contains(':') || first == "localhost") =>
        {
            (first.to_string(), rest.to_string())
        }
        _ => ("docker.io".to_string(), name.clone()),
    };
    let host = if host == "docker.io" || host == "index.docker.io" {
        if !repo.contains('/') {
            repo = format!("library/{repo}");
        }
        "registry-1.docker.io".to_string()
    } else {
        host
    };
    (host, repo, tag)
}

static CHALLENGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(\w+)="([^"]*)""#).expect("static regex"));

fn challenge(header: &str) -> HashMap<String, String> {
    CHALLENGE.captures_iter(header).map(|c| (c[1].to_string(), c[2].to_string())).collect()
}

pub async fn remote_digest(reference: &str) -> Result<String, RegistryError> {
    let (host, repo, tag) = parse(reference);
    let url = format!("https://{host}/v2/{repo}/manifests/{tag}");
    let c = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| RegistryError(format!("{host}: {}", e.without_url())))?;
    let net = |e: reqwest::Error| RegistryError(format!("{host}: {}", e.without_url()));
    let mut r = c.head(&url).header("Accept", ACCEPT).send().await.map_err(net)?;
    if r.status() == 401 {
        let ch = challenge(r.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).unwrap_or(""));
        let Some(realm) = ch.get("realm") else {
            return Err(RegistryError(format!("{host}: unauthorized and no token realm offered")));
        };
        let scope = ch.get("scope").cloned().unwrap_or_else(|| format!("repository:{repo}:pull"));
        let service = ch.get("service").cloned().unwrap_or_default();
        let t = c.get(realm).query(&[("service", service), ("scope", scope)]).send().await.map_err(net)?;
        if t.status() != 200 {
            return Err(RegistryError(format!("{host}: token request -> {} (private image?)", t.status().as_u16())));
        }
        let j = crate::http::response_json(t).await.map_err(|e| RegistryError(format!("{host}: {e}")))?;
        let tok = j["token"].as_str().or(j["access_token"].as_str()).unwrap_or("").to_string();
        r = c.head(&url).header("Accept", ACCEPT).bearer_auth(tok).send().await.map_err(net)?;
    }
    if r.status() == 429 {
        return Err(RegistryError(format!("{host}: rate limited (toomanyrequests)")));
    }
    if r.status() != 200 {
        return Err(RegistryError(format!("{host}/{repo}:{tag} -> {}", r.status().as_u16())));
    }
    r.headers()
        .get("docker-content-digest")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .ok_or_else(|| RegistryError(format!("{host}: no Docker-Content-Digest header")))
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn refs() {
        let p = |r: &str| {
            let (h, repo, t) = parse(r);
            format!("{h} {repo} {t}")
        };
        assert_eq!(p("postgres:17"), "registry-1.docker.io library/postgres 17");
        assert_eq!(p("lscr.io/linuxserver/radarr:latest"), "lscr.io linuxserver/radarr latest");
        assert_eq!(p("ghcr.io/a/b"), "ghcr.io a/b latest");
        assert_eq!(p("qmcgaw/gluetun@sha256:ab"), "registry-1.docker.io qmcgaw/gluetun sha256:ab");
        assert_eq!(p("localhost:5000/x:1"), "localhost:5000 x 1");
    }
}
