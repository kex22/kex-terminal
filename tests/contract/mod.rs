mod proxy_test;
mod terminal_test;

use crate::support::{BinaryStep, Scenario, ScenarioStep};
use kex::cloud::manager::ProxyEvent;
use serde_json::Value;
use std::collections::HashMap;

/// Filter scenario steps by direction.
pub fn filter_steps<'a>(scenario: &'a Scenario, direction: &str) -> Vec<&'a ScenarioStep> {
    scenario
        .steps
        .iter()
        .filter(|s| s.direction == direction)
        .collect()
}

/// Convert a ProxyEvent into a JSON Value matching the scenario message format.
pub fn proxy_event_to_message(event: &ProxyEvent) -> Option<Value> {
    match event {
        ProxyEvent::Head {
            request_id,
            status,
            headers,
        } => {
            let hdr_json: serde_json::Map<String, Value> = headers
                .iter()
                .map(|(k, v): (&String, &Vec<String>)| {
                    let val = if v.len() == 1 {
                        Value::String(v[0].clone())
                    } else {
                        Value::Array(
                            v.iter()
                                .map(|s: &String| Value::String(s.clone()))
                                .collect(),
                        )
                    };
                    (k.clone(), val)
                })
                .collect();
            Some(serde_json::json!({
                "v": 1,
                "type": "proxy.response.head",
                "payload": {
                    "requestId": request_id,
                    "status": status,
                    "headers": hdr_json,
                }
            }))
        }
        ProxyEvent::End { request_id } => Some(serde_json::json!({
            "v": 1,
            "type": "proxy.response.end",
            "payload": { "requestId": request_id }
        })),
        ProxyEvent::Error {
            request_id,
            message,
        } => Some(serde_json::json!({
            "v": 1,
            "type": "proxy.response.error",
            "payload": { "requestId": request_id, "message": message }
        })),
        ProxyEvent::Body { .. } => None, // binary frame, handled separately
        _ => None,
    }
}

/// Build a binary frame from a scenario BinaryStep.
pub fn binary_step_to_frame(step: &BinaryStep) -> Vec<u8> {
    use base64::Engine;
    let frame_type = u8::from_str_radix(step.frame_type.trim_start_matches("0x"), 16)
        .expect("invalid frameType hex");
    let payload = base64::engine::general_purpose::STANDARD
        .decode(&step.payload_base64)
        .expect("invalid base64 payload");
    kex::cloud::manager::encode_binary_frame(&step.id, frame_type, &payload)
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
