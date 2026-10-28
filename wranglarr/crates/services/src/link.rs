// SPDX-License-Identifier: GPL-3.0-or-later
//! The services that store their own links to the arrs (Bazarr, Seerr): what
//! Connect asks of them. Which links exist, and the arr side of the matrix
//! (download clients and Prowlarr in the arrs), stay in `connect.rs`; a service
//! that [`Takes`](super::Takes) arrs implements [`LinkSource`].

use std::fmt;

use async_trait::async_trait;
use serde::Serialize;

use wranglarr_core::config::AppConfig;
use wranglarr_core::error::Result;

/// A link's state in the app storing it, or what wiring it did. Its spelling
/// (`connected`, `differs`, …) is the `state` / `action` the HTTP API and the run
/// log show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    /// In place and pointing at the app.
    Connected,
    /// There, but pointing elsewhere, with another key, or disabled.
    Differs,
    /// Not there.
    Missing,
    /// The app storing it can't take links yet (Seerr before its own setup).
    Waiting,
    /// Wiring left an existing entry as it was.
    Kept,
    /// Wiring added the entry.
    Created,
    /// Wiring changed the entry.
    Updated,
    /// Wiring couldn't make the link.
    Failed,
    /// The state couldn't be read.
    Error,
}

impl LinkState {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkState::Connected => "connected",
            LinkState::Differs => "differs",
            LinkState::Missing => "missing",
            LinkState::Waiting => "waiting",
            LinkState::Kept => "kept",
            LinkState::Created => "created",
            LinkState::Updated => "updated",
            LinkState::Failed => "failed",
            LinkState::Error => "error",
        }
    }
}

impl fmt::Display for LinkState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A [`LinkState`] and a detail for the page and the run log: connected,
/// "sonarr:8989"; differs, "points at …"; missing, "".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkOutcome {
    pub state: LinkState,
    pub detail: String,
}

impl LinkOutcome {
    pub fn new(state: LinkState, detail: impl Into<String>) -> LinkOutcome {
        LinkOutcome { state, detail: detail.into() }
    }
}

/// One link, as the app storing it sees it.
#[derive(Clone, Debug)]
pub struct LinkTarget<'a> {
    /// The app linked in.
    pub app: &'a AppConfig,
    /// Where the source reaches it: localhost when they share a network namespace.
    pub host: String,
    pub port: u16,
    /// Its URL base ("" for none).
    pub base: String,
    pub ssl: bool,
    /// The host in the app's own URL (an entry added by hand may use that one).
    pub url_host: Option<String>,
}

/// `new`: only links never handled before, and an existing entry is kept.
/// `all`: create or update every link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireMode {
    New,
    All,
}

/// What Connect knows that a link source may need while wiring.
#[async_trait]
pub trait LinkHost: Send + Sync {
    /// The address people open `app` at; "" when unknown.
    async fn public_url(&self, app: &AppConfig) -> String;
}

/// A service that stores links to other apps.
#[async_trait]
pub trait LinkSource: Send + Sync {
    /// Whether it takes links yet (Seerr: not before its own setup is done).
    async fn accepts_links(&self) -> bool {
        true
    }
    /// The link's state in the app (read only).
    async fn link_state(&self, to: &LinkTarget<'_>) -> Result<LinkOutcome>;
    /// Create or update the link.
    async fn wire(&self, to: &LinkTarget<'_>, mode: WireMode, host: &dyn LinkHost) -> Result<LinkOutcome>;
}
