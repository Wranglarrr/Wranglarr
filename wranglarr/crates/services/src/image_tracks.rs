// SPDX-License-Identifier: GPL-3.0-or-later
//! Published image tracks, keyed by publisher as well as application. A tag's
//! meaning belongs to its publisher, not to an app's internal update branch.
//! Checked against the publishers' documentation on 2026-10-27. Unknown images
//! have no guessed channels; callers can still accept a validated custom tag.

use serde::Serialize;
use wranglarr_core::docker::split_ref;

#[derive(Debug, Serialize)]
pub struct Track {
    pub tag: &'static str,
    pub label: &'static str,
}

const fn track(tag: &'static str, label: &'static str) -> Track {
    Track { tag, label }
}

const STABLE: Track = track("latest", "Stable");
const DEVELOP: Track = track("develop", "Development");
const NIGHTLY: Track = track("nightly", "Nightly");
const EDGE: Track = track("edge", "Development (main)");
const GLUETUN_DEV: Track = track("latest", "Development (latest commit)");
const DEVELOPMENT: Track = track("development", "Prerelease");
const UNSTABLE: Track = track("unstable", "Prerelease");
const LIBTORRENT2: Track = track("latest", "Stable (libtorrent v2)");
const LIBTORRENT1: Track = track("libtorrentv1", "Stable (libtorrent v1)");
const RELEASE: Track = track("release", "Stable");
const TESTING: Track = track("testing", "Testing / prerelease");

#[derive(Debug, Serialize)]
pub struct Catalog {
    pub choices: &'static [Track],
    pub docs: String,
}

/// Only exact, recognized repositories receive presets. Never suggest a tag
/// from a different publisher, including when the application name is the same.
pub fn catalog(reference: &str) -> Catalog {
    let (repo, _, _) = split_ref(reference);
    if repo == "ghcr.io/wranglarrr/wranglarr" {
        return Catalog { choices: &[STABLE, EDGE], docs: "https://wranglarrr.github.io/Wranglarr/#/Updates".into() };
    }
    if repo == "ghcr.io/seerr-team/seerr" || repo == "seerr/seerr" || repo == "docker.io/seerr/seerr" {
        return Catalog { choices: &[STABLE, DEVELOP], docs: "https://docs.seerr.dev/getting-started/docker/".into() };
    }
    if repo == "qmcgaw/gluetun" || repo == "docker.io/qmcgaw/gluetun" {
        return Catalog {
            choices: &[GLUETUN_DEV],
            docs: "https://github.com/qdm12/gluetun-wiki/blob/main/setup/docker-image-tags.md".into(),
        };
    }
    for prefix in ["lscr.io/linuxserver/", "ghcr.io/linuxserver/", "linuxserver/", "docker.io/linuxserver/"] {
        if let Some(app) = repo.strip_prefix(prefix) {
            let choices: &'static [Track] = match app {
                "radarr" | "lidarr" | "prowlarr" => &[STABLE, DEVELOP, NIGHTLY],
                "sonarr" => &[STABLE, DEVELOP],
                "bazarr" => &[STABLE, DEVELOPMENT],
                "sabnzbd" => &[STABLE, UNSTABLE, NIGHTLY],
                "nzbget" => &[STABLE, TESTING],
                "qbittorrent" => &[LIBTORRENT2, LIBTORRENT1],
                _ => &[],
            };
            return Catalog { choices, docs: format!("https://docs.linuxserver.io/images/docker-{app}/") };
        }
    }
    for prefix in ["ghcr.io/hotio/", "hotio/", "docker.io/hotio/"] {
        if let Some(app) = repo.strip_prefix(prefix) {
            let choices: &'static [Track] = match app {
                "radarr" | "lidarr" | "prowlarr" | "sabnzbd" => &[RELEASE, TESTING, NIGHTLY],
                "sonarr" | "bazarr" => &[RELEASE, NIGHTLY],
                "qbittorrent" | "nzbget" => &[RELEASE],
                _ => &[],
            };
            return Catalog { choices, docs: format!("https://hotio.dev/containers/{app}/") };
        }
    }
    Catalog { choices: &[], docs: String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishers_define_the_channels() {
        let tags = |r| catalog(r).choices.iter().map(|c| c.tag).collect::<Vec<_>>();
        assert_eq!(tags("lscr.io/linuxserver/sonarr:latest"), ["latest", "develop"]);
        assert_eq!(tags("ghcr.io/hotio/sonarr:release"), ["release", "nightly"]);
        assert_eq!(tags("lscr.io/linuxserver/bazarr:latest"), ["latest", "development"]);
        assert_eq!(tags("lscr.io/linuxserver/qbittorrent:latest"), ["latest", "libtorrentv1"]);
        assert_eq!(tags("ghcr.io/wranglarrr/wranglarr:latest"), ["latest", "edge"]);
        assert_eq!(tags("qmcgaw/gluetun:v3.41.3"), ["latest"]);
        assert!(tags("private.example/sonarr:latest").is_empty());
        assert!(tags("postgres:17").is_empty(), "do not suggest an unbounded major upgrade");
    }
}
