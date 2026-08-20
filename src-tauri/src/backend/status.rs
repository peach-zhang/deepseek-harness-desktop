use serde::Serialize;

use crate::update::{UpdateStage, UPDATE_STAGE_TOTAL};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackendStatus {
    pub phase: String,
    pub message: String,
    pub url: Option<String>,
    pub harness_version: String,
    /// Current update step (1-based) when the backend is mid-update; `None`
    /// otherwise. Paired with `update_stage_total` so the frontend can render
    /// a "step N/M" progress indicator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_stage: Option<usize>,
    /// Total number of update steps; always `Some` once a value is set so the
    /// JSON payload lets the frontend size a progress bar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_stage_total: Option<usize>,
    /// Short, localized label for the current update step (e.g. "下载 npm CLI").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_stage_description: Option<String>,
}

impl BackendStatus {
    pub fn starting(version: &str) -> Self {
        Self {
            phase: "starting".into(),
            message: "正在启动内置 DeepSeek Harness…".into(),
            url: None,
            harness_version: version.into(),
            update_stage: None,
            update_stage_total: None,
            update_stage_description: None,
        }
    }

    pub fn checking_update(current: &str) -> Self {
        Self {
            phase: "checking".into(),
            message: "正在检查 DeepSeek Harness 更新…".into(),
            url: None,
            harness_version: current.into(),
            update_stage: None,
            update_stage_total: None,
            update_stage_description: None,
        }
    }

    pub fn updating(target: &str) -> Self {
        Self {
            phase: "updating".into(),
            message: format!("发现新版本 {target}，正在更新 DeepSeek Harness…"),
            url: None,
            harness_version: target.into(),
            update_stage: None,
            update_stage_total: None,
            update_stage_description: None,
        }
    }

    /// Status emitted while the update flows through its discrete steps
    /// (downloading the npm CLI, installing the new Harness, …). Keeps the
    /// existing `message` so the top-level heading still reads naturally,
    /// while `update_stage`/`update_stage_total` drive a step indicator.
    pub fn updating_stage(stage: UpdateStage, target: &str) -> Self {
        Self {
            phase: "updating".into(),
            message: format!("发现新版本 {target}，正在更新 DeepSeek Harness…"),
            url: None,
            harness_version: target.into(),
            update_stage: Some(stage.index),
            update_stage_total: Some(UPDATE_STAGE_TOTAL),
            update_stage_description: Some(stage.description.to_owned()),
        }
    }

    pub fn running(url: String, version: &str) -> Self {
        Self {
            phase: "running".into(),
            message: "DeepSeek Harness 已就绪。".into(),
            url: Some(url),
            harness_version: version.into(),
            update_stage: None,
            update_stage_total: None,
            update_stage_description: None,
        }
    }

    pub fn failed(message: impl Into<String>, version: &str) -> Self {
        Self {
            phase: "failed".into(),
            message: message.into(),
            url: None,
            harness_version: version.into(),
            update_stage: None,
            update_stage_total: None,
            update_stage_description: None,
        }
    }
}
