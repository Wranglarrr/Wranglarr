// SPDX-License-Identifier: GPL-3.0-or-later
//! The configured apps, read with what the registry knows about their kinds:
//! wranglarr.toml checked against the registry ([`load`], [`parse`]), and the
//! kind-dependent parts of an [`AppConfig`] ([`AppConfigExt`]): its descriptor,
//! where its API key or login is read from.

use std::path::{Path, PathBuf};

use crate::key_file::KeyFileError;
use crate::{BackupMode, Credentials, Roles, ServiceDef};
use wranglarr_core::config::{AppConfig, Config, Kinds};
use wranglarr_core::error::{Error, Result};

/// The registry, as the kinds a config may use.
struct Registry;

impl Kinds for Registry {
    fn knows(&self, kind: &str) -> bool {
        crate::get(kind).is_some()
    }
    fn ids(&self) -> String {
        crate::ids()
    }
}

/// wranglarr.toml at `path`, read in the environment `env`
/// ([`wranglarr_core::config::process_env`] where a command starts).
pub fn load(path: &Path, env: impl Fn(&str) -> Option<String>) -> Result<Config> {
    wranglarr_core::config::load_with(path, &Registry, env)
}

/// wranglarr.toml's text, read in the environment `env`.
pub fn parse(text: &str, env: impl Fn(&str) -> Option<String>) -> Result<Config> {
    wranglarr_core::config::parse_with(text, &Registry, env)
}

/// What an app's kind adds to its [`AppConfig`].
pub trait AppConfigExt {
    /// What wranglarr knows about this app's kind (None only for a kind the
    /// config wouldn't accept, e.g. one built by hand in a test).
    fn def(&self) -> Option<&'static ServiceDef>;
    /// Whether its kind has all of `roles`.
    fn has(&self, roles: Roles) -> bool;
    /// Backed up as a copy of its config folder (no backup API).
    fn is_config_dir(&self) -> bool;
    /// A reason this app's configuration cannot produce a complete backup.
    fn backup_limitation(&self) -> Option<&'static str>;
    /// The API key: given directly, or read from the app's own config file. Reads
    /// the file on the calling thread; async code uses [`api_key`].
    fn api_key_blocking(&self) -> Result<String>;
    /// The app's /config as mounted into wranglarr, for the natural-format copy.
    fn snapshot_source(&self) -> Option<PathBuf>;
    /// Username and password: given, or read from its config file where its kind
    /// keeps them. Reads the file on the calling thread; async code uses [`credentials`].
    fn credentials_blocking(&self) -> (String, String);
}

/// The app's API key ([`AppConfigExt::api_key_blocking`]), its file read off the
/// async threads. Read each time: a restore can change the key in the file.
pub async fn api_key(app: &AppConfig) -> Result<String> {
    if !app.api_key.is_empty() {
        return Ok(app.api_key.clone());
    }
    let app = app.clone();
    tokio::task::spawn_blocking(move || app.api_key_blocking()).await?
}

/// The app's username and password ([`AppConfigExt::credentials_blocking`]), its
/// file read off the async threads.
pub async fn credentials(app: &AppConfig) -> (String, String) {
    let given = (app.username.clone(), app.password.clone());
    if (!given.0.is_empty() && !given.1.is_empty()) || app.api_key_file.is_empty() {
        return given;
    }
    let app = app.clone();
    tokio::task::spawn_blocking(move || app.credentials_blocking()).await.unwrap_or(given)
}

