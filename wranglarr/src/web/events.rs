// SPDX-License-Identifier: GPL-3.0-or-later
//! Live status over HTTP: the arrs' webhook (`POST /api/hooks/<app>`), the
//! stream the signed-in pages listen on (`GET /api/events/stream`), and the
//! System page's status and rewire (`/api/events`).

use std::convert::Infallible;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::app::App;
use crate::web::ApiResult;

/// A stream ends after this and the browser opens a new one, which signs in
/// again: a page left open stops getting events once its session is gone.
const LIFETIME: Duration = Duration::from_secs(30 * 60);

pub fn routes() -> Router<App> {
    Router::new()
        .route("/api/hooks/{app}", post(hook))
        .route("/api/events", get(status))
        .route("/api/events/wire", post(wire))
        .route("/api/events/stream", get(stream))
}

/// `POST /api/hooks/<app>`: an event from an arr. The login guard lets it
/// through without a session; the app's own token is checked here.
async fn hook(
    State(app): State<App>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("");
    // the arrs send JSON; anything else still counts as an event
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    app.receive_event(&name, auth, &body).await?;
    Ok(Json(json!({"ok": true})))
}

/// `GET /api/events`: per app, whether it's wired and what it sent last.
async fn status(State(app): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(app.events_status().await?))
}

/// `POST /api/events/wire`: put the webhook into every arr now.
async fn wire(State(app): State<App>) -> ApiResult<Json<Value>> {
    let results = app.wire_events(&app.event_apps()).await?;
    let mut out = app.events_status().await?;
    out["results"] = json!(results);
    Ok(Json(out))
}

/// `GET /api/events/stream`: Server-Sent Events, one JSON object each. The
/// first is `{"type":"hello"}`; after a reconnect the page reloads what it shows,
/// since it may have missed something meanwhile.
async fn stream(State(app): State<App>) -> Response {
    let (rx, closing) = app.subscribe_events();
    let until = tokio::time::Instant::now() + LIFETIME;
    let events = futures::stream::unfold((rx, closing, true), move |(mut rx, mut closing, first)| async move {
        if first {
            let hello = Event::default().retry(Duration::from_secs(5)).data(json!({"type": "hello"}).to_string());
            return Some((Ok::<_, Infallible>(hello), (rx, closing, false)));
        }
        if *closing.borrow() {
            return None;
        }
        let msg = tokio::select! {
            r = rx.recv() => match r {
                Ok(v) => v,
                Err(RecvError::Lagged(_)) => json!({"type": "resync"}),
                Err(RecvError::Closed) => return None,
            },
            _ = closing.changed() => return None,
            () = tokio::time::sleep_until(until) => return None,
        };
        Some((Ok(Event::default().data(msg.to_string())), (rx, closing, false)))
    });
    let mut r = Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(20))).into_response();
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    // nginx and friends would hold the events back otherwise
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    r
}
