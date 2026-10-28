// SPDX-License-Identifier: GPL-3.0-or-later
//! wranglarr's foundation, below the services and the app: wranglarr.toml, the
//! state database, the backup store and its file copies, Docker and the image
//! registry, Postgres, and the plumbing every API client shares. Nothing here
//! knows which kinds of app exist; the service registry (wranglarr-services)
//! builds on it.

pub mod config;
pub mod docker;
pub mod error;
pub mod files;
pub mod http;
pub mod manifest;
pub mod net;
pub mod owner;
pub mod pg;
pub mod registry;
pub mod shared;
pub mod state;
pub mod store;
pub mod time;
pub mod value;
