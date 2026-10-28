// SPDX-License-Identifier: GPL-3.0-or-later
//! [`FileApp`]: the plumbing of an app whose config is backed up as a plain copy of
//! its mounted config folder (see wranglarr_core::files): the download clients and
//! Seerr.

use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Value, json};

use wranglarr_core::http::{ArrError, ArrResult, http};
use wranglarr_core::value::head;

/// An app whose config is a folder wranglarr copies: its name, address, HTTP
/// client and mounted config folder.
#[derive(Clone)]
pub(crate) struct FileApp {
    pub(crate) name: String,
    pub(crate) base: String,
    /// The app's /config, mounted into wranglarr.
    pub(crate) config_dir: Option<PathBuf>,
    pub(crate) http: reqwest::Client,
}

impl FileApp {
    pub(crate) fn new(name: &str, url: &str, config_dir: &str, timeout: Duration) -> FileApp {
        FileApp {
            name: name.into(),
            base: url.trim_end_matches('/').to_string(),
            config_dir: (!config_dir.is_empty()).then(|| PathBuf::from(config_dir)),
            http: http(timeout),
        }
    }

    /// An error status as an ArrError.
    pub(crate) fn check(&self, what: &str, status: StatusCode, body: &str) -> ArrResult<()> {
        if status.as_u16() >= 400 {
            return Err(ArrError::with_status(
                status,
                format!("{}: {what} -> {}: {}", self.name, status.as_u16(), head(body, 160)),
            ));
        }
        Ok(())
    }

    pub(crate) fn restart_error(&self) -> ArrError {
        ArrError::new(format!("{}: no restart API; restart the container instead", self.name))
    }

    /// Put a config snapshot folder back into config_dir. Only while the app is stopped.
    pub(crate) async fn restore_config(&self, path: &Path, app_stopped: bool) -> ArrResult<Value> {
        if !app_stopped {
            return Err(ArrError::new(format!(
                "{}: config files can only be restored while the container is stopped \
                 (mount the docker socket and set `container`)",
                self.name
            )));
        }
        let Some(cd) = self.config_dir.clone() else {
            return Err(ArrError::new(format!("{}: config_dir is required", self.name)));
        };
        let (name, src) = (self.name.clone(), path.to_path_buf());
        tokio::task::spawn_blocking(move || wranglarr_core::files::restore_config(&name, &src, &cd))
            .await
            .map_err(|e| ArrError::new(format!("{}: {e}", self.name)))?
            // its errors name the app already
            .map_err(|e| ArrError::new(e.to_string()))?;
        Ok(json!({"restartRequired": true, "method": "files"}))
    }
}
