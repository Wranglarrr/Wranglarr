// SPDX-License-Identifier: GPL-3.0-or-later
//! Notifications: tell the user about things they may want to act on.
//!
//! Channels (Telegram, Discord, ntfy, ...) live in the state db. Events come from two
//! places: finished jobs (backups, updates, restores, syncs) and a watcher on a timer
//! that looks at the stack every few minutes (apps offline, health warnings, VPN,
//! disk space, stuck downloads, imports an arr can't do on its own). The watcher
//! remembers what it has already reported, so each problem is sent once, and once
//! more when it clears (apps and the VPN).
//!
//! A channel can also be added to the arrs' own Connect settings, so the arrs send
//! their grabs / imports / health issues to the same place.

mod arr;
mod catalog;
mod channel;
mod delivery;
mod runs;
mod service;
mod settings;
mod watcher;

pub use channel::{ArrPick, Channel, public};
pub use delivery::Endpoints;
pub use service::{Notifier, Overview};
pub use settings::Settings;

use crate::app::App;

// state db keys
const KEY_CHANNELS: &str = "notify:channels";
const KEY_SETTINGS: &str = "notify:settings";
const KEY_STATE: &str = "notify:state";
const KEY_LOG: &str = "notify:log";
/// What a secret reads as on the page.
pub const MASK: &str = "••••••••";
/// Log entries kept.
const LOG_KEEP: usize = 100;

/// Start the watcher (from `App::start`).
pub async fn start(app: &App) {
    app.notifier().reschedule(app).await;
}

#[cfg(test)]
mod tests;
