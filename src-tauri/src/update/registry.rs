//! npm registry HTTP client — metadata lookups and version resolution.

use std::time::Duration;

use semver::Version;
use serde_json::Value;

const DEFAULT_REGISTRY: &str = "https://registry.npmmirror.com";
const OFFICIAL_REGISTRY: &str = "https://registry.npmjs.org";
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

/// Registry order for update checks and installs. The configured registry stays
/// first, while the canonical npm registry covers inconsistent mirror metadata
/// and failed installs.
pub(crate) fn registry_candidates() -> Vec<String> {
    registries_with_official_fallback(registry_base())
}

fn registries_with_official_fallback(primary: String) -> Vec<String> {
    if primary.eq_ignore_ascii_case(OFFICIAL_REGISTRY) {
        vec![primary]
    } else {
        vec![primary, OFFICIAL_REGISTRY.to_owned()]
    }
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
/// the highest published version when the tag is absent or unparseable. A
/// valid tag must also exist in `versions`; mirrors can expose a new dist-tag
/// before the corresponding package metadata has finished syncing.
pub(crate) fn latest_candidate(metadata: &Value) -> Option<Version> {
    let versions = metadata.get("versions").and_then(Value::as_object)?;
    if let Some(tagged) = metadata
        .get("dist-tags")
        .and_then(|tags| tags.get("latest"))
        .and_then(Value::as_str)
    {
        if let Ok(version) = Version::parse(tagged) {
            return versions.contains_key(tagged).then_some(version);
        }
    }
    versions
        .keys()
        .filter_map(|key| Version::parse(key).ok())
        .max()
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
    fn rejects_latest_tag_before_version_metadata_is_synced() {
        let metadata = json!({
            "dist-tags": { "latest": "0.1.2-rc.1" },
            "versions": { "0.1.1-rc.2": {} }
        });
        assert_eq!(latest_candidate(&metadata), None);
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

    #[test]
    fn appends_official_registry_as_fallback_without_duplicates() {
        assert_eq!(
            registries_with_official_fallback(DEFAULT_REGISTRY.to_owned()),
            vec![DEFAULT_REGISTRY.to_owned(), OFFICIAL_REGISTRY.to_owned()]
        );
        assert_eq!(
            registries_with_official_fallback(OFFICIAL_REGISTRY.to_owned()),
            vec![OFFICIAL_REGISTRY.to_owned()]
        );
    }
}
