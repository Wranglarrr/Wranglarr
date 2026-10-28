// SPDX-License-Identifier: GPL-3.0-or-later
//! The proxy page's view of the running app: saved settings, the stack's entries
//! (from the config and Docker), and the rendered answer.

use serde_json::{Map, Value};

use super::{Entry, KEY, ProxySettings, Rendered, check, plan, render, ungated};
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

    fn names(&self) -> Vec<String> {
        std::iter::once("wranglarr".to_string()).chain(self.0.links().apps().iter().map(|a| a.name.clone())).collect()
    }

    /// Merge the body into the saved settings, check and save them.
    pub async fn save(&self, body: &Map<String, Value>) -> Result<ProxySettings> {
        let s = check(&self.settings().await, body, &self.names())?;
        self.0.state().put(KEY, &s).await?;
        Ok(s)
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

    /// The config for these settings, for a browser at `browser_host`.
    pub async fn render(&self, s: &ProxySettings, browser_host: &str) -> Rendered {
        let env = &self.0.deployment().proxy;
        let entries = self.entries(env.apps_auth == "tunnel").await;
        let ports = self.0.links().published(false).await;
        render(plan(&entries, s, browser_host, &ports, env), s, env)
    }

    /// `{"settings": …, "text", "filename", "how", "plan", "notes"}`, as the API answers.
    pub async fn answer(&self, s: &ProxySettings, browser_host: &str) -> Result<Value> {
        let r = self.render(s, browser_host).await;
        let mut v = serde_json::to_value(&r)?;
        if let Some(o) = v.as_object_mut() {
            o.insert("settings".into(), serde_json::to_value(s)?);
        }
        Ok(v)
    }
}
