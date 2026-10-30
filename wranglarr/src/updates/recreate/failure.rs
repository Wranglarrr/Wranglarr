// SPDX-License-Identifier: GPL-3.0-or-later
//! Finalize a failed swap after its callbacks finish. History and rejected-image
//! handling precede health checks; recovery notes precede API restoration; the
//! failure report is built only after both container and data outcomes are known.

use super::super::recovery::{ContainerRecovery, DataPhase, Outcome, Placement, Recovery};
use super::super::{OLD_SUFFIX, Target, Updater};
use super::DataBack;
use super::on_run::{DataProgress, OnRun};
use super::swap::{AttemptedSwap, SwapFacts};
use crate::error::Error;
use crate::jobs::RunLog;
use crate::jobs::restore::{ApiAvailability, ApiRestoreFailed, ApiRestoreOutcome, Prepared};
use crate::value::head;

/// What to do about a container a failed swap left parked or half put
/// back, for the Updates page while it's like that. `None` when nothing
/// is left to sort out by hand.
fn recovery_note(name: &str, state: Recovery, progress: &DataProgress) -> Option<String> {
    let parked = format!("{name}{OLD_SUFFIX}");
    let data = progress.note.clone();
    let data_back = "if the new version ran, put the data back from the pre-update backup";
    Some(match state {
        Recovery::Parked if progress.recovery.state == DataPhase::NeedsRecovery => data,
        Recovery::Parked => format!(
            "The old container couldn't be renamed back from {parked} (see the run log). {data} \
                 Rename it to {name} and start it."
        ),
        Recovery::Rejected => format!(
            "The new {name} couldn't be removed; it's stopped. The old container is parked as {parked}. \
                 Remove {name}, {data_back}, then rename {parked} to {name} and start it."
        ),
        Recovery::RejectedRunning => format!(
            "The new {name} couldn't be removed or stopped, and may still be changing the data. Stop and \
                 remove it first. Then {data_back}, rename {parked} to {name} and start it."
        ),
        Recovery::Unknown => format!(
            "wranglarr couldn't tell how {name} was left. Check {name} and {parked} in Docker, and don't \
                 start either until you know which data it would run on."
        ),
        Recovery::Running | Recovery::Unhealthy | Recovery::Stopped | Recovery::NotStarted => return None,
    })
}

impl Updater {
    /// Keep the failure sequence here; helpers return facts to the next phase.
    pub(super) async fn finish_failed_swap(
        &self,
        on_run: OnRun<'_>,
        action: &str,
        error: &Error,
        attempt: AttemptedSwap,
    ) {
        let (rl, t, data) = (on_run.rl, on_run.target, on_run.data);
        let name = t.container.as_str();
        // swap has returned: no callbacks can mutate this progress any more.
        let (mut progress, captured) = on_run.finish();
        let AttemptedSwap { facts, recovery, was_running } = attempt;

        self.record(rl, facts.row(action, "failed", head(&error.to_string(), 300))).await;
        let rejected = action == "update" && facts.new_image != facts.old_image;
        let blocked = if rejected { self.reject_failed_image(rl, name, &facts, error).await } else { false };
        let state = self.recovered_container_state(rl, t, &recovery, was_running).await;

        if matches!(recovery, ContainerRecovery::Incomplete { .. }) {
            let note = recovery_note(name, state, &progress);
            // keep_stopped's own error already said it.
            let said = state == Recovery::Parked && progress.recovery.state == DataPhase::NeedsRecovery;
            if let Some(n) = note.as_ref().filter(|_| !said) {
                rl.append(format!("{name}: {n}")).await;
            }
            self.set_recovery(name, note).await;
        }
        let state = self.restore_recovered_config(rl, t, &mut progress, state).await;

        if rejected {
            if progress.note.is_empty() && matches!(data, DataBack::None) && t.role == "app" {
                progress.note = "No pre-update backup was made, so the data wasn't put back.".into();
            }
            let recovery_phase = match recovery {
                ContainerRecovery::Incomplete { phase, .. } => Some(phase),
                ContainerRecovery::Restored { .. } => None,
            };
            let outcome =
                Outcome { state, blocked, data: progress.note, data_recovery: progress.recovery, recovery_phase };
            self.report_failure(rl, t, &facts, &error.to_string(), captured.as_ref(), &outcome).await;
        }
    }

