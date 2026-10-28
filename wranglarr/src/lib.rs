// SPDX-License-Identifier: GPL-3.0-or-later
//! wranglarr: backup, versioning and restore for an arr stack, plus the glue
//! that wires the apps together.

// The layers below, re-exported so the app reads `crate::config`, `crate::service`, …
pub(crate) use wranglarr_core::{config, docker, error, files, http, manifest, owner, pg, state, store, time, value};
pub(crate) use wranglarr_services as service;

pub mod app;
pub(crate) mod applogins;
pub mod appsauth;
pub(crate) mod arr_notifications;
pub mod auth;
pub(crate) mod background;
pub(crate) mod cli;
pub(crate) mod community;
pub(crate) mod connect;
pub mod deployment;
pub(crate) mod downloads;
pub(crate) mod events;
pub(crate) mod feeds;
pub(crate) mod format;
pub(crate) mod ids;
pub(crate) mod indexers;
pub(crate) mod jobs;
pub(crate) mod keys;
pub mod links;
pub mod mediaservers;
pub mod notify;
pub(crate) mod overview;
pub mod proxy;
pub mod proxy_setup;
pub(crate) mod restore_plan;
pub(crate) mod sched;
pub mod serve;
pub mod setup;
pub(crate) mod standalone;
pub mod updates;
pub mod url_base;
pub(crate) mod vpn;
pub mod web;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The published image, for messages and the setup wizard.
pub const IMAGE: &str = "ghcr.io/wranglarrr/wranglarr:latest";
