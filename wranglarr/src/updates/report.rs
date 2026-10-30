// SPDX-License-Identifier: GPL-3.0-or-later
//! A report of a failed update: what was tried, how it failed, and the failed
//! container's last log lines, with secrets, addresses and domains taken out.
//!
//! Each report has a fingerprint, `wf-` and ten hex digits, made from what the
//! failure looks like rather than when or where it happened. People who hit
//! the same failure get the same fingerprint, so one search of the issues
//! finds the report someone already filed.

mod fingerprint;
mod github;
mod redact;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::recovery::{DataRecovery, Outcome, Recovery, RecoveryPhase};
use super::recreate::SwapFacts;
use super::{OLD_SUFFIX, Target, Updater};
use crate::jobs::RunLog;
use crate::service::ReportTo;
use crate::value::head;
use fingerprint::{ERROR_LINE, fingerprint, pick_lines, repo_of};
use github::{first_issue, github_api, github_repo};
use redact::Redactor;

/// The state key the reports are kept under, newest first.
pub(crate) const KEY: &str = "updates:reports";
/// How many are kept.
const KEEP: usize = 25;
/// The issue form in the repo the report goes to.
pub(crate) const TEMPLATE: &str = "update-failure.yml";
/// Log lines a report keeps at most.
const LOG_LINES: usize = 60;
/// A GitHub new-issue link longer than this is cut by browsers or GitHub: the
/// report is then pasted by hand.
const URL_MAX: usize = 7000;

/// What a failed container looked like just before it was removed.
#[derive(Debug, Clone, Default)]
pub(super) struct Captured {
    pub(crate) log: String,
    /// Its `State` from inspect: exit code, OOM, health checks.
    pub(crate) state: Value,
}

/// One failed update of one container.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Report {
    /// This failure here: `<run>-<container>`. Its page and API go by it.
    pub(crate) key: String,
    /// The managed container as named here, for finding its reports. Only the
    /// cleaned `container` goes into what's exported.
    pub(crate) target: String,
    /// The fingerprint: the same failure anywhere has the same one.
    pub(crate) id: String,
    pub(crate) container: String,
    pub(crate) kind: String,
    pub(crate) label: String,
    /// The image without its tag or digest.
    pub(crate) image: String,
    pub(crate) old_version: String,
    pub(crate) new_version: String,
    pub(crate) old_image: String,
    pub(crate) new_image: String,
    pub(crate) error: String,
    pub(crate) exit_code: Option<i64>,
    pub(crate) oom: bool,
    pub(crate) health: Vec<String>,
    pub(crate) state: Recovery,
    pub(crate) blocked: bool,
    /// What happened to the app's data.
    pub(crate) data: String,
    pub(crate) data_recovery: DataRecovery,
    pub(crate) recovery_phase: Option<RecoveryPhase>,
    pub(crate) log: Vec<String>,
    pub(crate) wranglarr: String,
    /// `owner/name` on GitHub; "" when nobody is known to report it to.
    pub(crate) repo: String,
    /// The repo is wranglarr's or an official lariat's.
    pub(crate) curated: bool,
    pub(crate) at: String,
    pub(crate) run_id: i64,
}

/// What [`Report::new`] is made from.
pub(super) struct Failure<'a> {
    pub(crate) container: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) label: &'a str,
    pub(crate) facts: &'a SwapFacts,
    pub(crate) error: &'a str,
    pub(crate) captured: Option<&'a Captured>,
    pub(crate) outcome: &'a Outcome,
    pub(crate) report_to: ReportTo,
    pub(crate) run_id: i64,
}

