use std::collections::BTreeMap;

use super::*;
use crate::internal::ai::runtime::fix_protocol::ReviewFixQuestion;

#[test]
fn oversized_user_response_fails_before_control_submission() {
    let interaction = ReviewFixInteraction {
        id: "input-1".to_string(),
        kind: ReviewFixInteractionKind::RequestUserInput,
        title: None,
        prompt: None,
        options: Vec::new(),
        questions: vec![ReviewFixQuestion {
            id: "answer".to_string(),
            prompt: "Answer?".to_string(),
            options: Vec::new(),
            allow_other: false,
            is_secret: false,
        }],
    };
    let response = ReviewFixInteractionResponse {
        selected_option: String::new(),
        answers: BTreeMap::from([("answer".to_string(), vec!["x".repeat(256 * 1024 + 1)])]),
    };
    assert_eq!(
        validate_response(&interaction, &response).expect_err("oversized response fails closed"),
        ReviewFixBridgeError::InvalidInteractionResponse
    );
}

#[test]
fn abort_is_a_stable_approval_denial() {
    let interaction = ReviewFixInteraction {
        id: "approval-1".to_string(),
        kind: ReviewFixInteractionKind::Approval,
        title: None,
        prompt: None,
        options: vec!["approve".to_string(), "abort".to_string()],
        questions: Vec::new(),
    };
    let response = ReviewFixInteractionResponse {
        selected_option: "abort".to_string(),
        answers: BTreeMap::new(),
    };
    assert_eq!(
        denial_for(&interaction, &response),
        Some(ReviewFixBridgeError::ApprovalDenied)
    );
}

#[test]
fn plan_revision_hands_control_back_without_idle_timeout() {
    let interaction = ReviewFixInteraction {
        id: "plan-1".to_string(),
        kind: ReviewFixInteractionKind::PostPlanChoice,
        title: None,
        prompt: None,
        options: vec!["execute".to_string(), "modify".to_string()],
        questions: Vec::new(),
    };
    let response = ReviewFixInteractionResponse {
        selected_option: "modify".to_string(),
        answers: BTreeMap::new(),
    };
    assert!(requires_code_session_handoff(&interaction, &response));
}
