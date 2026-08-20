//! npm registry HTTP client — metadata lookups and version resolution.

use std::time::Duration;

use semver::Version;
use serde_json::Value;

const DEFAULT_REGISTRY: &str = "https://registry.npmmirror.com";
const REGISTRY_ENV: &str = "DSH_DESKTOP_REGISTRY";
const DISABLE_ENV: &str = "DSH_DESKTOP_UPDATE_DISABLED";
pub(crate) const DSH_METADATA_PATH: &str = "@deepseek-ai%2Fdsh";
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const MAX_METADATA_BYTES: u64 = 32 * 1024 * 1024;

pub(crate) fn updates_disabled() -> bool {
    std::env::var(DISABLE_ENV)
        .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

pub(crate) fn registry_base() -> String {
    std::env::var(REGISTRY_ENV)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_REGISTRY.to_owned())
}

pub(crate) fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into()
}

pub(crate) fn fetch_json(agent: &ureq::Agent, url: &str) -> Result<Value, String> {
    let mut response = agent
        .get(url)
        .header("Accept", "application/vnd.npm.install-v1+json")
        .call()
        .map_err(|error| format!("请求失败（{url}）：{error}"))?;
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_METADATA_BYTES)
        .read_to_string()
        .map_err(|error| format!("读取响应失败（{url}）：{error}"))?;
    serde_json::from_str(&body)
        .map_err(|error| format!("响应不是有效 JSON（{url}）：{error}"))
}

/// The version we track: the publisher's `latest` dist-tag, falling back to
/// the highest published version when the tag is absent or unparseable.
pub(crate) fn latest_candidate(metadata: &Value) -> Option<Version> {
    let tagged = metadata
        .get("dist-tags")
        .and_then(|tags| tags.get("latest"))
        .and_then(Value::as_str)
        .and_then(|version| Version::parse(version).ok());
    if tagged.is_some() {
        return tagged;
    }
    metadata
        .get("versions")
        .and_then(Value::as_object)
        .and_then(|versions| {
            versions
                .keys()
                .filter_map(|key| Version::parse(key).ok())
                .max()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prefers_latest_dist_tag() {
        let metadata = json!({
            "dist-tags": { "latest": "0.1.0-rc.8", "next": "0.1.0-rc.7" },
            "versions": { "0.1.0-rc.8": {}, "0.1.0-rc.7": {} }
        });
        assert_eq!(
            latest_candidate(&metadata),
            Some(Version::parse("0.1.0-rc.8").unwrap())
        );
    }

    #[test]
    fn falls_back_to_highest_version_without_latest_tag() {
        let metadata = json!({
            "versions": { "0.1.0-rc.8": {}, "0.1.0-rc.7": {}, "0.0.1-rc.5": {} }
        });
        assert_eq!(
            latest_candidate(&metadata),
            Some(Version::parse("0.1.0-rc.8").unwrap())
        );
    }

    #[test]
    fn stable_release_outranks_prerelease() {
        let metadata = json!({
            "versions": { "0.1.0-rc.7": {}, "0.1.0": {} }
        });
        assert_eq!(
            latest_candidate(&metadata),
            Some(Version::parse("0.1.0").unwrap())
        );
        assert!(Version::parse("0.1.0").unwrap() > Version::parse("0.1.0-rc.7").unwrap());
        assert!(Version::parse("0.1.0-rc.8").unwrap() > Version::parse("0.1.0-rc.7").unwrap());
    }
}
