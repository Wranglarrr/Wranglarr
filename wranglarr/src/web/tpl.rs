// SPDX-License-Identifier: GPL-3.0-or-later
//! The Jinja templates, rendered with minijinja, plus the filters and globals they use.

use std::sync::LazyLock;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use minijinja::value::Kwargs;
use minijinja::{Environment, Error as TplError, ErrorKind, State, Value};
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};

use crate::format::{html_escape, quote};
use crate::store::parse_version;

#[derive(RustEmbed)]
#[folder = "templates/"]
struct Templates;

#[derive(RustEmbed)]
#[folder = "static/"]
pub struct Static;

pub fn template_names() -> Vec<String> {
    Templates::iter().map(|n| n.into_owned()).collect()
}

fn loader(name: &str) -> Result<Option<String>, TplError> {
    match Templates::get(name) {
        Some(f) => String::from_utf8(f.data.into_owned())
            .map(Some)
            .map_err(|e| TplError::new(ErrorKind::InvalidOperation, format!("{name}: {e}"))),
        None => Ok(None),
    }
}

// ---- filters ----------------------------------------------------------------

fn as_f64(v: &Value) -> Option<f64> {
    if v.is_none() || v.is_undefined() {
        return Some(0.0);
    }
    if let Some(s) = v.as_str() {
        return s.trim().parse().ok();
    }
    f64::try_from(v.clone()).ok()
}

pub fn human_bytes(v: &Value) -> String {
    as_f64(v).map_or_else(|| "?".into(), crate::format::bytes)
}

/// A timestamp: RFC 3339, a version id (`20260101T030000Z`), or
/// `YYYY-MM-DD[ HH:MM[:SS[.fraction]]]` with a `T` or a space and an optional
/// offset. One without a zone is local time.
pub fn parse_ts(s: &str) -> Option<DateTime<Local>> {
    if s.len() == 16 && s.ends_with('Z') && s.contains('T') {
        return parse_version(s).map(|d| d.with_timezone(&Local));
    }
    let z = s.replace('Z', "+00:00");
    if let Ok(d) = DateTime::parse_from_rfc3339(&z) {
        return Some(d.with_timezone(&Local));
    }
    for f in ["%Y-%m-%dT%H:%M:%S%.f%:z", "%Y-%m-%d %H:%M:%S%.f%:z", "%Y-%m-%dT%H:%M%:z", "%Y-%m-%d %H:%M%:z"] {
        if let Ok(d) = DateTime::parse_from_str(&z, f) {
            return Some(d.with_timezone(&Local));
        }
    }
    for f in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
            return Local.from_local_datetime(&n).earliest();
        }
    }
    let n = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?.and_hms_opt(0, 0, 0)?;
    Local.from_local_datetime(&n).earliest()
}

pub fn fmt_ts(v: &Value) -> String {
    if !v.is_true() {
        return "–".into();
    }
    match v.as_str() {
        Some(s) => parse_ts(s).map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|| s.to_string()),
        None => v.to_string(),
    }
}

/// When a version (`20260101T030000Z`, maybe with `-1`) was made.
pub fn version_dt(version: &str) -> Option<DateTime<Utc>> {
    parse_version(version.split('-').next().unwrap_or(""))
}

pub fn age_text(at: DateTime<Utc>) -> String {
    let mut s = (Utc::now() - at).num_milliseconds() as f64 / 1000.0;
    if s < 90.0 {
        return "just now".into();
    }
    for (div, unit, limit) in [(60.0, "min", 60.0), (60.0, "h", 48.0), (24.0, "d", 14.0), (7.0, "w", 1e9)] {
        s /= div;
        if s < limit {
            return format!("{s:.0} {unit} ago");
        }
    }
    "long ago".into()
}

/// How long ago a backup version (or an RFC 3339 time) was.
pub fn human_age(v: &Value) -> String {
    let rfc = |s: &str| DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc));
    match v.as_str().filter(|s| s.len() >= 16).and_then(|s| version_dt(s).or_else(|| rfc(s))) {
        Some(d) => age_text(d),
        None => "–".into(),
    }
}

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// The common cron shapes in words ("daily at 03:30"); anything else comes back as it is.
pub fn cron_text(expr: &str) -> String {
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 {
        return if expr.is_empty() { "never".into() } else { expr.into() };
    }
    let (m, h, dom, mon, dow) = (f[0], f[1], f[2], f[3], f[4]);
    if !(digits(m) && dom == "*" && mon == "*") {
        return expr.into();
    }
    let mi: u32 = m.parse().unwrap_or(0);
    if let Some(every) = h.strip_prefix("*/").filter(|e| digits(e))
        && dow == "*"
    {
        return format!("every {every} hours at :{mi:02}");
    }
    if !digits(h) {
        return expr.into();
    }
    let at = format!("{:02}:{mi:02}", h.parse::<u32>().unwrap_or(0));
    if dow == "*" {
        return format!("daily at {at}");
    }
    if digits(dow)
        && let Ok(d @ 0..=7) = dow.parse::<usize>()
    {
        return format!("{}s at {at}", DAYS[d % 7]);
    }
    if dow == "1-5" || dow == "mon-fri" {
        return format!("weekdays at {at}");
    }
    expr.into()
}

