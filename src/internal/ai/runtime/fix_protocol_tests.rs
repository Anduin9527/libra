use super::*;

fn snapshot_with_interaction(interaction: serde_json::Value) -> ReviewFixSessionSnapshot {
    serde_json::from_value(serde_json::json!({
        "status": "awaiting_interaction",
        "interactions": [interaction],
        "patchsets": []
    }))
    .expect("test snapshot follows the narrow wire schema")
}

#[test]
fn request_user_input_requires_unique_question_ids() {
    let snapshot = snapshot_with_interaction(serde_json::json!({
        "id": "input-1",
        "kind": "request_user_input",
        "status": "pending",
        "metadata": {
            "questions": [
                { "id": "risk", "prompt": "Risk?" },
                { "id": "risk", "prompt": "Again?" }
            ]
        }
    }));
    assert_eq!(
        snapshot
            .pending_interactions()
            .expect_err("duplicate ids fail closed"),
        ReviewFixBridgeError::ExecutionFailed
    );
}

#[test]
fn selectable_ids_reject_terminal_control_characters() {
    let snapshot = snapshot_with_interaction(serde_json::json!({
        "id": "approval-1",
        "kind": "approval",
        "status": "pending",
        "options": [{ "id": "approve\u{001b}[2J" }]
    }));
    assert_eq!(
        snapshot
            .pending_interactions()
            .expect_err("control ids fail closed"),
        ReviewFixBridgeError::ExecutionFailed
    );
}
