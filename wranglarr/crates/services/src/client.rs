// SPDX-License-Identifier: GPL-3.0-or-later
//! [`AnyClient`]: any configured app's API client, as a shared [`Service`], built
//! by its kind's descriptor ([`client`]). The rest of wranglarr (backups,
//! restores, overview, sync) holds one of these and asks for capabilities rather
//! than branching on the kind. Each kind's client lives in its own folder here
//! (`arr/client.rs`, `bazarr/client.rs`, …).
//!
//! Every upstream failure is an [`ArrError`], whatever the app.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::AppConfigExt;
use crate::arr::ArrClient;
use crate::downloads::Download;
use crate::{DownloadClient, Service, ServiceDef};
use wranglarr_core::config::AppConfig;
use wranglarr_core::error::{Error, Result};
use wranglarr_core::http::{ArrError, ArrResult};

/// Any app's API client: a [`Service`], shared.
#[derive(Clone)]
pub struct AnyClient(Arc<dyn Service>);

/// The client for an app, built by its kind's service file off the async threads:
/// building one reads the app's credential file and sets up TLS.
pub async fn client(app: &AppConfig) -> Result<AnyClient> {
    let app = app.clone();
    tokio::task::spawn_blocking(move || client_blocking(&app)).await?
}

/// [`client`] on the calling thread, for code that isn't async: the command-line
/// tools and closures that already run on a blocking thread.
pub fn client_blocking(app: &AppConfig) -> Result<AnyClient> {
    let def = app.def().ok_or_else(|| Error::invalid(format!("{}: unsupported app kind {:?}", app.name, app.kind)))?;
    (def.build)(app)
}

impl AnyClient {
    pub fn new(service: impl Service) -> AnyClient {
        AnyClient(Arc::new(service))
    }

    /// The service behind it, for its capabilities.
    pub fn service(&self) -> &dyn Service {
        self.0.as_ref()
    }

