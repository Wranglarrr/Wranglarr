// SPDX-License-Identifier: GPL-3.0-or-later
//! The restore stack's `RESTORE.md`.

use serde_json::Value;

use super::plan::Stack;
use super::{NETWORK, image_ref};
use crate::service::{AppConfigExt, Roles};

/// `RESTORE.md`: what the folder holds and how to bring it up.
pub(super) fn readme(st: &Stack) -> String {
    let (manifest, suite, pg_host, n_dumps) = (st.manifest, st.suite, st.pg_host, st.dumps.len());
    let has_pg = st.postgres.is_some();
    let vpn_used = st.vpn_used || !st.own_vpn.is_empty();
    let gated = &st.gated;
    let mut rows = vec![];
    for sa in &st.apps {
        let (a, e) = (sa.app, sa.entry);
        let img = image_ref(&a.kind, e.container.as_ref());
        let dbs: Vec<String> = e.databases.iter().map(|d| format!("`{}`", d.name)).collect();
        let ver = match e.version.as_ref().and_then(Value::as_str).unwrap_or("") {
            "" => "?",
            v => v,
        };
        rows.push(format!(
            "| {} | {ver} | `{img}` | `{}/config/` ({} files) | {} |",
            a.name,
            a.name,
            e.config_snapshot.as_ref().map_or(0, |s| s.files),
            if dbs.is_empty() { "–".to_string() } else { dbs.join(", ") }
        ));
    }
    let v = manifest.version.as_str();
    let created = match manifest.created.as_deref().unwrap_or("") {
        "" => v,
        c => c,
    };
    let name = &suite.name;
    let rows = rows.join("\n");
    let mut out = format!(
        r#"# Restoring {name} @ {v} without wranglarr

This folder is a complete, runnable copy of suite **{name}** as it was at
{created}. Nothing here needs wranglarr.

| App | Version | Image | Config | Databases |
| --- | --- | --- | --- | --- |
{rows}

## Quick start

```sh
cp -a "{v}" ~/arrs-restored      # work on a copy: the apps write into their config folders
cd ~/arrs-restored
sha256sum -c */config.sha256 --quiet && echo "configs intact"
$EDITOR .env                      # check the data paths and ports
docker compose up -d              # Podman: podman compose up -d
```
"#
    );
    if has_pg {
        out += &format!(
            r#"
On its first start the `{pg_host}` container runs `postgres/initdb/10-restore.sh`,
which creates and fills {n_dumps} database(s) from the `.dump` files here
(`docker compose logs -f {pg_host}` shows progress). The apps wait until it
is done. `POSTGRES_PASSWORD` in `.env` must stay what the apps' configs
(`config.xml` / Bazarr's environment) expect. It is the one in use when the
backup ran.

To redo the restore from scratch: `docker compose down`, delete
`postgres/data`, `docker compose up -d`.
"#
        );
    } else {
        out += &format!(
            r#"
This suite has no databases. Its apps join the `{NETWORK}` network created by
the suite that has Postgres. Start that one first, or
`docker network create {NETWORK}`.
"#
        );
    }
    if !gated.is_empty() {
        out += &format!(
            r#"
**Logins:** these apps ran behind AuthTunnel, so they have no login of their own:
{}. This stack publishes their web pages on this machine only
(`127.0.0.1:…` under `ports:` in `compose.yaml`). Open each one, set a login
(Settings → General → Authentication in the arrs), then drop the `127.0.0.1:`
to reach it from other machines.
"#,
            gated.join(", ")
        );
    }
    if vpn_used && st.apps.iter().any(|a| a.app.has(Roles::DOWNLOADS)) {
        out += r#"
**VPN:** the download clients ran behind a VPN. This stack does not recreate the
tunnel, so they are in the `downloads` profile and don't start by default.
Run `docker compose --profile downloads up -d` only if they may use your
connection directly.
"#;
    }
    out += r#"
## Without compose

Each `<app>/config/` folder is that app's `/config` exactly as the app lays it
out (logs, caches, poster art and live database files left out). Mount it as
`/config` on the same image:

```sh
docker run -d --name radarr -p 7878:7878 -e PUID=1000 -e PGID=1000 \
  -v "$PWD/radarr/config:/config" -v /mnt/data:/data lscr.io/linuxserver/radarr:latest
```

Database dumps are standard `pg_dump -Fc` files:

```sh
createdb -U arr radarr
pg_restore -U arr -d radarr --no-owner --no-acl radarr/radarr.dump
```

The arrs' `config.xml` points at the Postgres host by name (`PostgresHost`),
so the database container has to be reachable under that name.

`manifest.json` has the full inventory: versions, collection size, checksums.
"#;
    out
}
