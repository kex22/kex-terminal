mod proxy_test;
mod terminal_test;

use crate::support::{Scenario, ScenarioStep};
use serde_json::Value;

/// Filter scenario steps by direction.
pub fn filter_steps<'a>(scenario: &'a Scenario, direction: &str) -> Vec<&'a ScenarioStep> {
    scenario
        .steps
        .iter()
        .filter(|s| s.direction == direction)
        .collect()
}

/// Assert that an actual JSON message matches an expected scenario message,
/// ignoring dynamic fields like requestId.
pub fn assert_message_matches(actual: &Value, expected: &Value) {
    let actual_type = actual["type"].as_str().unwrap_or("");
    let expected_type = expected["type"].as_str().unwrap_or("");
    assert_eq!(actual_type, expected_type, "message type mismatch");
    assert_eq!(actual["v"], expected["v"], "version mismatch");

    // Compare payload fields, skipping requestId (dynamic)
    let actual_payload = &actual["payload"];
    let expected_payload = &expected["payload"];

    if let (Some(a_obj), Some(e_obj)) = (actual_payload.as_object(), expected_payload.as_object()) {
        for (key, expected_val) in e_obj {
            if key == "requestId" {
                // Just verify it exists, don't compare value
                assert!(
                    a_obj.contains_key(key),
                    "missing dynamic field '{key}' in actual payload"
                );
                continue;
            }
            let actual_val = a_obj.get(key);
            assert_eq!(
                actual_val,
                Some(expected_val),
                "payload field '{key}' mismatch"
            );
        }
    }
}