impl Report {
    /// The report of `f`, every field of it through `r`: it's made to be sent.
    pub(super) fn new(f: &Failure<'_>, r: &Redactor, now: &str) -> Report {
        let st = f.captured.map(|c| &c.state).unwrap_or(&Value::Null);
        let raw_log = f.captured.map(|c| c.log.as_str()).unwrap_or("");
        let log = pick_lines(&r.clean(raw_log));
        let error = r.clean(f.error);
        let health: Vec<String> = st["Health"]["Log"]
            .as_array()
            .map(|l| {
                let outs: Vec<String> = l
                    .iter()
                    .filter_map(|h| h["Output"].as_str())
                    .map(|o| head(r.clean(o).trim(), 300))
                    .filter(|o| !o.is_empty())
                    .collect();
                outs[outs.len().saturating_sub(3)..].to_vec()
            })
            .unwrap_or_default();
        // a private registry's name is as much yours as your domain
        let image = r.clean(&repo_of(&f.facts.reference));
        let (old_version, new_version) = (r.clean(&f.facts.old_version), r.clean(&f.facts.new_version));
        let first_error = log.iter().find(|l| ERROR_LINE.is_match(l)).map_or("", String::as_str);
        let id = fingerprint(f.kind, &image, &new_version, &error, first_error);
        Report {
            key: format!("{}-{}", f.run_id, f.container),
            target: f.container.to_owned(),
            id,
            container: r.clean(f.container),
            kind: f.kind.to_owned(),
            label: r.clean(f.label),
            image,
            old_version,
            new_version,
            old_image: super::short(&f.facts.old_image),
            new_image: super::short(&f.facts.new_image),
            error,
            exit_code: st["ExitCode"].as_i64().filter(|c| *c != 0),
            oom: st["OOMKilled"].as_bool().unwrap_or(false),
            health,
            state: f.outcome.state,
            blocked: f.outcome.blocked,
            data: r.clean(&f.outcome.data),
            data_recovery: DataRecovery {
                completed: f.outcome.data_recovery.completed.iter().map(|part| r.clean(part)).collect(),
                ..f.outcome.data_recovery.clone()
            },
            recovery_phase: f.outcome.recovery_phase,
            log,
            wranglarr: crate::VERSION.to_owned(),
            repo: f.report_to.repo.clone(),
            curated: f.report_to.curated,
            at: now.to_owned(),
            run_id: f.run_id,
        }
    }

    pub(super) fn version(v: &str, image: &str) -> String {
        if v.is_empty() { format!("image {image}") } else { v.to_owned() }
    }

    /// Where the container was left, in a sentence or two: never more than
    /// was found.
    pub(crate) fn outcome(&self) -> String {
        let (c, old, new) = (
            &self.container,
            Self::version(&self.old_version, &self.old_image),
            Self::version(&self.new_version, &self.new_image),
        );
        let state = match self.state {
            Recovery::Running => format!("{c} is back on {old}."),
            Recovery::Unhealthy => format!("{c} was put back on {old}, but it didn't come up healthy: check it."),
            Recovery::Stopped => format!("{c} was put back on {old}, stopped as it was."),
            Recovery::Parked => format!(
                "{c} is stopped: the old container is parked as {c}{OLD_SUFFIX} for recovery by hand (see the run log)."
            ),
            Recovery::NotStarted => format!("{c} was put back on {old}, but couldn't be started: start it by hand."),
            Recovery::Rejected => format!(
                "{c} is stopped: the new container couldn't be removed, and the old one is parked as \
                 {c}{OLD_SUFFIX} for recovery by hand (see the run log)."
            ),
            Recovery::RejectedRunning => format!(
                "The new container {c} couldn't be removed or stopped and may still be running: stop it before \
                 anything else, then recover {c}{OLD_SUFFIX} by hand (see the run log)."
            ),
            Recovery::Unknown => format!(
                "wranglarr couldn't tell how {c} was left: check {c} and {c}{OLD_SUFFIX} in Docker before \
                 starting either (see the run log)."
            ),
        };
        let block = if self.blocked {
            format!("{new} is skipped from now on.")
        } else {
            format!("wranglarr couldn't record that {new} failed, so it may be tried again: hold {c} until it's fixed.")
        };
        [state, block, self.data.clone()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" ")
    }

    /// The issue title: it carries the fingerprint so a search finds it.
    pub(crate) fn title(&self) -> String {
        format!(
            "Update failed: {} {} → {} [{}]",
            self.label,
            Self::version(&self.old_version, &self.old_image),
            Self::version(&self.new_version, &self.new_image),
            self.id
        )
    }

