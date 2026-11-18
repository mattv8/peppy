use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum LaunchPhase {
    Starting,
    Unlocking,
    Opening,
    Updating,
    Loading,
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct LaunchStep {
    pub phase: LaunchPhase,
    #[ts(type = "number")]
    pub duration_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct LaunchUpdate {
    #[ts(type = "number")]
    pub completed: u64,
    #[ts(type = "number")]
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct LaunchProgress {
    pub phase: LaunchPhase,
    #[ts(type = "number")]
    pub elapsed_ms: u64,
    pub steps: Vec<LaunchStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub update: Option<LaunchUpdate>,
}
