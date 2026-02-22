use serde_json::Value;
use std::fs;
use std::path::Path;

fn load_json(path: &str) -> Value {
    let content = fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
    serde_json::from_str(&content).unwrap_or_else(|e| panic!("failed to parse {path}: {e}"))
}

fn schema_dir() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/protocol/schemas/v1")
}

fn fixture_dir() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/protocol/fixtures/v1")
}

fn validate_fixture_envelope(fixture: &Value) {
    let schema_path = format!("{}/envelope.json", schema_dir());
    let schema = load_json(&schema_path);
    let validator = jsonschema::validator_for(&schema).expect("invalid envelope schema");
    assert!(fixture.is_array(), "fixture must be an array");
    for (i, msg) in fixture.as_array().unwrap().iter().enumerate() {
        let result = validator.validate(msg);
        assert!(result.is_ok(), "fixture[{i}] failed envelope validation: {:?}",
            result.err().map(|e| e.to_string()));
    }
}

fn validate_fixture_payloads(fixture: &Value, schema: &Value) {
    let defs = schema.get("definitions").expect("schema missing definitions");
    for (i, msg) in fixture.as_array().unwrap().iter().enumerate() {
        let msg_type = msg["type"].as_str().unwrap();
        let payload_schema = defs.get(msg_type)
            .unwrap_or_else(|| panic!("no definition for type '{msg_type}'"));
        let validator = jsonschema::validator_for(payload_schema)
            .unwrap_or_else(|e| panic!("invalid schema for '{msg_type}': {e}"));
        let result = validator.validate(&msg["payload"]);
        assert!(result.is_ok(), "fixture[{i}] payload failed '{msg_type}' validation: {:?}",
            result.err().map(|e| e.to_string()));
    }
}

#[test]
fn fixture_auth_validates() {
    let fixture = load_json(&format!("{}/auth.json", fixture_dir()));
    validate_fixture_envelope(&fixture);
    let schema = load_json(&format!("{}/auth.json", schema_dir()));
    validate_fixture_payloads(&fixture, &schema);
}

#[test]
fn fixture_terminal_sync_validates() {
    let fixture = load_json(&format!("{}/terminal-sync.json", fixture_dir()));
    validate_fixture_envelope(&fixture);
    let schema = load_json(&format!("{}/terminal-sync.json", schema_dir()));
    validate_fixture_payloads(&fixture, &schema);
}

#[test]
fn fixture_device_status_validates() {
    let fixture = load_json(&format!("{}/device-status.json", fixture_dir()));
    validate_fixture_envelope(&fixture);
    let schema = load_json(&format!("{}/device-status.json", schema_dir()));
    validate_fixture_payloads(&fixture, &schema);
}

#[test]
fn fixture_error_validates() {
    let fixture = load_json(&format!("{}/error.json", fixture_dir()));
    validate_fixture_envelope(&fixture);
    let schema = load_json(&format!("{}/error.json", schema_dir()));
    validate_fixture_payloads(&fixture, &schema);
}

#[test]
fn all_fixtures_have_version_1() {
    let dir = Path::new(fixture_dir());
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let fixture = load_json(path.to_str().unwrap());
        for (i, msg) in fixture.as_array().unwrap().iter().enumerate() {
            assert_eq!(msg["v"], 1, "{}[{i}] has wrong version", path.display());
        }
    }
}