    /// The report as Markdown, for the issue body.
    pub(crate) fn markdown(&self) -> String {
        let mut m = String::new();
        let line = |m: &mut String, k: &str, v: &str| m.push_str(&format!("- **{k}:** {v}\n"));
        m.push_str(&format!("### Update failure `{}`\n\n", self.id));
        line(&mut m, "App", &format!("{} (`{}`)", self.label, self.kind));
        line(&mut m, "Image", &format!("`{}`", self.image));
        line(
            &mut m,
            "From → to",
            &format!(
                "{} (`{}`) → {} (`{}`)",
                Self::version(&self.old_version, &self.old_image),
                self.old_image,
                Self::version(&self.new_version, &self.new_image),
                self.new_image
            ),
        );
        line(&mut m, "wranglarr", &self.wranglarr);
        line(&mut m, "What happened", &self.error);
        if let Some(c) = self.exit_code {
            line(&mut m, "Exit code", &c.to_string());
        }
        if self.oom {
            line(&mut m, "Out of memory", "yes, the kernel killed it");
        }
        let after = self.outcome();
        if !after.is_empty() {
            line(&mut m, "Afterwards", &after);
        }
        if !self.health.is_empty() {
            m.push_str("\n#### Health check\n\n```\n");
            m.push_str(&self.health.join("\n"));
            m.push_str("\n```\n");
        }
        m.push_str("\n#### Log\n\n");
        if self.log.is_empty() {
            m.push_str("The container left no log.\n");
        } else {
            m.push_str("```\n");
            m.push_str(&self.log.join("\n"));
            m.push_str("\n```\n");
        }
        m
    }

    /// The links to search for this failure and to file it, and the title to use.
    pub(crate) fn links(&self) -> Value {
        if self.repo.is_empty() {
            return json!({"search": null, "new_issue": null, "title": self.title(), "body_in_link": false});
        }
        let base = format!("https://github.com/{}/issues", self.repo);
        let search = url::Url::parse_with_params(&base, [("q", format!("is:issue \"{}\"", self.id))])
            .map(String::from)
            .unwrap_or_default();
        let new = format!("{base}/new");
        let versions = format!(
            "{} → {}",
            Self::version(&self.old_version, &self.old_image),
            Self::version(&self.new_version, &self.new_image)
        );
        let mut params = vec![
            ("template", TEMPLATE.to_owned()),
            ("title", self.title()),
            ("fingerprint", self.id.clone()),
            ("app", format!("{} ({})", self.label, self.kind)),
            ("versions", versions),
        ];
        let short = url::Url::parse_with_params(&new, &params).map(String::from).unwrap_or_default();
        params.push(("report", self.markdown()));
        let full = url::Url::parse_with_params(&new, &params).map(String::from).unwrap_or_default();
        let body_in_link = full.len() <= URL_MAX;
        json!({
            "search": search,
            "new_issue": if body_in_link { full } else { short },
            "title": self.title(),
            "body_in_link": body_in_link,
        })
    }

    /// What a run's result and the Updates page show of it.
    pub(super) fn brief(&self) -> Value {
        let l = self.links();
        json!({"key": self.key, "id": self.id, "title": l["title"], "search": l["search"], "state": self.state})
    }

    /// The report for the API: itself, its Markdown and links, and how often
    /// the same failure is among the kept ones (`all`).
    pub(crate) fn view(&self, all: &[Report]) -> Value {
        let same: Vec<&Report> = all.iter().filter(|r| r.id == self.id).collect();
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        v["title"] = json!(self.title());
        v["outcome"] = json!(self.outcome());
        v["markdown"] = json!(self.markdown());
        v["links"] = self.links();
        v["seen"] = json!(same.len().max(1));
        v["first_seen"] = json!(same.iter().map(|r| r.at.as_str()).min().unwrap_or(&self.at));
        v
    }
}

