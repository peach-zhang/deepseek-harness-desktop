use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackendStatus {
    pub phase: String,
    pub message: String,
    pub url: Option<String>,
    pub harness_version: String,
}

impl BackendStatus {
    pub fn starting(version: &str) -> Self {
        Self {
            phase: "starting".into(),
            message: "正在启动内置 DeepSeek Harness…".into(),
            url: None,
            harness_version: version.into(),
        }
    }

    pub fn checking_update(current: &str) -> Self {
        Self {
            phase: "checking".into(),
            message: "正在检查 DeepSeek Harness 更新…".into(),
            url: None,
            harness_version: current.into(),
        }
    }

    pub fn updating(target: &str) -> Self {
        Self {
            phase: "updating".into(),
            message: format!("发现新版本 {target}，正在更新 DeepSeek Harness…"),
            url: None,
            harness_version: target.into(),
        }
    }

    pub fn running(url: String, version: &str) -> Self {
        Self {
            phase: "running".into(),
            message: "DeepSeek Harness 已就绪。".into(),
            url: Some(url),
            harness_version: version.into(),
        }
    }

    pub fn failed(message: impl Into<String>, version: &str) -> Self {
        Self {
            phase: "failed".into(),
            message: message.into(),
            url: None,
            harness_version: version.into(),
        }
    }
}
