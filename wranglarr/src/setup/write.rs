// SPDX-License-Identifier: GPL-3.0-or-later
//! What the wizard writes: the full setup (.env, wranglarr.toml, the restore
//! plan, configs from legacy backups) and the `--auth` change of the app-login
//! keys in an existing .env. Everything that can fail is checked before .env is
//! touched, and an existing .env is copied to `.env.bak-<timestamp>` first.

use std::collections::BTreeMap;
use std::fs::FileTimes;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use chrono::Local;
use serde::Deserialize;
use serde_json::{Value, json};

use super::env::{
    self, Answers, ENV_KEYS_SECRET, auth_env, auth_steps, check_auth, check_backup_prefs, check_values, next_steps,
    render_env, set_env_keys,
};
use super::host;
use super::input;
use super::legacy;
use super::scan::scan_backups;
use super::web::Wizard;
use crate::error::{Error, Result};
use crate::files::{Dir, NewFile, Owner};
use crate::restore_plan::{self, RestoreItem, RestorePlan};

/// `POST /api/setup/write-auth`.
#[derive(Deserialize)]
pub struct AuthWriteReq {
    pub answers: Answers,
}

/// `POST /api/setup/write`.
#[derive(Deserialize)]
pub struct WriteReq {
    pub answers: Answers,
    #[serde(default)]
    pub restore_versions: Vec<RestoreItem>,
    /// app -> zip path to take config.xml from
    #[serde(default)]
    pub legacy_configs: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "input::flag")]
    pub overwrite: bool,
}

/// Copy the stack's .env (never a link) to `name` beside it: private, with
/// the original's times. Returns the original's metadata.
fn back_up_env(stack: &Dir, name: &str) -> Result<std::fs::Metadata> {
    let (data, m) = stack.read(".env")?;
    let times = FileTimes::new().set_accessed(m.accessed()?).set_modified(m.modified()?);
    stack.write(name, &data, NewFile::private().times(times))?;
    Ok(m)
}

/// `.env.bak-<timestamp>`, next to .env.
fn backup_name() -> String {
    format!(".env.bak-{}", Local::now().format("%Y%m%d%H%M%S"))
}

impl Wizard {
    /// (the .env text, its values, the new login values) for `--auth`.
    fn auth_change(&self, a: &mut Answers) -> Result<(String, BTreeMap<String, String>, env::Pairs)> {
        let path = self.env_path();
        if !path.exists() {
            return Err(Error::conflict("no .env here yet: run the full setup (wranglarr-setup without --auth) first"));
        }
        let text = std::fs::read_to_string(&path)?;
        let old = env::parse_env(&text);
        self.unmask(a);
        a.services = old
            .get("COMPOSE_PROFILES")
            .map_or("", String::as_str)
            .split(',')
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect();
        check_values(a)?;
        check_auth(a)?;
        let new = auth_env(a, &old)?;
        Ok((text, old, new))
    }

    /// What `--auth` would change in .env, secrets masked.
    pub fn preview_auth(&self, mut a: Answers) -> Result<Value> {
        let (_, old, new) = self.auth_change(&mut a)?;
        let mask = |k: &str, v: &str| {
            if ENV_KEYS_SECRET.contains(&k) && !v.is_empty() { "••••••".to_string() } else { v.to_string() }
        };
        let changes: Vec<Value> = new
            .iter()
            .map(|(k, v)| {
                let o = old.get(*k).map_or("", String::as_str);
                json!({"key": k, "old": mask(k, o), "new": mask(k, v), "same": o == v})
            })
            .collect();
        Ok(json!({"changes": changes}))
    }

    /// `--auth`: write the app-login keys into the existing .env.
    pub fn write_auth(&self, mut a: Answers) -> Result<Value> {
        let (text, _, new) = self.auth_change(&mut a)?;
        let stack = Dir::open(&self.target)?;
        let text = set_env_keys(&text, &new)?;
        let bak = backup_name();
        let old = back_up_env(&stack, &bak)?;
        // the new .env keeps the old one's owner
        let owner = Owner { uid: Some(old.uid()), gid: Some(old.gid()) };
        stack.write(".env", text.as_bytes(), NewFile::private().owner(owner))?;
        let bak = self.target.join(bak);
        let written = vec![
            bak.to_string_lossy().replacen(&*self.target.to_string_lossy(), ".", 1),
            "./.env (app login keys only)".to_string(),
        ];
        Ok(json!({"written": written, "next_steps": auth_steps(&a, true)}))
    }

