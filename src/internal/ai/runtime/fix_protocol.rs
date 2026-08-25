//! Narrow, typed control-protocol projection for `libra review --fix`.
//!
//! The Code session endpoint exposes a much richer snapshot. This module
//! deliberately deserializes only the fields the review-fix state machine may
//! act on, so untrusted wire JSON cannot leak into execution decisions.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{PlanExecutionRepairState, ReviewFixBridgeError};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewFixSessionStatus {
    Idle,
    Thinking,
    ExecutingTool,
    AwaitingInteraction,
    Completed,
    Error,
    IndeterminateSideEffect,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewFixInteractionKind {
    Approval,
    SandboxApproval,
    RequestUserInput,
    IntentReviewChoice,
    PostPlanChoice,
    PlanExecutionRepair,
}

impl ReviewFixInteractionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::SandboxApproval => "sandbox_approval",
            Self::RequestUserInput => "request_user_input",
            Self::IntentReviewChoice => "intent_review_choice",
            Self::PostPlanChoice => "post_plan_choice",
            Self::PlanExecutionRepair => "plan_execution_repair",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReviewFixQuestion {
    pub id: String,
    pub prompt: String,
    pub options: Vec<String>,
    pub allow_other: bool,
    pub is_secret: bool,
}

#[derive(Clone, Debug)]
pub struct ReviewFixInteraction {
    pub id: String,
    pub kind: ReviewFixInteractionKind,
    pub title: Option<String>,
    pub prompt: Option<String>,
    pub options: Vec<String>,
    pub questions: Vec<ReviewFixQuestion>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFixInteractionResponse {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub selected_option: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub answers: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReviewFixInteractionStatus {
    Pending,
    Resolved,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireInteraction {
    id: String,
    kind: ReviewFixInteractionKind,
    status: ReviewFixInteractionStatus,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    options: Vec<WireOption>,
    #[serde(default)]
    metadata: WireInteractionMetadata,
}

#[derive(Clone, Debug, Deserialize)]
struct WireOption {
    id: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct WireInteractionMetadata {
    #[serde(default)]
    questions: Vec<WireQuestion>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireQuestion {
    id: String,
    #[serde(default)]
    header: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    options: Vec<WireOption>,
    #[serde(default)]
    is_other: bool,
    #[serde(default)]
    is_secret: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReviewFixPatchset {
    id: String,
    status: String,
    #[serde(default)]
    changes: Vec<ReviewFixPatchChange>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
struct ReviewFixPatchChange {
    path: String,
    #[serde(alias = "type")]
    change_type: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReviewFixSessionSnapshot {
    pub status: ReviewFixSessionStatus,
    #[serde(default)]
    interactions: Vec<WireInteraction>,
    #[serde(default)]
    patchsets: Vec<ReviewFixPatchset>,
    #[serde(default)]
    pub plan_execution_repair: Option<PlanExecutionRepairState>,
}

impl ReviewFixSessionSnapshot {
    pub(super) fn pending_interactions(
        &self,
    ) -> Result<Vec<ReviewFixInteraction>, ReviewFixBridgeError> {
        let mut ids = BTreeSet::new();
        let pending = self
            .interactions
            .iter()
            .filter(|interaction| interaction.status == ReviewFixInteractionStatus::Pending)
            .collect::<Vec<_>>();
        // A human-controlled CLI cannot safely service an unbounded form.
        if pending.len() > 64 {
            return Err(ReviewFixBridgeError::ExecutionFailed);
        }
        pending
            .into_iter()
            .map(|interaction| interaction.to_public(&mut ids))
            .collect()
    }

    pub(super) fn patchsets(&self) -> Result<BTreeSet<ReviewFixPatchset>, ReviewFixBridgeError> {
        if self
            .patchsets
            .iter()
            .any(|patchset| patchset.id.trim().is_empty())
        {
            return Err(ReviewFixBridgeError::ExecutionFailed);
        }
        Ok(self.patchsets.iter().cloned().collect())
    }
}

impl WireInteraction {
    fn to_public(
        &self,
        interaction_ids: &mut BTreeSet<String>,
    ) -> Result<ReviewFixInteraction, ReviewFixBridgeError> {
        let id = required_unique_id(&self.id, interaction_ids)?;
        let options = option_ids(&self.options)?;
        if self.metadata.questions.len() > 16 {
            return Err(ReviewFixBridgeError::ExecutionFailed);
        }
        let mut question_ids = BTreeSet::new();
        let questions = self
            .metadata
            .questions
            .iter()
            .map(|question| question.to_public(&mut question_ids))
            .collect::<Result<Vec<_>, _>>()?;
        let requests_input = self.kind == ReviewFixInteractionKind::RequestUserInput;
        if (requests_input && (!options.is_empty() || questions.is_empty()))
            || (!requests_input && (!questions.is_empty() || options.is_empty()))
        {
            return Err(ReviewFixBridgeError::ExecutionFailed);
        }
        Ok(ReviewFixInteraction {
            id,
            kind: self.kind,
            title: non_empty(self.title.as_deref()),
            prompt: non_empty(self.prompt.as_deref())
                .or_else(|| non_empty(self.description.as_deref())),
            options,
            questions,
        })
    }
}

impl WireQuestion {
    fn to_public(
        &self,
        question_ids: &mut BTreeSet<String>,
    ) -> Result<ReviewFixQuestion, ReviewFixBridgeError> {
        let id = required_unique_id(&self.id, question_ids)?;
        let prompt = non_empty(Some(&self.prompt))
            .or_else(|| non_empty(Some(&self.header)))
            .ok_or(ReviewFixBridgeError::ExecutionFailed)?;
        Ok(ReviewFixQuestion {
            id,
            prompt,
            options: option_ids(&self.options)?,
            allow_other: self.is_other,
            is_secret: self.is_secret,
        })
    }
}

fn option_ids(options: &[WireOption]) -> Result<Vec<String>, ReviewFixBridgeError> {
    if options.len() > 64 {
        return Err(ReviewFixBridgeError::ExecutionFailed);
    }
    let mut ids = BTreeSet::new();
    options
        .iter()
        .map(|option| required_unique_id(&option.id, &mut ids))
        .collect()
}

fn required_unique_id(
    value: &str,
    ids: &mut BTreeSet<String>,
) -> Result<String, ReviewFixBridgeError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed != value
        || trimmed.chars().count() > 512
        || trimmed.chars().any(char::is_control)
        || !ids.insert(trimmed.to_string())
    {
        return Err(ReviewFixBridgeError::ExecutionFailed);
    }
    Ok(trimmed.to_string())
}

fn non_empty(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
#[path = "fix_protocol_tests.rs"]
mod tests;
