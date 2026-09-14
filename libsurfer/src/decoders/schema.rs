//! TOML-defined decoder schemas.
//!
//! A schema declares a decoder's id, name, inputs, settings, and the
//! parameters passed to a [`DecoderEngine`](super::engines::DecoderEngine).

use std::collections::BTreeMap;

use eyre::{Result, bail};
use serde::Deserialize;

use super::engines::{DecoderEngine, EngineParams, engine_by_id};
use super::{
    DecodedData, DecoderContext, DecoderInput, DecoderSettings, InputSpec, SettingField,
    SettingKind, SettingOption, SettingValue, SettingsSchema, SignalDecoder,
};

/// Where an engine parameter comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamSource {
    /// Resolved from the setting with this key.
    Setting(String),
    /// A literal value.
    Literal(SettingValue),
}

/// A decoder defined by a TOML schema.
#[derive(Debug, Clone)]
pub struct DecoderSchema {
    pub id: String,
    pub name: String,
    pub engine: String,
    pub inputs: Vec<InputSpec>,
    pub settings: SettingsSchema,
    pub params: BTreeMap<String, ParamSource>,
}

impl DecoderSchema {
    /// Parse a schema from TOML text.
    pub fn parse(text: &str) -> Result<Self> {
        let raw: RawSchema = toml::from_str(text)?;

        let inputs = raw
            .inputs
            .into_iter()
            .map(|input| InputSpec {
                role: input.role,
                description: input.description,
                required: input.required,
            })
            .collect::<Vec<_>>();

        let mut fields = Vec::new();
        for setting in raw.settings {
            let default = setting_value(&setting.default).ok_or_else(|| {
                eyre::eyre!("Invalid default value for setting '{}'", setting.key)
            })?;
            let kind = match setting.kind.as_str() {
                "bool" => SettingKind::Bool,
                "integer" => SettingKind::Integer {
                    min: setting.min.unwrap_or(i64::MIN),
                    max: setting.max.unwrap_or(i64::MAX),
                },
                "enum" => SettingKind::Enum {
                    options: setting
                        .options
                        .into_iter()
                        .map(|option| SettingOption {
                            value: option.value,
                            label: option.label,
                        })
                        .collect(),
                },
                other => bail!("Unknown setting type '{other}'"),
            };
            fields.push(SettingField {
                key: setting.key,
                label: setting.label,
                default,
                kind,
            });
        }

        let mut params = BTreeMap::new();
        for (key, value) in raw.engine_params {
            let source = match &value {
                toml::Value::String(text) if text.starts_with('$') => {
                    let setting = text[1..].to_string();
                    if !fields.iter().any(|field| field.key == setting) {
                        bail!("Engine parameter '{key}' references unknown setting '{setting}'");
                    }
                    ParamSource::Setting(setting)
                }
                _ => ParamSource::Literal(
                    setting_value(&value)
                        .ok_or_else(|| eyre::eyre!("Invalid engine parameter '{key}'"))?,
                ),
            };
            params.insert(key, source);
        }

        let schema = Self {
            id: raw.id,
            name: raw.name,
            engine: raw.engine,
            inputs,
            settings: SettingsSchema { fields },
            params,
        };

        // Fail early if the engine or engine parameters are invalid.
        engine_by_id(&schema.engine)
            .ok_or_else(|| eyre::eyre!("Unknown decoder engine '{}'", schema.engine))?;
        let defaults = schema.settings.defaults();
        let _ = schema.resolve_params(&defaults);

        Ok(schema)
    }

    /// Resolve the engine parameters for the given settings.
    ///
    /// A setting missing from `settings` (e.g. one added to the schema after
    /// the settings were stored in a state file) falls back to the schema
    /// default rather than an empty enum value.
    #[must_use]
    pub fn resolve_params(&self, settings: &DecoderSettings) -> EngineParams {
        self.params
            .iter()
            .map(|(key, source)| {
                let value = match source {
                    ParamSource::Setting(setting) => settings
                        .get(setting)
                        .or_else(|| self.settings.default_for(setting))
                        .cloned()
                        .unwrap_or(SettingValue::Enum(String::new())),
                    ParamSource::Literal(value) => value.clone(),
                };
                (key.clone(), value)
            })
            .collect()
    }
}

/// A [`SignalDecoder`] backed by a schema and an engine.
pub struct SchemaDecoder {
    schema: DecoderSchema,
    engine: &'static dyn DecoderEngine,
}

impl SchemaDecoder {
    pub fn new(schema: DecoderSchema) -> Result<Self> {
        let engine = engine_by_id(&schema.engine)
            .ok_or_else(|| eyre::eyre!("Unknown decoder engine '{}'", schema.engine))?;
        Ok(Self { schema, engine })
    }

    #[must_use]
    pub fn schema(&self) -> &DecoderSchema {
        &self.schema
    }
}

impl SignalDecoder for SchemaDecoder {
    fn id(&self) -> &str {
        &self.schema.id
    }

    fn display_name(&self) -> &str {
        &self.schema.name
    }

    fn inputs(&self) -> &[InputSpec] {
        &self.schema.inputs
    }

    fn settings_schema(&self) -> &SettingsSchema {
        &self.schema.settings
    }

