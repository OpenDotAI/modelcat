//! One sourced per-model catalog; a gateway may impose narrower limits.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub id: String,
    pub context_window: i64,
    pub max_output_tokens: i64,
    pub official_max_output_tokens: i64,
    pub input_modalities: Vec<String>,
    pub reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: String,
    pub source_url: String,
    pub wire_apis: Vec<String>,
}
fn catalog() -> Value {
    serde_json::from_slice(include_bytes!("../assets/model-capabilities.json"))
        .expect("valid shipped model catalog")
}
fn base_id(id: &str) -> &str {
    if let Some((base, date)) = id.rsplit_once('-') {
        if date.len() == 8 && date.starts_with("20") && date.bytes().all(|b| b.is_ascii_digit()) {
            return base;
        }
    }
    id
}
fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
pub fn descriptor(item: &Value) -> Option<ModelDescriptor> {
    let id = item["id"].as_str()?;
    let registry = catalog();
    let official = &registry["models"][base_id(id)];
    let gateway = &item["modelcat"];
    let has_gateway = gateway["configured"] == true;
    if !has_gateway && official.is_null() {
        return None;
    }
    let source = if has_gateway { gateway } else { official };
    let context = source["context_window"].as_i64()?;
    let official_output = source["official_max_output_tokens"]
        .as_i64()
        .filter(|n| *n > 0)
        .or_else(|| official["max_output_tokens"].as_i64())
        .unwrap_or(0);
    let output = if has_gateway {
        source["max_output_tokens"].as_i64()?
    } else {
        official["max_output_tokens"].as_i64()?.min(32768)
    };
    if context < 1 || context > 10000000 || output < 1 || output > 1048576 || output > context {
        return None;
    }
    let input = strings(&source["input_modalities"]);
    if input.is_empty() {
        return None;
    }
    let apis = strings(&source["wire_apis"]);
    Some(ModelDescriptor {
        id: id.into(),
        context_window: context,
        max_output_tokens: output,
        official_max_output_tokens: official_output,
        input_modalities: input,
        reasoning_efforts: if has_gateway {
            strings(&source["reasoning_efforts"])
        } else {
            source["default_reasoning_effort"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| vec![s.to_owned()])
                .unwrap_or_default()
        },
        default_reasoning_effort: source["default_reasoning_effort"]
            .as_str()
            .unwrap_or("")
            .into(),
        source_url: source["source_url"].as_str().unwrap_or("").into(),
        wire_apis: apis,
    })
}
pub fn local_output_limit(id: &str) -> i64 {
    catalog()["models"][base_id(id)]["max_output_tokens"]
        .as_i64()
        .unwrap_or(32768)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn model_specific_official_and_gateway_limits_stay_separate() {
        let gpt = descriptor(&json!({"id":"gpt-6-sol"})).unwrap();
        assert_eq!(gpt.context_window, 1050000);
        assert_eq!(gpt.official_max_output_tokens, 128000);
        assert_eq!(gpt.max_output_tokens, 32768);
        assert!(gpt.input_modalities.contains(&"image".into()));
        let deep = descriptor(&json!({"id":"deepseek-v4-pro"})).unwrap();
        assert_eq!(deep.official_max_output_tokens, 393216);
        assert_eq!(deep.input_modalities, vec!["text"]);
        assert!(descriptor(&json!({"id":"unknown-model"})).is_none());
        let mut value = json!({"id":"gpt-6-sol","modelcat":{"configured":true,"context_window":64000,"max_output_tokens":8000,"input_modalities":["text"],"source_url":"https://example.invalid/spec","wire_apis":["openai-responses"]}});
        assert_eq!(descriptor(&value).unwrap().max_output_tokens, 8000);
        value["modelcat"]["max_output_tokens"] = json!(99999999);
        assert!(descriptor(&value).is_none());
    }
}
