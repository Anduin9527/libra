//! Validation and outcome classification for review-fix interaction responses.

use std::collections::BTreeSet;

use super::{
    ReviewFixBridgeError,
    fix_protocol::{ReviewFixInteraction, ReviewFixInteractionKind, ReviewFixInteractionResponse},
};

const MAX_RESPONSE_BYTES: usize = 256 * 1024;

pub(super) fn validate_response(
    interaction: &ReviewFixInteraction,
    response: &ReviewFixInteractionResponse,
) -> Result<(), ReviewFixBridgeError> {
    if !response.selected_option.is_empty() && !response.answers.is_empty() {
        return Err(ReviewFixBridgeError::InvalidInteractionResponse);
    }
    if !response.selected_option.is_empty()
        && !interaction
            .options
            .iter()
            .any(|option| option == &response.selected_option)
    {
        return Err(ReviewFixBridgeError::InvalidInteractionResponse);
    }
    if response.selected_option.is_empty() && response.answers.is_empty() {
        return Err(ReviewFixBridgeError::InvalidInteractionResponse);
    }
    if !response.answers.is_empty() {
        validate_answers(interaction, response)?;
    }
    Ok(())
}

fn validate_answers(
    interaction: &ReviewFixInteraction,
    response: &ReviewFixInteractionResponse,
) -> Result<(), ReviewFixBridgeError> {
    let expected = interaction
        .questions
        .iter()
        .map(|question| question.id.as_str())
        .collect::<BTreeSet<_>>();
    let supplied = response
        .answers
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if expected != supplied {
        return Err(ReviewFixBridgeError::InvalidInteractionResponse);
    }
    let mut response_bytes = 0_usize;
    for question in &interaction.questions {
        let answers = response
            .answers
            .get(&question.id)
            .ok_or(ReviewFixBridgeError::InvalidInteractionResponse)?;
        if answers.is_empty() || answers.iter().any(|answer| answer.trim().is_empty()) {
            return Err(ReviewFixBridgeError::InvalidInteractionResponse);
        }
        for answer in answers {
            response_bytes = response_bytes
                .checked_add(answer.len())
                .filter(|size| *size <= MAX_RESPONSE_BYTES)
                .ok_or(ReviewFixBridgeError::InvalidInteractionResponse)?;
        }
        if !question.options.is_empty()
            && !question.allow_other
            && answers
                .iter()
                .any(|answer| !question.options.contains(answer))
        {
            return Err(ReviewFixBridgeError::InvalidInteractionResponse);
        }
    }
    Ok(())
}

pub(super) fn denial_for(
    interaction: &ReviewFixInteraction,
    response: &ReviewFixInteractionResponse,
) -> Option<ReviewFixBridgeError> {
    let selection = response.selected_option.to_ascii_lowercase();
    if !matches!(
        selection.as_str(),
        "deny" | "decline" | "reject" | "abort" | "no"
    ) {
        return None;
    }
    match interaction.kind {
        ReviewFixInteractionKind::Approval => Some(ReviewFixBridgeError::ApprovalDenied),
        ReviewFixInteractionKind::SandboxApproval => Some(ReviewFixBridgeError::SandboxDenied),
        _ => None,
    }
}

pub(super) fn requires_code_session_handoff(
    interaction: &ReviewFixInteraction,
    response: &ReviewFixInteractionResponse,
) -> bool {
    let selection = response.selected_option.to_ascii_lowercase();
    matches!(
        (interaction.kind, selection.as_str()),
        (
            ReviewFixInteractionKind::IntentReviewChoice,
            "modify" | "cancel"
        ) | (
            ReviewFixInteractionKind::PostPlanChoice,
            "modify" | "cancel" | "back"
        )
    )
}

#[cfg(test)]
#[path = "fix_response_tests.rs"]
mod tests;
