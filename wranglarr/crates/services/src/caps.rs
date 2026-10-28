// SPDX-License-Identifier: GPL-3.0-or-later
//! What a service's API client can do: the core every app has ([`Service`]) and
//! the capabilities only some have (`download_client()`, `feed()`, `arr()`). Modules hold an
//! [`AnyClient`](crate::AnyClient) and ask for a capability rather than
//! matching on the kind.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use super::{FeedKind, ServiceDef};
use crate::arr::ArrClient;
use crate::downloads::Download;
use crate::feed::{FeedPage, FeedQuery, Lookups};
use wranglarr_core::http::{ArrError, ArrResult};

/// One configured app's API client.
#[async_trait]
pub trait Service: Send + Sync + 'static {
    /// What wranglarr knows about its kind.
    fn def(&self) -> &'static ServiceDef;
    /// The app's name in wranglarr.toml.
    fn name(&self) -> &str;

    async fn ping(&self) -> bool;
    /// Up and answering within `timeout`.
    async fn wait_ready(&self, timeout: Duration) -> bool;
    /// `system/status` in the arrs' shape: version, databaseType, instanceName, branch, …
    async fn status(&self) -> ArrResult<Value>;
    /// Library / queue counts.
    async fn collection(&self) -> ArrResult<Value>;
    /// How much configuration the backup covers (counts per settings list).
    async fn scope(&self) -> Value;
    /// Health messages, `[{"type": "warning"|"error", "message": …}]`.
    async fn health(&self) -> Vec<Value>;
    async fn restart(&self) -> ArrResult<()>;

    /// The app's own backup zip, saved into `dest_dir`. None for the apps that
    /// have none ([`BackupMode::ConfigDir`](super::BackupMode)): only their config
    /// folder is copied.
    async fn capture_config(&self, _dest_dir: &Path) -> ArrResult<Option<PathBuf>> {
        Ok(None)
    }
    /// Put config back: the app's backup zip through its API, or a config
    /// snapshot folder (only while the app is stopped).
    async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value>;

    /// The error for asking it for an arr resource list it doesn't have.
    fn no_resources(&self, resource: &str) -> ArrError {
        ArrError::new(format!("{}: has no '{resource}' resources", self.name()))
    }

    // ---- capabilities ---------------------------------------------------------
    /// The arr API (resources, Connect, indexers, community configs).
    fn arr(&self) -> Option<&ArrClient> {
        None
    }
    /// The download client API.
    fn download_client(&self) -> Option<&dyn DownloadClient> {
        None
    }
    /// Stores its own links to other apps (the kinds its descriptor [`Takes`](super::Takes)).
    fn link_source(&self) -> Option<&dyn super::link::LinkSource> {
        None
    }
    /// The home page lists its def's `feeds` names.
    fn feed(&self) -> Option<&dyn Feed> {
        None
    }
}

/// The lists under an app on the home page: the ones its def's
/// [`feeds`](super::ServiceDef::feeds) names.
#[async_trait]
pub trait Feed: Send + Sync {
    /// One list, as `query` asks (how far ahead, how many). `lookups` keeps what's
    /// worth looking up once (Seerr's titles). A kind this app doesn't list is an
    /// error.
    async fn list(&self, kind: FeedKind, query: &FeedQuery, lookups: &Lookups) -> ArrResult<FeedPage>;
}

/// A download client: what it's downloading and where to.
#[async_trait]
pub trait DownloadClient: Send + Sync {
    /// Current downloads.
    async fn downloads(&self) -> ArrResult<Vec<Download>>;
    /// Where completed downloads go.
    async fn download_root(&self) -> ArrResult<String>;
    /// Create the missing categories; returns the ones created. For the clients
    /// whose [`DownloadsDef::categories`](super::DownloadsDef) says so.
    async fn ensure_categories(&self, _names: &[String]) -> ArrResult<Vec<String>>;
}