    pub fn def(&self) -> &'static ServiceDef {
        self.0.def()
    }

    pub fn name(&self) -> &str {
        self.0.name()
    }

    pub fn kind(&self) -> &str {
        self.0.def().id
    }

    pub async fn ping(&self) -> bool {
        self.0.ping().await
    }

    /// Up and answering within `timeout`.
    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        self.0.wait_ready(timeout).await
    }

    /// `system/status` in the arrs' shape: version, databaseType, instanceName, branch, …
    pub async fn status(&self) -> ArrResult<Value> {
        self.0.status().await
    }

    /// Library / queue counts.
    pub async fn collection(&self) -> ArrResult<Value> {
        self.0.collection().await
    }

    /// How much configuration the backup covers (counts per settings list).
    pub async fn scope(&self) -> Value {
        self.0.scope().await
    }

    /// Health messages, `[{"type": "warning"|"error", "message": …}]`.
    pub async fn health(&self) -> Vec<Value> {
        self.0.health().await
    }

    pub async fn restart(&self) -> ArrResult<()> {
        self.0.restart().await
    }

    /// The app's own backup zip, saved into `dest_dir`. None for the apps that
    /// have none (download clients, Seerr): only their config folder is copied.
    pub async fn capture_config(&self, dest_dir: &Path) -> ArrResult<Option<PathBuf>> {
        self.0.capture_config(dest_dir).await
    }

    /// Put config back: an arr or Bazarr gets its backup zip, the others a
    /// config snapshot folder (only while stopped).
    pub async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value> {
        self.0.restore_config(path, app_stopped).await
    }

    // ---- shared resources: arrs only ---------------------------------------
    pub async fn list_resources(&self, resource: &str) -> ArrResult<Vec<Value>> {
        match self.0.arr() {
            Some(c) => c.list(resource).await,
            None => Err(self.0.no_resources(resource)),
        }
    }

    pub async fn test_resource(&self, resource: &str, body: &Value) -> ArrResult<()> {
        match self.0.arr() {
            Some(c) => c.test_resource(resource, body).await,
            None => Err(self.0.no_resources(resource)),
        }
    }

    /// Create or update by name: ("created"|"updated", the saved resource).
    pub async fn upsert_resource(&self, resource: &str, body: &Value) -> ArrResult<(&'static str, Value)> {
        match self.0.arr() {
            Some(c) => c.upsert_resource(resource, body).await,
            None => Err(self.0.no_resources(resource)),
        }
    }

    // ---- download clients ----------------------------------------------------
    fn dl(&self) -> ArrResult<&dyn DownloadClient> {
        self.0.download_client().ok_or_else(|| ArrError::new(format!("{}: not a download client", self.name())))
    }

    /// Current downloads (download clients only).
    pub async fn downloads(&self) -> ArrResult<Vec<Download>> {
        self.dl()?.downloads().await
    }

    /// Where completed downloads go (download clients only).
    pub async fn download_root(&self) -> ArrResult<String> {
        self.dl()?.download_root().await
    }

    /// Create the missing categories (SABnzbd, NZBGet). Returns the ones created.
    pub async fn ensure_categories(&self, names: &[String]) -> ArrResult<Vec<String>> {
        match self.0.download_client() {
            Some(d) => d.ensure_categories(names).await,
            None => Err(ArrError::new(format!("{}: has no categories to create", self.name()))),
        }
    }

    // ---- capabilities ----------------------------------------------------------
    /// The arr API, when it's an arr.
    pub fn arr(&self) -> Option<&ArrClient> {
        self.0.arr()
    }
    /// It stores its own links to other apps (Connect).
    pub fn link_source(&self) -> Option<&dyn crate::link::LinkSource> {
        self.0.link_source()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app(kind: &str) -> AppConfig {
        AppConfig {
            name: format!("my{kind}"),
            kind: kind.into(),
            url: "http://127.0.0.1:9/".into(),
            api_key: "k".into(),
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn picks_the_client() {
        for kind in ["radarr", "prowlarr", "bazarr", "qbittorrent", "sabnzbd", "nzbget", "seerr"] {
            let c = client_blocking(&app(kind)).expect("client");
            assert_eq!(c.kind(), kind);
            assert_eq!(c.name(), format!("my{kind}"));
        }
        assert!(client_blocking(&app("radarr")).expect("arr").arr().is_some());
        assert!(client_blocking(&app("seerr")).expect("seerr").link_source().is_some());
        let mut a = app("sabnzbd");
        a.api_key.clear();
        let e = client_blocking(&a).err().expect("needs a key");
        assert_eq!(e.to_string(), "mysabnzbd: api_key or api_key_file is required");
        let mut a = app("qbittorrent");
        a.api_key.clear();
        assert!(client_blocking(&a).is_ok(), "qbittorrent signs in with a username and password");
    }

    #[tokio::test]
    async fn what_other_kinds_lack() {
        let qb = client(&app("qbittorrent")).await.expect("qb");
        assert!(qb.capture_config(Path::new("/nonexistent")).await.expect("no backup api").is_none());
        let e = qb.test_resource("downloadclient", &json!({})).await.expect_err("no resources");
        assert_eq!(e.message(), "myqbittorrent: has no 'downloadclient' resources");
        let e = qb.restart().await.expect_err("no restart");
        assert_eq!(e.message(), "myqbittorrent: no restart API; restart the container instead");
        let e = qb.restore_config(Path::new("/x"), false).await.expect_err("running");
        assert!(
            e.message().starts_with("myqbittorrent: config files can only be restored while the container is stopped")
        );
        assert_eq!(qb.scope().await, json!({}));
        assert!(qb.health().await.is_empty());

        let bz = client(&app("bazarr")).await.expect("bazarr");
        let e = bz.upsert_resource("notification", &json!({})).await.expect_err("no resources");
        assert_eq!(e.message(), "mybazarr: bazarr has no 'notification' resources");
        let e = bz.downloads().await.expect_err("not a download client");
        assert_eq!(e.message(), "mybazarr: not a download client");
    }
}
