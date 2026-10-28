// SPDX-License-Identifier: GPL-3.0-or-later
//! The job runner ([`JobRunner`]) and the suite jobs it runs: backups
//! ([`backup`]), restores and restore plans ([`restore`]), and the
//! shared-settings sync ([`shared`]).

mod backup;
mod restore;
mod runner;
mod shared;

pub use runner::{Finished, JobOutcome, JobResult, JobRunner, RunLog};

use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::store::sha256;

async fn sha256_of(p: PathBuf) -> std::io::Result<String> {
    tokio::task::spawn_blocking(move || sha256(&p)).await.map_err(std::io::Error::other)?
}

async fn size_of(p: &Path) -> u64 {
    tokio::fs::metadata(p).await.map(|m| m.len()).unwrap_or(0)
}

/// A failed step inside a job: the run fails with this message.
fn fail(msg: impl Into<String>) -> Error {
    Error::internal(msg)
}
