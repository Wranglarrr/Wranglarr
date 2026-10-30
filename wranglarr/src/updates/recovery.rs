// SPDX-License-Identifier: GPL-3.0-or-later
//! Recovery facts, independent of the wording of a failure report.

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::jobs::restore::{ApiAvailability, ApiRestoreOutcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Placement {
    Rejected { running: bool },
    Parked,
    Back { running: bool },
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RecoveryPhase {
    RemoveNew,
    RestoreData,
    RenameOld,
    StartOld,
}

impl std::fmt::Display for RecoveryPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RemoveNew => "removing the new container",
            Self::RestoreData => "restoring data",
            Self::RenameOld => "renaming the old container",
            Self::StartOld => "starting the old container",
        })
    }
}

/// A successful operation gives known placement; errors include reconciliation
/// with Docker because a lost response need not mean the operation failed.
#[derive(Debug)]
pub(super) enum ContainerRecovery {
    Restored { running: bool },
    Incomplete { phase: RecoveryPhase, placement: Placement, error: Error },
}

impl ContainerRecovery {
    pub(super) fn placement(&self) -> Placement {
        match self {
            Self::Restored { running } => Placement::Back { running: *running },
            Self::Incomplete { placement, .. } => *placement,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DataPhase {
    #[default]
    Untouched,
    Restoring,
    OfflineRestored,
    Verified,
    NeedsRecovery,
}

/// Data completion and API restoration are independent of container placement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct DataRecovery {
    pub(super) state: DataPhase,
    pub(super) completed: Vec<String>,
    pub(super) api: ApiRestoreOutcome,
}

/// Where the container was left after a failed update.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Recovery {
    /// Docker didn't say, or said something unexpected.
    #[default]
    Unknown,
    /// The old container is back and came up healthy.
    Running,
    /// The old container is back, but didn't come up healthy.
    Unhealthy,
    /// The old container is back, stopped, as it was before the update.
    Stopped,
    /// The old container is stopped and parked, for recovery by hand.
    Parked,
    /// The old container is back under its name, but couldn't be started.
    NotStarted,
    /// The new container couldn't be removed; it's stopped, and the old one parked.
    Rejected,
    /// The new container couldn't be removed or stopped: it may still be running.
    RejectedRunning,
}

impl Recovery {
    pub(super) fn after_api(self, availability: ApiAvailability) -> Self {
        match availability {
            ApiAvailability::Unchanged | ApiAvailability::Ready => self,
            ApiAvailability::Unavailable => Self::Unhealthy,
            ApiAvailability::Unknown => Self::Unknown,
        }
    }
}

/// Independent final outcomes used to generate a failure report.
pub(super) struct Outcome {
    pub(super) state: Recovery,
    /// The rejected image was remembered, so it isn't tried again by itself.
    pub(super) blocked: bool,
    /// What happened to the app's data.
    pub(super) data: String,
    pub(super) data_recovery: DataRecovery,
    pub(super) recovery_phase: Option<RecoveryPhase>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_availability_preserves_known_recovery_without_a_new_observation() {
        for state in [Recovery::Running, Recovery::NotStarted, Recovery::Stopped, Recovery::Parked] {
            assert_eq!(state.after_api(ApiAvailability::Unchanged), state);
        }
        assert_eq!(Recovery::Running.after_api(ApiAvailability::Unavailable), Recovery::Unhealthy);
        assert_eq!(Recovery::Running.after_api(ApiAvailability::Unknown), Recovery::Unknown);
    }
}