/// Decode strictly inside a write transaction. A malformed value must not be
/// treated as an absent list and overwritten by the next save or deletion.
fn stored_reports(tx: &crate::state::Tx<'_>) -> crate::error::Result<Vec<Report>> {
    match tx.get_value(KEY)? {
        None => Ok(vec![]),
        Some(value) => serde_json::from_value(value)
            .map_err(|e| crate::error::Error::internal(format!("stored failure reports are malformed: {e}"))),
    }
}

// ---- keeping them --------------------------------------------------------------

impl Updater {
    /// What to take out of a report: every key and password wranglarr knows,
    /// and your domains.
    pub(super) async fn redactor(&self) -> Redactor {
        let cfg = self.app.cfg();
        let mut secrets: Vec<String> = Vec::new();
        let mut domains: Vec<String> = Vec::new();
        let host_of = |u: &str| url::Url::parse(u).ok().and_then(|u| u.host_str().map(str::to_owned));
        for a in cfg.all_apps() {
            secrets.extend([a.api_key.clone(), a.password.clone(), a.username.clone()]);
            if let Ok(k) = crate::service::api_key(a).await {
                secrets.push(k);
            }
            domains.extend(host_of(&a.url));
        }
        secrets.push(cfg.postgres.password.clone());
        domains.push(cfg.postgres.host.clone());
        if let Some(v) = &cfg.vpn {
            secrets.push(v.api_key.clone());
            domains.extend(host_of(&v.control_url));
        }
        domains.push(self.app.proxy().settings().await.domain);
        Redactor::new(secrets, domains)
    }

    pub(super) async fn reports(&self) -> Vec<Report> {
        self.app.state().get::<Vec<Report>>(KEY).await.ok().flatten().unwrap_or_default()
    }

    /// Keep `r` at the top: each container's failure in each run has its own,
    /// however many share its fingerprint.
    pub(super) async fn keep_report(&self, r: Report) -> crate::error::Result<Report> {
        let saved = r.clone();
        self.app
            .state()
            .write(move |tx| {
                let mut all = stored_reports(tx)?;
                all.retain(|x| x.key != saved.key);
                all.insert(0, saved);
                all.truncate(KEEP);
                tx.put(KEY, &all)
            })
            .await?;
        Ok(r)
    }

    /// Make and keep the report of `t`'s failed update, and say in the run's
    /// log what it left behind and where to report it.
    pub(super) async fn report_failure(
        &self,
        rl: &RunLog,
        t: &Target,
        facts: &SwapFacts,
        error: &str,
        captured: Option<&Captured>,
        outcome: &Outcome,
    ) {
        let kind = t.kind();
        let def = crate::service::get(&kind);
        let label = def.map_or_else(|| t.container.clone(), |d| d.label.to_owned());
        let report_to = match (def, t.role) {
            (Some(d), _) => d.report_to(),
            // somebody else's image: its own repo, if it names one
            (None, "extra") => self.source_of(&facts.new_image).await,
            _ => ReportTo::wranglarr(),
        };
        let f = Failure {
            container: &t.container,
            kind: &kind,
            label: &label,
            facts,
            error,
            captured,
            outcome,
            report_to,
            run_id: rl.run_id,
        };
        let r = Report::new(&f, &self.redactor().await, &crate::time::now_iso());
        let name = &t.container;
        let new = Report::version(&r.new_version, &r.new_image);
        rl.append(format!("{name}: the update of {label} to {new} failed. {}", r.outcome())).await;
        let r = match self.keep_report(r).await {
            Ok(r) => r,
            Err(e) => {
                rl.append(format!("{}: could not save the failure report: {e}", t.container)).await;
                return;
            }
        };
        let l = r.links();
        let page = format!("/updates/report/{}", r.key);
        match l["search"].as_str() {
            Some(search) => {
                rl.append(format!(
                    "{name}: a report ({}) is at {page}: check there whether it's been reported, and report it if not",
                    r.id
                ))
                .await;
                rl.append(format!("{name}: or search for it by hand: {search}")).await;
            }
            None => {
                rl.append(format!(
                    "{name}: a report ({}) is at {page}; its image doesn't say where to report it",
                    r.id
                ))
                .await;
            }
        }
    }

