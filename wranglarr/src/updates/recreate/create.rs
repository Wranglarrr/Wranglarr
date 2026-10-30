// SPDX-License-Identifier: GPL-3.0-or-later
//! Building the create request for a container's replacement from the old one's settings.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::super::USER_ENV;
use crate::value::truthy;

/// A /containers/create body that reproduces `old` on a new image.
///
/// Values the old container only had because its *image* defined them (env,
/// labels, cmd, entrypoint, exposed ports …) are dropped so the new image's
/// defaults apply, the way `docker compose up` would. Returns (body,
/// extra_networks): the create call accepts one network, the rest are
/// connected afterwards.
pub(in crate::updates) fn build_create_body(
    old: &Value,
    old_image: Option<&Value>,
    image_ref: &str,
    new_image_id: &str,
    network_mode: Option<&str>,
) -> (Value, Vec<(String, Value)>) {
    let mut cfg = old["Config"].as_object().cloned().unwrap_or_default();
    let mut hc = old["HostConfig"].as_object().cloned().unwrap_or_default();
    let empty = Map::new();
    let icfg = old_image.and_then(|i| i["Config"].as_object()).unwrap_or(&empty);
    let old_id = old["Id"].as_str().unwrap_or("");
    let old_short = old_id.get(..12).unwrap_or(old_id);
    let null = Value::Null;

    cfg.insert("Image".into(), json!(image_ref));
    // a value equal to the image's could be inherited or set on purpose; these are always yours
    let img_env: HashSet<&str> = icfg
        .get("Env")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|e| !USER_ENV.contains(&e.split('=').next().unwrap_or("")))
        .collect();
    // Podman lists the HOSTNAME it set among the env: the new container gets its own
    let engine_hostname = cfg.get("Hostname").and_then(Value::as_str).map(|h| format!("HOSTNAME={h}"));
    let env: Vec<Value> = cfg
        .get("Env")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| !e.as_str().is_some_and(|s| img_env.contains(s) || engine_hostname.as_deref() == Some(s)))
        .cloned()
        .collect();
    cfg.insert("Env".into(), Value::Array(env));
    let img_labels = icfg.get("Labels").and_then(Value::as_object).unwrap_or(&empty);
    let mut labels: Map<String, Value> = cfg
        .get("Labels")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(k, v)| img_labels.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if labels.contains_key("com.docker.compose.image") {
        labels.insert("com.docker.compose.image".into(), json!(new_image_id)); // keeps compose from seeing drift
    }
    cfg.insert("Labels".into(), Value::Object(labels));
    for key in ["Cmd", "Entrypoint", "WorkingDir", "User", "Healthcheck", "StopSignal", "OnBuild", "Shell"] {
        if cfg.get(key).is_some_and(|v| v == icfg.get(key).unwrap_or(&null)) {
            cfg.remove(key);
        }
    }
    for key in ["ExposedPorts", "Volumes"] {
        let img_keys = icfg.get(key).and_then(Value::as_object).unwrap_or(&empty);
        let left: Map<String, Value> = cfg
            .get(key)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(k, _)| !img_keys.contains_key(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if left.is_empty() {
            cfg.remove(key);
        } else {
            cfg.insert(key.into(), Value::Object(left));
        }
    }

    if let Some(m) = network_mode.filter(|m| !m.is_empty()) {
        hc.insert("NetworkMode".into(), json!(m));
    }
    let mode = hc.get("NetworkMode").and_then(Value::as_str).unwrap_or("").to_string();
    let shared_ns = mode.starts_with("container:");
    if shared_ns || cfg.get("Hostname").and_then(Value::as_str) == Some(old_short) {
        cfg.remove("Hostname");
        cfg.remove("Domainname");
    }
    if shared_ns {
        cfg.remove("ExposedPorts");
        cfg.remove("MacAddress");
        hc.remove("PortBindings");
    }

    // volumes the container had that aren't declared in HostConfig (anonymous
    // volumes from the image's VOLUME lines): reattach so data isn't orphaned
    let mut declared: HashSet<String> = hc
        .get("Binds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|b| b.split(':').nth(1).map(str::to_string))
        .collect();
    declared.extend(
        hc.get("Mounts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|m| m["Target"].as_str().map(str::to_string)),
    );
    let mut extra_mounts = vec![];
    for m in old["Mounts"].as_array().into_iter().flatten() {
        let dest = m["Destination"].as_str().unwrap_or("");
        if m["Type"] == "volume" && !declared.contains(dest) && truthy(&m["Name"]) {
            let rw = m["RW"].as_bool().unwrap_or(true);
            extra_mounts.push(json!({"Type": "volume", "Source": m["Name"], "Target": m["Destination"],
                                     "ReadOnly": !rw}));
        }
    }
    if !extra_mounts.is_empty() {
        let mounts = hc.entry("Mounts").or_insert_with(|| json!([]));
        if !mounts.is_array() {
            *mounts = json!([]);
        }
        if let Some(a) = mounts.as_array_mut() {
            a.extend(extra_mounts);
        }
    }

    let mut endpoints: Vec<(String, Value)> = vec![];
    if !shared_ns && mode != "host" && mode != "none" {
        for (net, ep) in old["NetworkSettings"]["Networks"].as_object().into_iter().flatten() {
            let aliases: Vec<Value> = ep["Aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|a| !a.as_str().is_some_and(|a| a == old_short || a == old_id))
                .cloned()
                .collect();
            let mut e = Map::new();
            e.insert("Aliases".into(), if aliases.is_empty() { Value::Null } else { Value::Array(aliases) });
            for k in ["IPAMConfig", "Links", "DriverOpts"] {
                if truthy(&ep[k]) {
                    e.insert(k.into(), ep[k].clone());
                }
            }
            endpoints.push((net.clone(), Value::Object(e)));
        }
    }
    let first = endpoints.iter().position(|(n, _)| *n == mode).or(if endpoints.is_empty() { None } else { Some(0) });
    let mut body = cfg;
    body.insert("HostConfig".into(), Value::Object(hc));
    if let Some(i) = first {
        let (net, ep) = endpoints.remove(i);
        body.insert("NetworkingConfig".into(), json!({"EndpointsConfig": {net: ep}}));
    }
    (Value::Object(body), endpoints)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_container() -> Value {
        json!({
            "Id": "abcdef0123456789ffff",
            "Image": "sha256:old",
            "Config": {"Image": "lscr.io/linuxserver/radarr:latest", "Hostname": "abcdef012345",
                       "Env": ["PATH=/usr/bin", "PUID=1000", "TZ=UTC", "MINE=1"],
                       "Labels": {"maintainer": "x", "com.docker.compose.image": "sha256:old",
                                  "com.docker.compose.service": "radarr"},
                       "Cmd": ["/init"], "ExposedPorts": {"7878/tcp": {}, "9999/tcp": {}},
                       "Volumes": {"/config": {}}, "Healthcheck": null},
            "HostConfig": {"NetworkMode": "arrs", "Binds": ["/srv/radarr:/config"],
                           "RestartPolicy": {"Name": "unless-stopped"}},
            "Mounts": [{"Type": "bind", "Source": "/srv/radarr", "Destination": "/config", "RW": true},
                       {"Type": "volume", "Name": "anon1", "Destination": "/cache", "RW": false}],
            "NetworkSettings": {"Networks": {
                "arrs": {"Aliases": ["radarr", "abcdef012345"], "IPAMConfig": null, "Links": null},
                "other": {"Aliases": ["abcdef0123456789ffff"], "IPAMConfig": {"IPv4Address": "10.0.0.5"}}}}
        })
    }

    fn old_image() -> Value {
        json!({"Config": {"Env": ["PATH=/usr/bin", "TZ=UTC"], "Labels": {"maintainer": "x"}, "Cmd": ["/init"],
                          "ExposedPorts": {"7878/tcp": {}}, "Volumes": {"/config": {}}}})
    }

    #[test]
    fn create_body_keeps_yours_drops_the_images() {
        let (body, extra) =
            build_create_body(&old_container(), Some(&old_image()), "lscr.io/x:latest", "sha256:new", None);
        assert_eq!(body["Image"], "lscr.io/x:latest");
        // image-only env dropped; TZ is a user var so kept even though it equals the image's
        assert_eq!(body["Env"], json!(["PUID=1000", "TZ=UTC", "MINE=1"]));
        assert_eq!(
            body["Labels"],
            json!({"com.docker.compose.image": "sha256:new", "com.docker.compose.service": "radarr"})
        );
        assert!(body.get("Cmd").is_none(), "Cmd equal to the image's is dropped");
        assert!(body.get("Healthcheck").is_none(), "null on both sides is dropped");
        assert_eq!(body["ExposedPorts"], json!({"9999/tcp": {}}));
        assert!(body.get("Volumes").is_none());
        assert!(body.get("Hostname").is_none(), "the generated hostname follows the new id");
        assert_eq!(body["HostConfig"]["RestartPolicy"], json!({"Name": "unless-stopped"}));
        assert_eq!(
            body["HostConfig"]["Mounts"],
            json!([{"Type": "volume", "Source": "anon1", "Target": "/cache", "ReadOnly": true}])
        );
        assert_eq!(body["NetworkingConfig"], json!({"EndpointsConfig": {"arrs": {"Aliases": ["radarr"]}}}));
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].0, "other");
        assert_eq!(extra[0].1, json!({"Aliases": null, "IPAMConfig": {"IPv4Address": "10.0.0.5"}}));
    }

    #[test]
    fn create_body_in_a_shared_namespace() {
        let mut old = old_container();
        old["HostConfig"]["PortBindings"] = json!({"7878/tcp": [{"HostPort": "7878"}]});
        old["Config"]["Hostname"] = json!("custom");
        old["Config"]["MacAddress"] = json!("02:00:00:00:00:01");
        let (body, extra) = build_create_body(&old, Some(&old_image()), "x:1", "sha256:new", Some("container:vpnid"));
        assert_eq!(body["HostConfig"]["NetworkMode"], "container:vpnid");
        for k in ["Hostname", "ExposedPorts", "MacAddress", "NetworkingConfig"] {
            assert!(body.get(k).is_none(), "{k} must go in a shared namespace");
        }
        assert!(body["HostConfig"].get("PortBindings").is_none());
        assert!(extra.is_empty());
    }

    #[test]
    fn create_body_leaves_podmans_hostname_env_behind() {
        // Podman lists the HOSTNAME it set (the old id) among the env
        let mut old = old_container();
        old["Config"]["Env"] = json!(["PUID=1000", "HOSTNAME=abcdef012345", "MINE=1"]);
        let (body, _) = build_create_body(&old, Some(&old_image()), "x:1", "sha256:new", None);
        assert_eq!(body["Env"], json!(["PUID=1000", "MINE=1"]), "the new container gets its own");
        // a HOSTNAME of your own, unlike the container's, is yours
        old["Config"]["Env"] = json!(["HOSTNAME=media-box"]);
        let (body, _) = build_create_body(&old, Some(&old_image()), "x:1", "sha256:new", None);
        assert_eq!(body["Env"], json!(["HOSTNAME=media-box"]));
    }

    #[test]
    fn create_body_without_the_old_image() {
        let (body, _) = build_create_body(&old_container(), None, "x:1", "sha256:new", None);
        assert_eq!(body["Env"], json!(["PATH=/usr/bin", "PUID=1000", "TZ=UTC", "MINE=1"]));
        assert_eq!(body["Cmd"], json!(["/init"]));
        assert_eq!(body["Labels"]["maintainer"], "x");
    }
}
