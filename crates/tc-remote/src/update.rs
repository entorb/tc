//! Latest release tag of tc on GitHub, for the daily update check.

use std::time::Duration;

use tc_core::{Error, Result};

const LATEST: &str = "https://api.github.com/repos/entorb/tc/releases/latest";

/// Tag of the latest non-prerelease release (`v1.2.3`); blocks up to 10 s.
pub fn latest_tag() -> Result<String> {
    let err = |e: ureq::Error| Error::Remote(format!("update check: {e}"));
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .https_only(true)
        .user_agent("tc")
        .build()
        .new_agent();
    let body = (agent.get(LATEST).call().map_err(err)?)
        .body_mut()
        .with_config()
        .limit(1 << 20)
        .read_to_string()
        .map_err(err)?;
    tag_from_json(&body).ok_or_else(|| Error::Remote("update check: no tag_name".into()))
}

/// Value of the first `"tag_name"` string; no JSON parser for one field.
fn tag_from_json(json: &str) -> Option<String> {
    let rest = &json[json.find("\"tag_name\"")? + "\"tag_name\"".len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_owned())
}

#[cfg(test)]
mod tests {
    use super::tag_from_json;

    #[test]
    fn tag_compact_pretty_and_missing() {
        assert_eq!(
            tag_from_json(r#"{"id":1,"tag_name":"v1.2.3","name":"x"}"#).as_deref(),
            Some("v1.2.3")
        );
        assert_eq!(
            tag_from_json("{\n  \"tag_name\" :  \"v0.1.0\",\n}").as_deref(),
            Some("v0.1.0")
        );
        assert_eq!(tag_from_json(r#"{"name":"v1"}"#), None);
        assert_eq!(tag_from_json(r#"{"tag_name":5}"#), None);
    }

    #[test]
    #[ignore = "network: asks GitHub"]
    fn live_latest_tag() {
        let tag = super::latest_tag().unwrap();
        assert!(tag.starts_with('v'), "{tag}");
    }
}