impl AppConfigExt for AppConfig {
    /// What wranglarr knows about this app's kind (None only for a kind the
    /// config wouldn't accept, e.g. one built by hand in a test).
    fn def(&self) -> Option<&'static ServiceDef> {
        crate::get(&self.kind)
    }
    /// Whether its kind has all of `roles`.
    fn has(&self, roles: Roles) -> bool {
        crate::has(&self.kind, roles)
    }
    /// Backed up as a copy of its config folder (no backup API).
    fn is_config_dir(&self) -> bool {
        self.def().is_some_and(|d| d.backup == BackupMode::ConfigDir)
    }

    fn backup_limitation(&self) -> Option<&'static str> {
        (self.kind == "seerr" && self.databases.is_empty()).then_some(
            "Seerr SQLite backups are not supported: its database is excluded from the config copy. Configure Seerr with PostgreSQL and list its database, or back up SQLite separately while Seerr is stopped",
        )
    }

    /// The API key: given directly, or read from the app's own config file.
    fn api_key_blocking(&self) -> Result<String> {
        if !self.api_key.is_empty() {
            return Ok(self.api_key.clone());
        }
        let (name, path) = (&self.name, &self.api_key_file);
        match self.def().map(|d| d.credentials) {
            // it signs in with a username and password, and may have a key as well
            Some(Credentials::UserPassword { optional_key, .. }) => Ok(optional_key
                .filter(|_| !self.config_dir.is_empty())
                .and_then(|(files, format)| {
                    files.iter().find_map(|f| {
                        let text = std::fs::read_to_string(Path::new(&self.config_dir).join(f)).ok()?;
                        format.read(&text).ok()
                    })
                })
                .unwrap_or_default()),
            Some(Credentials::ApiKey { file }) if !path.is_empty() => {
                let text = std::fs::read_to_string(path).map_err(|e| Error::invalid(format!("{name}: {path}: {e}")))?;
                file.read(&text).map_err(|e| match e {
                    KeyFileError::Unreadable(e) => Error::invalid(format!("{name}: {path}: {e}")),
                    KeyFileError::NoKey => Error::invalid(file.missing(name, path)),
                })
            }
            None => Err(Error::invalid(format!("{name}: unsupported app kind {:?}", self.kind))),
            Some(Credentials::ApiKey { .. }) => {
                Err(Error::invalid(format!("{name}: api_key or api_key_file is required")))
            }
        }
    }

    /// The app's /config as mounted into wranglarr, for the natural-format copy.
    fn snapshot_source(&self) -> Option<PathBuf> {
        if !self.config_dir.is_empty() {
            return Some(PathBuf::from(&self.config_dir));
        }
        if self.has(Roles::ARR) && self.api_key_file.ends_with("config.xml") {
            return Path::new(&self.api_key_file).parent().map(Path::to_path_buf);
        }
        None
    }

    fn credentials_blocking(&self) -> (String, String) {
        let (mut user, mut pw) = (self.username.clone(), self.password.clone());
        if (user.is_empty() || pw.is_empty())
            && !self.api_key_file.is_empty()
            && let Some(Credentials::UserPassword { config_keys: Some((user_key, pass_key)), .. }) =
                self.def().map(|d| d.credentials)
            && let Ok(text) = std::fs::read_to_string(&self.api_key_file)
        {
            // `key=value` at the start of a line
            let value = |key: &str| {
                text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(|v| v.trim().to_string())
            };
            if user.is_empty() {
                user = value(user_key).unwrap_or_default();
            }
            if pw.is_empty() {
                pw = value(pass_key).unwrap_or_default();
            }
        }
        (user, pw)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod backup_tests {
    use super::*;

    #[test]
    fn seerr_without_a_database_cannot_have_a_complete_backup() {
        let mut app = AppConfig { kind: "seerr".into(), ..Default::default() };
        assert!(app.backup_limitation().expect("unsupported").contains("SQLite"));
        app.databases.push("seerr".into());
        assert!(app.backup_limitation().is_none());
        app.kind = "qbittorrent".into();
        app.databases.clear();
        assert!(app.backup_limitation().is_none());
    }

    #[test]
    fn qbittorrents_key_comes_from_its_config_folder_if_it_has_one() {
        let dir = tempfile::tempdir().unwrap();
        let app = AppConfig {
            name: "qb".into(),
            kind: "qbittorrent".into(),
            config_dir: dir.path().to_string_lossy().into_owned(),
            ..Default::default()
        };
        assert_eq!(app.api_key_blocking().unwrap(), "", "no file: no key, and that's not an error");

        // hotio's layout
        std::fs::create_dir(dir.path().join("config")).unwrap();
        std::fs::write(dir.path().join("config/qBittorrent.conf"), "[WebUI]\nAPIKey=qbt_hotio\n").unwrap();
        assert_eq!(app.api_key_blocking().unwrap(), "qbt_hotio");

        // LinuxServer's layout, without a key: on to the next file
        std::fs::create_dir(dir.path().join("qBittorrent")).unwrap();
        std::fs::write(dir.path().join("qBittorrent/qBittorrent.conf"), "[WebUI]\nPort=8090\n").unwrap();
        assert_eq!(app.api_key_blocking().unwrap(), "qbt_hotio");

        // read each time: a key made or rotated in qBittorrent counts straight away
        std::fs::write(dir.path().join("qBittorrent/qBittorrent.conf"), "[WebUI]\nAPIKey=qbt_new\n").unwrap();
        assert_eq!(app.api_key_blocking().unwrap(), "qbt_new");

        let elsewhere = AppConfig { config_dir: String::new(), ..app };
        assert_eq!(elsewhere.api_key_blocking().unwrap(), "", "no config folder mounted");
    }
}
