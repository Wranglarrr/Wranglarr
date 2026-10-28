// SPDX-License-Identifier: GPL-3.0-or-later
//! The arrs' sign-in: `config/host` (Forms, or External behind the gate).

use crate::arr::ArrClient;
use crate::login::{
    LoginApi, LoginSource, LoginState, Managed, READ_TIMEOUT, SET_TIMEOUT, Wanted, base, http_err, key, required_text,
    set_http, str_or_empty,
};
use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use wranglarr_core::config::AppConfig;
use wranglarr_core::value::{text, truthy};

pub(super) static LOGIN: Managed =
    Managed { source: LoginSource::Api, ready_path: "/ping", init_checks_username: false, api: &ArrLogin };

struct ArrLogin;

async fn client(a: &AppConfig) -> Result<ArrClient, String> {
    let key = crate::api_key(a).await.map_err(|e| e.to_string())?;
    ArrClient::new(&a.name, &a.kind, &a.url, &key, None).map_err(|e| e.to_string())
}

#[async_trait]
impl LoginApi for ArrLogin {
    async fn read(&self, a: &AppConfig) -> Result<LoginState, String> {
        let h = client(a)
            .await?
            .call(Method::GET, "config/host", None, Some(READ_TIMEOUT))
            .await
            .map_err(|e| e.to_string())?;
        let m = if truthy(&h["authenticationMethod"]) { text(&h["authenticationMethod"]) } else { "none".into() };
        Ok(LoginState {
            required: h.get("authenticationRequired").and_then(Value::as_str).map(String::from),
            ..LoginState::new(m.to_lowercase(), str_or_empty(&h["username"]))
        })
    }

    async fn set(&self, a: &AppConfig, want: Wanted<'_>) -> Result<String, String> {
        let c = client(a).await?;
        let mut h = c.get("config/host").await.map_err(|e| e.to_string())?;
        let Some(o) = h.as_object_mut() else { return Err(format!("{}: config/host is not an object", a.name)) };
        match want {
            Wanted::Tunnel => {
                o.insert("authenticationMethod".into(), json!("external"));
                o.insert("authenticationRequired".into(), json!("enabled"));
            }
            Wanted::Apps { user, password, required } => {
                o.insert("authenticationMethod".into(), json!("forms"));
                o.insert("authenticationRequired".into(), json!(required));
                o.insert("username".into(), json!(user));
                o.insert("password".into(), json!(password));
                o.insert("passwordConfirmation".into(), json!(password));
            }
        }
        let id = text(key(&h, "id")?);
        c.put(&format!("config/host/{id}"), &h).await.map_err(|e| e.to_string())?;
        let got = c.get("config/host").await.map_err(|e| e.to_string())?;
        let Wanted::Apps { user, password, required } = want else {
            let m = got.get("authenticationMethod").filter(|v| truthy(v)).map(text).unwrap_or_default();
            if m.to_lowercase() != "external" {
                return Err(format!(
                    "still {}, not External",
                    got.get("authenticationMethod").map_or("None".into(), text)
                ));
            }
            return Ok("External: no login of its own, the gate signs you in".into());
        };
        let r = set_http()
            .post(format!("{}/login", base(a)))
            .query(&[("returnUrl", "/")])
            .form(&[("username", user), ("password", password), ("rememberMe", "on")])
            .timeout(SET_TIMEOUT)
            .send()
            .await
            .map_err(http_err)?;
        let loc = r.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("");
        if r.status() != StatusCode::FOUND || loc.contains("loginFailed") {
            return Err("saved, but signing in with it didn't work".into());
        }
        Ok(format!("login page, user {user}, asked {}: signed in with it", required_text(required).unwrap_or("")))
    }
}