    fn validate_settings(&self, settings: &DecoderSettings) -> Result<()> {
        self.schema.settings.validate(settings)?;
        let params = self.schema.resolve_params(settings);
        self.engine.validate(&params)
    }

    fn row_count(&self, inputs: &[DecoderInput], settings: &DecoderSettings) -> usize {
        let params = self.schema.resolve_params(settings);
        self.engine.row_count(inputs, &params)
    }

    fn decode(&self, ctx: &DecoderContext<'_>) -> Result<DecodedData> {
        let params = self.schema.resolve_params(ctx.settings);
        self.engine.decode(&params, ctx)
    }
}

#[derive(Debug, Deserialize)]
struct RawSchema {
    id: String,
    name: String,
    engine: String,
    #[serde(default)]
    inputs: Vec<RawInput>,
    #[serde(default)]
    settings: Vec<RawSetting>,
    #[serde(default)]
    engine_params: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawInput {
    role: String,
    description: String,
    #[serde(default = "default_true")]
    required: bool,
}

#[derive(Debug, Deserialize)]
struct RawSetting {
    key: String,
    label: String,
    #[serde(rename = "type")]
    kind: String,
    default: toml::Value,
    #[serde(default)]
    min: Option<i64>,
    #[serde(default)]
    max: Option<i64>,
    #[serde(default)]
    options: Vec<RawOption>,
}

#[derive(Debug, Deserialize)]
struct RawOption {
    value: String,
    label: String,
}

fn default_true() -> bool {
    true
}

fn setting_value(value: &toml::Value) -> Option<SettingValue> {
    match value {
        toml::Value::Boolean(value) => Some(SettingValue::Bool(*value)),
        toml::Value::Integer(value) => Some(SettingValue::Integer(*value)),
        toml::Value::String(value) => Some(SettingValue::Enum(value.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_schemas_parse_and_resolve() {
        let mut saw_tdm_audio = false;
        let mut saw_spdif = false;
        for text in super::super::BUILTIN_SCHEMAS {
            let schema = DecoderSchema::parse(text).expect("built-in schema should parse");
            let defaults = schema.settings.defaults();
            let params = schema.resolve_params(&defaults);
            for key in schema.params.keys() {
                assert!(
                    params.contains_key(key),
                    "schema {} did not resolve param {key}",
                    schema.id
                );
            }

            match schema.id.as_str() {
                "tdm_audio" => {
                    saw_tdm_audio = true;
                    assert_eq!(
                        params
                            .get("frame_active_high")
                            .and_then(SettingValue::as_bool),
                        Some(true)
                    );
                    assert_eq!(
                        params.get("bits").and_then(SettingValue::as_integer),
                        Some(16)
                    );

                    let mut settings = defaults;
                    settings.set("active_high".to_string(), SettingValue::Bool(false));
                    settings.set("bits".to_string(), SettingValue::Integer(24));
                    let params = schema.resolve_params(&settings);
                    assert_eq!(
                        params
                            .get("frame_active_high")
                            .and_then(SettingValue::as_bool),
                        Some(false)
                    );
                    assert_eq!(
                        params.get("bits").and_then(SettingValue::as_integer),
                        Some(24)
                    );
                }
                "spdif" => {
                    saw_spdif = true;
                    assert_eq!(
                        params.get("word_bits").and_then(SettingValue::as_enum),
                        Some("auto")
                    );
                    assert_eq!(
                        params.get("signed").and_then(SettingValue::as_bool),
                        Some(true)
                    );
                    assert_eq!(
                        params.get("show_preambles").and_then(SettingValue::as_bool),
                        Some(true)
                    );
                }
                other => panic!("unexpected built-in schema '{other}'"),
            }
        }
        assert!(saw_tdm_audio, "tdm_audio schema not found");
        assert!(saw_spdif, "spdif schema not found");
    }

    #[test]
    fn rejects_unknown_engine() {
        let text = r#"
id = "x"
name = "X"
engine = "does_not_exist"
"#;
        assert!(DecoderSchema::parse(text).is_err());
    }

    #[test]
    fn rejects_unknown_setting_reference() {
        let text = r#"
id = "x"
name = "X"
engine = "clocked_serial"

[[settings]]
key = "bits"
label = "Bits"
type = "integer"
default = 16

[engine_params]
clock = "clock"
data = "data"
frame = "frame"
bits = "$bitss"
"#;
        assert!(DecoderSchema::parse(text).is_err());
    }

    #[test]
    fn missing_settings_fall_back_to_defaults() {
        let text = r#"
id = "x"
name = "X"
engine = "clocked_serial"

[[settings]]
key = "bits"
label = "Bits"
type = "integer"
default = 24

[engine_params]
clock = "clock"
data = "data"
frame = "frame"
bits = "$bits"
"#;
        let schema = DecoderSchema::parse(text).expect("schema should parse");
        // An empty settings map stands in for a state file that predates the
        // setting: resolution must use the schema default, not an empty enum.
        let params = schema.resolve_params(&DecoderSettings::default());

        assert_eq!(
            params.get("bits").and_then(SettingValue::as_integer),
            Some(24)
        );
    }
}