    /// Where an image that isn't one of wranglarr's says it comes from: its
    /// `org.opencontainers.image.source` label, when that's on GitHub.
    async fn source_of(&self, image_id: &str) -> ReportTo {
        let nowhere = ReportTo { repo: String::new(), curated: false };
        let Ok(d) = self.docker() else { return nowhere };
        let Ok(img) = d.image(image_id).await else { return nowhere };
        let src = img["Config"]["Labels"]["org.opencontainers.image.source"].as_str().unwrap_or("");
        github_repo(src).map_or(nowhere, |repo| ReportTo { repo, curated: false })
    }

    /// The kept reports, newest first, for the API.
    pub(crate) async fn report_views(&self) -> Vec<Value> {
        let all = self.reports().await;
        all.iter().map(|r| r.view(&all)).collect()
    }

    pub(crate) async fn report_view(&self, key: &str) -> Option<Value> {
        let all = self.reports().await;
        all.iter().find(|r| r.key == key).map(|r| r.view(&all))
    }

    /// Forget a report: it was filed, or it doesn't matter any more.
    pub(crate) async fn forget_report(&self, key: &str) -> crate::error::Result<bool> {
        let key = key.to_owned();
        self.app
            .state()
            .write(move |tx| {
                let mut all = stored_reports(tx)?;
                let before = all.len();
                all.retain(|r| r.key != key);
                if all.len() == before {
                    return Ok(false);
                }
                tx.put(KEY, &all)?;
                Ok(true)
            })
            .await
    }

    /// Whether report `key`'s failure has been filed: the oldest issue in its
    /// repo, open or closed, with its fingerprint. Asked from here rather
    /// than from the browser, which would tell GitHub your wranglarr's address.
    pub(crate) async fn reported(&self, key: &str) -> crate::error::Result<Value> {
        use crate::error::Error;
        let r = self.reports().await.into_iter().find(|r| r.key == key);
        let r = r.ok_or_else(|| Error::not_found(format!("no failure report {key}")))?;
        if r.repo.is_empty() {
            return Err(Error::conflict(format!("{}'s image doesn't say where it's reported", r.label)));
        }
        let c = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent(format!("wranglarr/{}", crate::VERSION))
            .build()
            .map_err(|e| Error::internal(e.to_string()))?;
        let q = format!("repo:{} is:issue \"{}\"", r.repo, r.id);
        let net = |e: reqwest::Error| Error::upstream(format!("couldn't ask GitHub: {}", e.without_url()));
        let resp = c
            .get(format!("{}/search/issues", github_api()))
            .query(&[("q", q.as_str()), ("sort", "created"), ("order", "asc"), ("per_page", "10")])
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(net)?;
        match resp.status().as_u16() {
            403 | 429 => return Err(Error::upstream("GitHub is limiting searches: try again in a minute")),
            s if !(200..300).contains(&s) => return Err(Error::upstream(format!("GitHub answered {s}"))),
            _ => {}
        }
        let v: Value = resp.json().await.map_err(net)?;
        Ok(json!({"issue": first_issue(&v, &r.repo)}))
    }

