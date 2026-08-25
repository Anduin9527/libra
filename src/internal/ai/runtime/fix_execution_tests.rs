use super::*;

struct UnusedResponder;

#[async_trait]
impl ReviewFixInteractionResponder for UnusedResponder {
    async fn respond(
        &self,
        _interaction: ReviewFixInteraction,
    ) -> Result<ReviewFixInteractionResponse, String> {
        Err("the responder must not be called".to_string())
    }
}

#[test]
fn active_transport_failure_is_not_reported_as_missing_runtime() {
    assert_eq!(
        normalize_active_execution_error(ReviewFixBridgeError::Unavailable),
        ReviewFixBridgeError::ExecutionFailed
    );
}

#[test]
fn detach_failure_does_not_hide_a_definitive_denial() {
    assert_eq!(
        finish_execution(
            Err(ReviewFixBridgeError::ApprovalDenied),
            Err(ReviewFixBridgeError::ExecutionFailed),
        ),
        Err(ReviewFixBridgeError::ApprovalDenied)
    );
}

#[test]
fn detach_failure_still_fails_an_otherwise_successful_execution() {
    assert_eq!(
        finish_execution(
            Ok(ReviewFixExecutionOutcome::PatchApplied),
            Err(ReviewFixBridgeError::ExecutionFailed),
        ),
        Err(ReviewFixBridgeError::ExecutionFailed)
    );
}

#[tokio::test]
async fn untrusted_seed_is_refused_before_control_discovery() {
    let error = execute_review_fix(
        Path::new("/path-that-must-not-be-read"),
        ReviewFixInput::UntrustedSeed,
        &UnusedResponder,
    )
    .await
    .expect_err("untrusted input must not reach runtime discovery");
    assert_eq!(error, ReviewFixBridgeError::UntrustedSeed);
}