    async fn reject_failed_image(&self, rl: &RunLog, name: &str, f: &SwapFacts, error: &Error) -> bool {
        let reason = head(&format!("{} failed to start: {error}", f.new_shown()), 200);
        let blocked = match self.block(name, &f.new_image, &reason).await {
            Ok(()) => true,
            Err(e3) => {
                rl.append(format!("{name}: could not remember the rejected image: {e3}")).await;
                false
            }
        };
        self.retag(rl, &f.reference, &f.old_image).await;
        blocked
    }

    /// Being back under its name does not establish health. Check before an
    /// API restore is allowed to use the old container.
    async fn recovered_container_state(
        &self,
        rl: &RunLog,
        t: &Target,
        recovery: &ContainerRecovery,
        was_running: bool,
    ) -> Recovery {
        let name = t.container.as_str();
        // back under its name is not the same as up: look
        match recovery.placement() {
            Placement::Back { running: true } => match self.wait_healthy(rl, t, name).await {
                Ok(()) => Recovery::Running,
                Err(e) => {
                    rl.append(format!("{name}: the old container is back but didn't come up healthy ({e}); check it"))
                        .await;
                    Recovery::Unhealthy
                }
            },
            Placement::Back { running: false } if was_running => Recovery::NotStarted,
            Placement::Back { running: false } => Recovery::Stopped,
            Placement::Parked => Recovery::Parked,
            Placement::Rejected { running: false } => Recovery::Rejected,
            Placement::Rejected { running: true } => Recovery::RejectedRunning,
            Placement::Unknown => Recovery::Unknown,
        }
    }

    /// Complete the API part with owned progress, with no swap mutex held.
    async fn restore_recovered_config(
        &self,
        rl: &RunLog,
        t: &Target,
        progress: &mut DataProgress,
        state: Recovery,
    ) -> Recovery {
        let name = t.container.as_str();
        let Some(p) = progress.prepared.take().filter(Prepared::api_config) else { return state };
        if state != Recovery::Running {
            progress.note = format!(
                "{}'s own backup wasn't put back: that goes through its API, and the old version isn't up.",
                p.app.name
            );
            return state;
        }
        let result = self.app.restore_api_config_within(rl, &p, self.health().timeout).await;
        let (api, availability, note) = match result {
            Ok(api) => (api, ApiAvailability::Ready, "The data was put back from the pre-update backup.".into()),
            Err(ApiRestoreFailed { error: e, outcome, availability }) => {
                let note = match outcome {
                    ApiRestoreOutcome::UnavailableBeforeUpload => format!(
                        "{}'s API became unavailable before its config restore; upload was not attempted ({e}).",
                        p.app.name
                    ),
                    ApiRestoreOutcome::RestartNotReady => format!(
                        "The databases were put back, but {} restarted to apply its own backup and didn't come back ({e}).",
                        p.app.name
                    ),
                    ApiRestoreOutcome::UploadOutcomeUnknown => format!(
                        "The databases were put back, but the outcome of {}'s config upload is unknown ({e}); inspect it before retrying.",
                        p.app.name
                    ),
                    _ => format!("The app's config couldn't be put back through its API ({e})."),
                };
                rl.append(format!(
                    "{name}: could not complete {}'s config restore from {} ({e}); check it",
                    p.app.name, p.version
                ))
                .await;
                (outcome, availability, note)
            }
        };
        let state = state.after_api(availability);
        progress.recovery.api = api;
        if api == ApiRestoreOutcome::Verified {
            progress.recovery.state = DataPhase::Verified;
            progress.recovery.completed.push("API config".into());
        }
        progress.note = note;
        state
    }
}