    /// The reports a run made: container -> {key, id, title, search}.
    pub(super) async fn run_reports(&self, run_id: i64) -> Map<String, Value> {
        self.reports().await.into_iter().filter(|r| r.run_id == run_id).map(|r| (r.target.clone(), r.brief())).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[tokio::test]
    async fn a_failed_report_save_keeps_recovery_instructions_in_the_run_log() {
        let (_dir, app) = crate::app::tests::app().await;
        app.state().put(KEY, &json!({"malformed": true})).await.expect("seed");
        let jobs = crate::jobs::JobRunner::new(app.state().clone());
        let (finished, mut finished_rx) = tokio::sync::mpsc::unbounded_channel();
        jobs.start(move |run| {
            let _ = finished.send(run);
        });
        for state in [Recovery::NotStarted, Recovery::Unhealthy] {
            let updater = app.updates();
            let run = jobs
                .submit("media", "test", "postgres", HashSet::new(), move |rl| async move {
                    let target = Target::new("postgres", "postgres");
                    let facts = SwapFacts {
                        name: "postgres".into(),
                        reference: "postgres:17".into(),
                        old_image: "old".into(),
                        new_image: "new".into(),
                        old_version: "17.1".into(),
                        new_version: "17.2".into(),
                    };
                    let outcome = Outcome {
                        state,
                        blocked: true,
                        data: "Data needs inspection.".into(),
                        data_recovery: DataRecovery::default(),
                        recovery_phase: None,
                    };
                    updater.report_failure(&rl, &target, &facts, "update failed", None, &outcome).await;
                    Ok(crate::jobs::JobOutcome::ok(json!({})))
                })
                .await
                .expect("submit");
            let finished = tokio::time::timeout(std::time::Duration::from_secs(5), finished_rx.recv())
                .await
                .expect("finished")
                .expect("worker alive");
            assert_eq!(finished.run_id, run);
            let log = app.state().get_run(run).await.expect("run").expect("exists").log;
            let instruction = match state {
                Recovery::NotStarted => "couldn't be started: start it by hand",
                _ => "didn't come up healthy: check it",
            };
            let outcome = log.find(instruction).expect("recovery instructions retained");
            let save_error = log.find("could not save the failure report").expect("save failure logged");
            assert!(outcome < save_error, "{log}");
            assert!(log.contains("Data needs inspection."), "{log}");
            assert!(!log.contains("is at /updates/report/"), "{log}");
        }
        jobs.begin_shutdown();
        jobs.join().await.expect("worker shutdown");
        app.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn concurrent_report_save_and_delete_preserve_both_changes() {
        use std::future::{Future, poll_fn};
        use std::task::Poll;

        let (_dir, app) = crate::app::tests::app().await;
        let updater = app.updates();
        for save_first in [true, false] {
            let old = Report { key: "old".into(), ..Report::default() };
            let new = Report { key: "new".into(), ..Report::default() };
            app.state().put(KEY, &vec![old]).await.expect("seed");
            let state = app.state().clone();
            let (entered, entered_rx) = tokio::sync::oneshot::channel();
            let (release, release_rx) = std::sync::mpsc::channel();
            let gate = tokio::spawn(async move {
                state
                    .call(move |_| {
                        let _ = entered.send(());
                        release_rx
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .map_err(|e| crate::error::Error::internal(e.to_string()))?;
                        Ok(())
                    })
                    .await
            });
            entered_rx.await.expect("state worker gated");
            let mut save = Box::pin(updater.keep_report(new.clone()));
            let mut delete = Box::pin(updater.forget_report("old"));
            // Queue both operations before either gets its first state reply.
            // Separate get/put calls would read the same stale list here.
            if save_first {
                assert!(poll_fn(|cx| Poll::Ready(save.as_mut().poll(cx).is_pending())).await);
                assert!(poll_fn(|cx| Poll::Ready(delete.as_mut().poll(cx).is_pending())).await);
            } else {
                assert!(poll_fn(|cx| Poll::Ready(delete.as_mut().poll(cx).is_pending())).await);
                assert!(poll_fn(|cx| Poll::Ready(save.as_mut().poll(cx).is_pending())).await);
            }
            release.send(()).expect("release");
            let (saved, deleted) = tokio::join!(save, delete);
            saved.expect("save");
            assert!(deleted.expect("delete"));
            gate.await.expect("worker task").expect("worker gate");
            assert_eq!(updater.reports().await, [new]);
        }
        app.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn report_mutations_do_not_overwrite_malformed_state() {
        let (_dir, app) = crate::app::tests::app().await;
        app.state().put(KEY, &json!([])).await.expect("seed");
        let updater = app.updates();
        for raw in [r#"{"not":"a report list"}"#, "{not json"] {
            app.state()
                .write(move |tx| {
                    tx.conn().execute("UPDATE kv SET value=? WHERE key=?", rusqlite::params![raw, KEY])?;
                    Ok(())
                })
                .await
                .expect("corrupt stored value");
            assert!(updater.keep_report(Report { key: "new".into(), ..Report::default() }).await.is_err());
            assert!(updater.forget_report("old").await.is_err());
            let actual: String = app
                .state()
                .call(|db| {
                    // Read the raw value inside the worker, bypassing JSON decoding.
                    db.write(|tx| Ok(tx.conn().query_row("SELECT value FROM kv WHERE key=?", [KEY], |r| r.get(0))?))
                })
                .await
                .expect("raw value");
            assert_eq!(actual, raw);
        }
        app.shutdown().await.expect("shutdown");
    }

    #[test]
    fn nothing_known_leaves_in_any_field_of_a_report() {
        let facts = SwapFacts {
            name: "radarr".into(),
            reference: "Registry.Home.Lan:5000/me/radarr:latest".into(),
            old_image: format!("sha256:{}", "a1".repeat(32)),
            new_image: format!("sha256:{}", "b2".repeat(32)),
            old_version: "5.14.0".into(),
            new_version: "5.15.0".into(),
        };
        let captured = Captured {
            log: "[Fatal] boom: password=\"two words\" Cookie: a=b; c=d; apikey=SECRETKEY123\n   at X.Y()\n".into(),
            state: json!({"ExitCode": 139, "OOMKilled": false,
                          "Health": {"Log": [{"Output": "curl http://radarr.mine.example.org/ping SECRETKEY123"}]}}),
        };
        let outcome = Outcome {
            state: Recovery::Parked,
            blocked: false,
            data: "used SECRETKEY123".into(),
            data_recovery: DataRecovery::default(),
            recovery_phase: None,
        };
        let f = Failure {
            container: "radarr",
            kind: "radarr",
            label: "Radarr",
            facts: &facts,
            error: "exited with code 139 (apikey=SECRETKEY123)",
            captured: Some(&captured),
            outcome: &outcome,
            report_to: ReportTo::wranglarr(),
            run_id: 7,
        };
        let red = Redactor::new(["SECRETKEY123".to_string()], ["mine.example.org".to_string()]);
        let rep = Report::new(&f, &red, "2026-10-29T00:00:00Z");
        assert_eq!(rep.key, "7-radarr");
        assert_eq!(rep.image, "[domain]:5000/me/radarr");
        let link = rep.links()["new_issue"].as_str().expect("link").to_owned();
        let decoded: String =
            url::Url::parse(&link).expect("url").query_pairs().map(|(k, v)| format!("{k}={v}\n")).collect();
        let stored = serde_json::to_string(&rep).expect("json");
        for text in [&rep.markdown(), &decoded, &stored] {
            for leak in ["SECRETKEY123", "two words", "c=d", "Registry.Home.Lan", "mine.example.org"] {
                assert!(!text.to_lowercase().contains(&leak.to_lowercase()), "{leak} in {text}");
            }
        }
        let after = rep.outcome();
        assert!(after.contains("parked as radarr-wranglarr-old") && after.contains("may be tried again"), "{after}");
        assert!(!after.contains("is back on") && !after.contains("skipped from now on"), "{after}");
    }

    #[test]
    fn links_go_to_the_repo_with_the_fingerprint() {
        let rep = Report {
            id: "wf-0123456789".into(),
            label: "Radarr".into(),
            kind: "radarr".into(),
            old_version: "5.14.0".into(),
            new_version: "5.15.0".into(),
            repo: "Wranglarrr/Wranglarr".into(),
            curated: true,
            ..Default::default()
        };
        let l = rep.links();
        let search = l["search"].as_str().expect("a search link");
        assert!(search.starts_with("https://github.com/Wranglarrr/Wranglarr/issues?q="), "{search}");
        assert!(search.contains("wf-0123456789"));
        let new = l["new_issue"].as_str().expect("a new-issue link");
        assert!(new.starts_with("https://github.com/Wranglarrr/Wranglarr/issues/new?template=update-failure.yml"));
        assert!(new.contains("fingerprint=wf-0123456789"));
        assert_eq!(l["title"], "Update failed: Radarr 5.14.0 → 5.15.0 [wf-0123456789]");
        assert_eq!(l["body_in_link"], true);
        let nowhere = Report { repo: String::new(), ..rep };
        assert!(nowhere.links()["search"].is_null());
    }
}
