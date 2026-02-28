use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub description: String,
    pub steps: Vec<ScenarioStep>,
    pub assertions: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct ScenarioStep {
    pub direction: String,
    pub message: Option<serde_json::Value>,
    pub binary: Option<BinaryStep>,
}

#[derive(Debug, Deserialize)]
pub struct BinaryStep {
    #[serde(rename = "frameType")]
    pub frame_type: String,
    pub id: String,
    pub payload_base64: String,
}

pub fn load_scenario(name: &str) -> Scenario {
    let path = format!(
        "{}/protocol/scenarios/v1/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to load scenario {name}: {e}"));
    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("Failed to parse scenario {name}: {e}"))
}
