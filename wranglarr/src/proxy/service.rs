// SPDX-License-Identifier: GPL-3.0-or-later
//! The proxy page's view of the running app: saved settings, the stack's entries
//! (from the config and Docker), and the rendered answer.

use serde_json::{Map, Value};

use super::{Entry, KEY, ProxySettings, Rendered, check, plan, render, ungated, validate};
use crate::app::App;
use crate::config::AppConfig;
use crate::error::Result;
use crate::links::unique_apps;

/// The view of the service for the reverse proxy page: `app.proxy()`.
pub struct Proxy<'a>(&'a App);

impl App {
    pub fn proxy(&self) -> Proxy<'_> {
        Proxy(self)
    }
}

impl Proxy<'_> {
    pub async fn settings(&self) -> ProxySettings {
        self.0.state().get::<ProxySettings>(KEY).await.ok().flatten().unwrap_or_default()
    }

    /// Merge the body into the saved settings, check and save them.
    pub async fn save(&self, body: &Map<String, Value>) -> Result<ProxySettings> {
        self.save_configuration(body, None).await
    }

    /// The dashboard edits placement and browser links together: validate both and
    /// commit them in one transaction, including the proxy read-modify-write.
    pub async fn save_with_links(
        &self,
        body: &Map<String, Value>,
        links: &crate::links::LinkSettings,
    ) -> Result<ProxySettings> {
        self.save_configuration(body, Some(self.0.links().check(links)?)).await
    }

    async fn save_configuration(
        &self,
        body: &Map<String, Value>,
        links: Option<crate::links::LinkSettings>,
    ) -> Result<ProxySettings> {
        let body = body.clone();
        let entries: Vec<Entry> =
            std::iter::once(Entry::wranglarr()).chain(self.0.links().apps().into_iter().map(Entry::for_app)).collect();
        let names = entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        let cookie_domain = self.0.auth().options().cookie_domain;
        self.0
            .state()
            .write(move |tx| {
                let current = tx.get::<ProxySettings>(KEY)?.unwrap_or_default();
                let s = check(&current, &body, &names)?;
                let effective = crate::links::proxy_for_links(s.clone(), &cookie_domain);
                validate(&entries, &effective)?;
                tx.put(KEY, &s)?;
                if let Some(links) = links {
                    tx.put(crate::links::KEY, &links)?;
                }
                Ok(s)
            })
            .await
    }

    /// wranglarr and every app: name, kind, inner port, service host on the Docker network.
    pub async fn entries(&self, tunnel: bool) -> Vec<Entry> {
        let mut out = vec![Entry::wranglarr()];
        let docker_up = self.0.docker_ok().await;
        for a in unique_apps(self.0.cfg().all_apps()) {
            let mut e = Entry::for_app(a);
            // the labels only go on the app's own service when it isn't behind the gate
            if docker_up && (!tunnel || ungated(&a.kind)) {
                e.compose_service = self.compose_service(a, &e.service).await;
            }
            out.push(e);
        }
        out
    }

    /// The compose service the labels go on: the app's, or the VPN's when it shares its network.
    async fn compose_service(&self, a: &AppConfig, service: &str) -> String {
        let Some(d) = self.0.docker() else { return service.to_string() };
        let container = if a.container.is_empty() { &a.name } else { &a.container };
        let Ok(mut info) = d.inspect(container).await else { return service.to_string() };
        if let Some(other) = info["HostConfig"]["NetworkMode"].as_str().and_then(|m| m.strip_prefix("container:")) {
            match d.inspect(other).await {
                Ok(i) => info = i,
                Err(_) => return service.to_string(),
            }
        }
        info["Config"]["Labels"]["com.docker.compose.service"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(service)
            .to_string()
    }

    /// The config for these settings (AuthTunnel's cookie domain when they name no
    /// domain), for a browser at `browser_host`.
    pub async fn render(&self, s: &ProxySettings, browser_host: &str) -> Result<Rendered> {
        let env = &self.0.deployment().proxy;
        let s = &crate::links::proxy_for_links(s.clone(), &self.0.auth().options().cookie_domain);
        let entries = self.entries(env.apps_auth == "tunnel").await;
        let ports = self.0.links().published(false).await;
        render(plan(&entries, s, browser_host, &ports, env), s, env)
    }

    /// `{"settings": …, "text", "filename", "how", "plan", "notes"}`, as the API answers.
    pub async fn answer(&self, s: &ProxySettings, browser_host: &str) -> Result<Value> {
        let r = match self.render(s, browser_host).await {
            Ok(r) => r,
            // A previously saved configuration can become unsupported after the
            // app inventory changes. Keep the editor usable, but emit no scaffold.
            Err(e) => {
                return Ok(serde_json::json!({"settings": s, "valid": false, "text": "", "plan": [],
                "filename": "Configuration needs attention", "how": "Correct placement under Dashboard → Links, or choose another proxy.",
                "notes": [{"cls": "warn", "msg": e.to_string()}]}));
            }
        };
        let mut v = serde_json::to_value(&r)?;
        if let Some(o) = v.as_object_mut() {
            o.insert("settings".into(), serde_json::to_value(s)?);
            o.insert("valid".into(), true.into());
        }
        Ok(v)
    }
}