    /// The full setup: .env, wranglarr.toml, the restore plan and legacy configs.
    pub fn write(&self, req: WriteReq) -> Result<Value> {
        let mut a = req.answers;
        self.unmask(&mut a);
        check_values(&a)?;
        check_backup_prefs(&a)?;
        if a.services.is_empty() {
            return Err(Error::invalid("select at least one service"));
        }
        if a.postgres_password.is_empty() {
            return Err(Error::invalid("Postgres password is required"));
        }
        check_auth(&a)?;
        let stack = Dir::open(&self.target)?;
        let env_path = self.env_path();
        let had_env = stack.exists(".env")?;
        if had_env && !req.overwrite {
            return Err(Error::conflict(".env already exists; confirm overwrite"));
        }
        // everything that can fail is checked before .env is touched
        let text = render_env(&a, &self.env.image)?;
        let appdata = self.appdata_dir(&a.appdata)?;
        let appdata_real = host::resolve(&appdata);
        let mut legacy = Vec::with_capacity(req.legacy_configs.len());
        for (app_id, zpath) in &req.legacy_configs {
            legacy.push((app_id, zpath, legacy::open_zip(zpath, &appdata_real)?));
        }
        let owner = Owner::ids(a.puid, a.pgid);
        // appdata itself is where the admin pointed us; below it, no links
        let made = std::fs::create_dir_all(&appdata)
            .and_then(|()| Dir::open(&appdata))
            .and_then(|ad| Ok((ad.sub("wranglarr", Some((0o700, owner)))?, ad)));
        let (wdir, appdata_dir) = match made {
            Ok(d) => d,
            Err(e) => {
                return Err(Error::invalid(format!(
                    "cannot create {}/wranglarr ({e}); if APPDATA is outside the stack dir, add  -v {}:{}  to docker run",
                    a.appdata,
                    a.appdata,
                    self.env.mount("appdata").unwrap_or("")
                )));
            }
        };
        if rustix::fs::access(wdir.path(), rustix::fs::Access::WRITE_OK).is_err() {
            return Err(Error::invalid(format!("{}/wranglarr isn't writable by the wizard", a.appdata)));
        }

        let mut written: Vec<String> = vec![];
        if had_env {
            let bak = backup_name();
            back_up_env(&stack, &bak)?;
            written.push(self.target.join(bak).to_string_lossy().into_owned());
        }
        stack.write(".env", text.as_bytes(), NewFile::private().owner(owner))?;
        written.push(env_path.to_string_lossy().into_owned());

        appdata_dir.chown(owner);
        wdir.chown(owner);
        const TOML: &str = "wranglarr.toml";
        if !wdir.exists(TOML)? {
            let example = self.target.join("wranglarr").join("wranglarr.example.toml");
            if example.exists() {
                let toml = env::apply_backup_prefs(&std::fs::read_to_string(&example)?, &a);
                if wdir.write_new(TOML, toml.as_bytes(), NewFile::shared().owner(owner))? {
                    written.push(wdir.path().join(TOML).to_string_lossy().into_owned());
                }
            }
        }

        if !req.restore_versions.is_empty() {
            let plan = RestorePlan::from_wizard(&req.restore_versions);
            wdir.write(
                restore_plan::FILE,
                serde_json::to_string_pretty(&plan)?.as_bytes(),
                NewFile::shared().owner(owner),
            )?;
            written.push(wdir.path().join(restore_plan::FILE).to_string_lossy().into_owned());
        }

        for (app_id, zpath, mut zf) in legacy {
            let Some(cfg) = env::config_file(app_id) else { continue };
            let rel = Path::new(&app_id).join(cfg);
            let (Some(sub), Some(file)) = (rel.parent(), rel.file_name().and_then(|f| f.to_str())) else { continue };
            let dest = appdata.join(&rel);
            let folder = appdata_dir.sub(sub, Some((0o777, owner)))?;
            if folder.exists(file)? {
                continue; // never clobber a live config
            }
            if let Some((data, mode)) = legacy::member(&mut zf, zpath, &file.to_lowercase())? {
                if !folder.write_new(file, &data, NewFile::private().mode(mode).owner(owner))? {
                    continue;
                }
                folder.chown(owner);
                written.push(dest.to_string_lossy().into_owned());
            }
        }
        let scan = scan_backups(&appdata);
        let (t, ad) = (self.target.to_string_lossy(), appdata.to_string_lossy());
        let shown: Vec<String> = written
            .iter()
            .map(|w| w.replacen(&*t, ".", 1).replacen(&*ad, a.appdata.trim_end_matches('/'), 1))
            .collect();
        let steps = next_steps(&a, &self.env, &self.target, &scan, req.restore_versions.len());
        Ok(json!({"written": shown, "next_steps": steps}))
    }
}