fn cron_filter(v: &Value) -> String {
    if v.is_none() || v.is_undefined() {
        return "never".into();
    }
    cron_text(v.as_str().unwrap_or(""))
}

/// A number with thousands separators: `12,345`.
pub fn thousands(v: &Value) -> String {
    let text = match i64::try_from(v.clone()) {
        Ok(i) => i.to_string(),
        Err(_) => v.to_string(),
    };
    let (sign, rest) = text.strip_prefix('-').map_or(("", text.as_str()), |r| ("-", r));
    let (int, frac) = rest.split_once('.').map_or((rest, None), |(a, b)| (a, Some(b)));
    if !digits(int) {
        return text;
    }
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    match frac {
        Some(f) => format!("{sign}{out}.{f}"),
        None => format!("{sign}{out}"),
    }
}

// ---- globals ----------------------------------------------------------------

pub use crate::service::{COLLECTION_LABELS, SCOPE_LABELS};

/// A kind's shipped icon.
pub fn kind_icon(kind: &str) -> Option<String> {
    crate::service::get(kind).map(|d| d.icon())
}

/// The collection count shown as a kind's headline.
pub fn primary_count(kind: &str) -> Option<&'static str> {
    crate::service::get(kind).map(|d| d.primary_count)
}

pub fn lookup(table: &[(&str, &'static str)], key: &str) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn table(t: &[(&str, &str)]) -> Value {
    Value::from(t.iter().map(|(k, v)| (k.to_string(), Value::from(*v))).collect::<std::collections::BTreeMap<_, _>>())
}

/// An app's own logo (asked from the app), its kind's shipped one if that fails.
fn app_logo(state: &State, name: String, kind: Option<String>, cls: Option<String>, _kw: Kwargs) -> Value {
    let cls = cls.unwrap_or_else(|| "mini-logo".into());
    let base = state.lookup("base").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let fallback = crate::url_base::prefix(&base, &kind.as_deref().and_then(kind_icon).unwrap_or_default());
    Value::from_safe_string(format!(
        r#"<img class="{}" src="{}/api/apps/{}/logo" alt="" title="{}" loading="lazy" data-fallback="{}">"#,
        html_escape(&cls),
        html_escape(&base),
        quote(&name),
        html_escape(&name),
        html_escape(&fallback)
    ))
}

/// Changes whenever a static file does, so browsers don't keep old copies.
pub static ASSET_V: LazyLock<String> = LazyLock::new(|| {
    let mut names: Vec<String> = Static::iter().map(|n| n.into_owned()).collect();
    names.sort();
    let mut h = Sha256::new();
    for n in names {
        if let Some(f) = Static::get(&n) {
            h.update(n.as_bytes());
            h.update(f.metadata.sha256_hash());
        }
    }
    hex::encode(h.finalize())[..10].to_string()
});

/// HTML escaping of `& < > " '`. minijinja's default also escapes `/`, which would
/// write the paths in links and data attributes as `&#x2f;`.
fn html_formatter(out: &mut minijinja::Output, state: &mut State, value: &Value) -> Result<(), TplError> {
    if matches!(state.auto_escape(), minijinja::AutoEscape::Html)
        && !value.is_safe()
        && let Some(s) = value.as_str()
    {
        return out.write_str(&html_escape(s)).map_err(|e| TplError::new(ErrorKind::WriteFailure, e.to_string()));
    }
    minijinja::escape_formatter(out, state, value)
}

pub fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_loader(loader);
    env.add_filter("thousands", |v: Value| thousands(&v));
    env.set_formatter(html_formatter);
    env.add_filter("human_bytes", |v: Value| human_bytes(&v));
    env.add_filter("fmt_ts", |v: Value| fmt_ts(&v));
    env.add_filter("human_age", |v: Value| human_age(&v));
    env.add_filter("cron_text", |v: Value| cron_filter(&v));
    env.add_global("version", crate::VERSION);
    env.add_global("base", "");
    env.add_global("RELEASE_NOTES", crate::updates::RELEASE_NOTES);
    env.add_global("asset_v", ASSET_V.as_str());
    env.add_global("COLLECTION_LABELS", table(COLLECTION_LABELS));
    env.add_global("SCOPE_LABELS", table(SCOPE_LABELS));
    let services = crate::service::ALL;
    env.add_global(
        "PRIMARY_COUNT",
        Value::from(services.iter().map(|d| (d.id, d.primary_count)).collect::<std::collections::BTreeMap<_, _>>()),
    );
    env.add_global(
        "KIND_ICON",
        Value::from(services.iter().map(|d| (d.id, d.icon())).collect::<std::collections::BTreeMap<_, _>>()),
    );
    env.add_global("SERVICES", Value::from(minijinja::value::Serde(crate::service::views())));
    env.add_function("app_logo", app_logo);
    env
}

pub static ENV: LazyLock<Environment<'static>> = LazyLock::new(environment);

/// Render a template; a failure is a bug, so it becomes a 500.
pub fn render(name: &str, ctx: Value) -> crate::error::Result<String> {
    let t = ENV.get_template(name).map_err(|e| crate::error::Error::internal(format!("template {name}: {e:#}")))?;
    t.render(ctx).map_err(|e| crate::error::Error::internal(format!("template {name}: {e:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_compiles() {
        let env = environment();
        let mut bad = vec![];
        for n in template_names() {
            if let Err(e) = env.get_template(&n) {
                bad.push(format!("{n}: {e:#}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n\n"));
    }

    #[test]
    fn filters() {
        assert_eq!(human_bytes(&Value::from(0)), "0 B");
        assert_eq!(human_bytes(&Value::from(())), "0 B");
        assert_eq!(human_bytes(&Value::from(1536)), "1.5 KB");
        assert_eq!(human_bytes(&Value::from("x")), "?");
        assert_eq!(cron_text("30 3 * * *"), "daily at 03:30");
        assert_eq!(cron_text("0 3 * * 0"), "Sundays at 03:00");
        assert_eq!(cron_text("0 3 * * 7"), "Sundays at 03:00");
        assert_eq!(cron_text("5 */6 * * *"), "every 6 hours at :05");
        assert_eq!(cron_text("0 3 * * 1-5"), "weekdays at 03:00");
        assert_eq!(cron_text("0 3 1 * *"), "0 3 1 * *");
        assert_eq!(cron_text(""), "never");
        assert_eq!(thousands(&Value::from(1234567)), "1,234,567");
        assert_eq!(thousands(&Value::from(-1000)), "-1,000");
        assert_eq!(thousands(&Value::from(12)), "12");
        assert_eq!(fmt_ts(&Value::from("")), "–");
        assert_eq!(fmt_ts(&Value::from("garbage")), "garbage");
        assert!(fmt_ts(&Value::from("20260601T120000Z")).starts_with("2026-06-0"));
        assert!(fmt_ts(&Value::from("2026-06-01T12:00:00Z")).starts_with("2026-06-0"));
        assert_eq!(human_age(&Value::from("short")), "–");
        assert!(human_age(&Value::from("20000101T000000Z")).ends_with(" w ago"));
    }

    #[test]
    fn escapes_html_but_not_slashes() {
        let mut env = environment();
        env.add_template("t.html", "<a data-next=\"{{ p }}\">{{ t }}</a>").expect("tpl");
        let out = env
            .get_template("t.html")
            .expect("t")
            .render(minijinja::context! {p => "/suites/a b", t => "<b>&'\""})
            .expect("render");
        assert_eq!(out, "<a data-next=\"/suites/a b\">&lt;b&gt;&amp;&#39;&#34;</a>");
    }

    #[test]
    fn format_and_logo() {
        let mut env = environment();
        env.add_template(
            "t",
            "{{ n|thousands }}|{{ app_logo('a b', 'radarr') }}|{{ COLLECTION_LABELS['queue']|default('x') }}",
        )
        .expect("tpl");
        let out = env.get_template("t").expect("t").render(minijinja::context! {n => 12345}).expect("render");
        assert_eq!(
            out,
            r#"12,345|<img class="mini-logo" src="/api/apps/a%20b/logo" alt="" title="a b" loading="lazy" data-fallback="/static/icons/radarr.svg">|queued"#
        );
    }
}
