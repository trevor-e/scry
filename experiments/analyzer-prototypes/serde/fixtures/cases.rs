#![allow(dead_code)]
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RequiredSubset {
    Broad { id: String },
    Narrow { id: String, detail: String },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum IntegerRange {
    Wide { count: u64 },
    Narrow { count: u8 },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OptionalSuperset {
    Broad { id: Option<String> },
    Narrow { id: String },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DefaultSuperset {
    Broad { #[serde(default)] id: String },
    Narrow { id: String },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AnyFirst {
    Broad(serde_json::Value),
    Narrow(String),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RenamedFields {
    Broad { #[serde(rename = "id")] local_name: String },
    Narrow { id: String, detail: String },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SpecificFirst {
    Specific { id: String, detail: String },
    General { id: String },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OptionalUnknownFieldTrap {
    Earlier { optional: Option<String> },
    Later { id: String },
}

#[derive(Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum DeniedUnknown {
    Earlier { id: String },
    Later { id: String, detail: String },
}

#[derive(Debug, Deserialize)]
struct Short { id: String }

#[derive(Debug, Deserialize)]
struct Long { id: String, detail: String }

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum NamedStructSequenceTrap {
    Earlier(Short),
    Later(Long),
}

#[derive(Debug, Deserialize)]
#[serde(untagged, rename_all = "camelCase")]
enum EnumRenameDoesNotRenameFields {
    Earlier { snake_case: String },
    Later { #[serde(rename = "snakeCase")] another: String },
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum SerializeOnly {
    Earlier { id: String },
    Later { id: String, detail: String },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FlattenAbstention {
    Earlier { #[serde(flatten)] rest: std::collections::HashMap<String, String> },
    Later { id: String },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AliasAbstention {
    Earlier { #[serde(alias = "alternate")] id: String },
    Later { id: String },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CustomFieldAbstention {
    Earlier { #[serde(deserialize_with = "deserialize_string")] id: String },
    Later { id: String },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ConditionalAbstention {
    #[cfg(unix)]
    Earlier { id: String },
    Later { id: String },
}

fn deserialize_string<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    String::deserialize(deserializer)
}

#[test]
fn predicted_shadowing_selects_the_earlier_variant() {
    assert!(matches!(serde_json::from_str::<RequiredSubset>(r#"{"id":"x","detail":"y"}"#).unwrap(), RequiredSubset::Broad { .. }));
    assert!(matches!(serde_json::from_str::<IntegerRange>(r#"{"count":255}"#).unwrap(), IntegerRange::Wide { .. }));
    assert!(matches!(serde_json::from_str::<OptionalSuperset>(r#"{"id":"x"}"#).unwrap(), OptionalSuperset::Broad { .. }));
    assert!(matches!(serde_json::from_str::<DefaultSuperset>(r#"{"id":"x"}"#).unwrap(), DefaultSuperset::Broad { .. }));
    assert!(matches!(serde_json::from_str::<AnyFirst>(r#""x""#).unwrap(), AnyFirst::Broad(_)));
    assert!(matches!(serde_json::from_str::<RenamedFields>(r#"{"id":"x","detail":"y"}"#).unwrap(), RenamedFields::Broad { .. }));
}

#[test]
fn intentional_and_nonshadowed_later_variants_remain_reachable() {
    assert!(matches!(serde_json::from_str::<SpecificFirst>(r#"{"id":"x"}"#).unwrap(), SpecificFirst::General { .. }));
    assert!(matches!(serde_json::from_str::<OptionalUnknownFieldTrap>(r#"{"id":"x","optional":42}"#).unwrap(), OptionalUnknownFieldTrap::Later { .. }));
    assert!(matches!(serde_json::from_str::<DeniedUnknown>(r#"{"id":"x","detail":"y"}"#).unwrap(), DeniedUnknown::Later { .. }));
    assert!(matches!(serde_json::from_str::<NamedStructSequenceTrap>(r#"["x","y"]"#).unwrap(), NamedStructSequenceTrap::Later(_)));
    assert!(matches!(serde_json::from_str::<EnumRenameDoesNotRenameFields>(r#"{"snakeCase":"x"}"#).unwrap(), EnumRenameDoesNotRenameFields::Later { .. }));
}
