// SPDX-License-Identifier: GPL-3.0-or-later
//! Where a service's own config file keeps its API key, and reading it from
//! there: the format is the service's ([`Credentials::ApiKey`](crate::Credentials)),
//! the readers are shared.

use regex::Regex;
use serde_json::Value;

/// Where an app's config file (`api_key_file`) keeps its API key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyFile {
    /// An XML element's text: `<ApiKey>…</ApiKey>` (the arrs' config.xml).
    XmlTag(&'static str),
    /// A YAML key at any depth: `apikey: …` (Bazarr's config.yaml).
    YamlKey(&'static str),
    /// An INI key: `api_key = …` (SABnzbd's sabnzbd.ini).
    IniKey(&'static str),
    /// A JSON pointer: `/main/apiKey` (Seerr's settings.json). The app writes the
    /// key the first time it starts.
    JsonPointer(&'static str),
}

/// Why no key came out of the file's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyFileError {
    /// The file isn't in its format (a JSON parse error).
    Unreadable(String),
    /// It holds no key where the format says.
    NoKey,
}

impl KeyFile {
    /// The key in `text`, the file's contents.
    pub fn read(self, text: &str) -> Result<String, KeyFileError> {
        let found = match self {
            KeyFile::XmlTag(tag) => {
                let tag = regex::escape(tag);
                first_capture(&format!(r"<{tag}>\s*([^<]+?)\s*</{tag}>"), text)
            }
            KeyFile::YamlKey(key) => {
                first_capture(&format!(r#"(?m)^\s*{}:\s*['"]?([A-Za-z0-9]+)"#, regex::escape(key)), text)
            }
            KeyFile::IniKey(key) => {
                first_capture(&format!(r"(?m)^\s*{}\s*=\s*([A-Za-z0-9]+)", regex::escape(key)), text)
            }
            KeyFile::JsonPointer(pointer) => {
                let j: Value = serde_json::from_str(text).map_err(|e| KeyFileError::Unreadable(e.to_string()))?;
                j.pointer(pointer).and_then(Value::as_str).filter(|k| !k.is_empty()).map(str::to_string)
            }
        };
        found.ok_or(KeyFileError::NoKey)
    }

    /// The message for a file with no key in it: the key's place for a format
    /// that has one (`main.apiKey`), and the reason that usually is.
    pub fn missing(self, app: &str, path: &str) -> String {
        match self {
            KeyFile::JsonPointer(pointer) => {
                let dotted = pointer.trim_start_matches('/').replace('/', ".");
                format!("{app}: no {dotted} in {path} (has it started once?)")
            }
            KeyFile::XmlTag(_) | KeyFile::YamlKey(_) | KeyFile::IniKey(_) => {
                format!("{app}: no API key found in {path}")
            }
        }
    }
}

/// The first capture group of `pattern` in `text`. The patterns are built from a
/// descriptor's static names, escaped, so they always compile.
fn first_capture(pattern: &str, text: &str) -> Option<String> {
    let re = Regex::new(pattern).ok()?;
    re.captures(text).map(|c| c[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_each_format() {
        let xml = "<Config>\n  <Port>7878</Port>\n  <ApiKey> abc123 </ApiKey>\n</Config>";
        assert_eq!(KeyFile::XmlTag("ApiKey").read(xml), Ok("abc123".into()));
        let yaml = "general:\n  port: 6767\nauth:\n  apikey: 'f00d'\n";
        assert_eq!(KeyFile::YamlKey("apikey").read(yaml), Ok("f00d".into()));
        let ini = "[misc]\nnzb_key = nope\napi_key = sab42\n";
        assert_eq!(KeyFile::IniKey("api_key").read(ini), Ok("sab42".into()));
        let json = r#"{"main": {"apiKey": "seer=="}}"#;
        assert_eq!(KeyFile::JsonPointer("/main/apiKey").read(json), Ok("seer==".into()));
    }

    #[test]
    fn says_why_there_is_none() {
        assert_eq!(KeyFile::XmlTag("ApiKey").read("<Config/>"), Err(KeyFileError::NoKey));
        assert_eq!(KeyFile::JsonPointer("/main/apiKey").read(r#"{"main": {"apiKey": ""}}"#), Err(KeyFileError::NoKey));
        assert!(matches!(KeyFile::JsonPointer("/main/apiKey").read("not json"), Err(KeyFileError::Unreadable(_))));
        assert_eq!(
            KeyFile::JsonPointer("/main/apiKey").missing("seerr", "/c/settings.json"),
            "seerr: no main.apiKey in /c/settings.json (has it started once?)"
        );
        assert_eq!(
            KeyFile::IniKey("api_key").missing("sab", "/c/sabnzbd.ini"),
            "sab: no API key found in /c/sabnzbd.ini"
        );
    }
}
