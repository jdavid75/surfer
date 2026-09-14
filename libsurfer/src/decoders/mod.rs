//! Signal decoders that turn one or more digital signals into structured data.
//!
//! Decoders are defined by a TOML schema ([`DecoderSchema`]) that selects a
//! parameterized [`DecoderEngine`]. Adding a new variant of a supported
//! protocol only requires a schema file; adding a new protocol family requires
//! a new engine.
//!
//! Unlike translators (see [`crate::translation`]), which translate a single
//! signal value into text, decoders can read any number of input signals over
//! time.

pub mod engines;
pub mod schema;

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, OnceLock};

use eyre::Result;
use num::BigUint;
use serde::{Deserialize, Serialize};
use surfer_translation_types::{NumericRange, ValueKind};

use crate::wave_container::{SignalAccessor, SignalId, VariableRef, WaveContainer};

pub use schema::{DecoderSchema, SchemaDecoder};

/// A clock edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edge {
    Rising,
    Falling,
}

impl Edge {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "rising" | "rise" => Some(Self::Rising),
            "falling" | "fall" => Some(Self::Falling),
            _ => None,
        }
    }
}

/// The order in which the bits of a value are transmitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BitOrder {
    MsbFirst,
    LsbFirst,
}

impl BitOrder {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "msb" | "msb_first" => Some(Self::MsbFirst),
            "lsb" | "lsb_first" => Some(Self::LsbFirst),
            _ => None,
        }
    }
}

/// How integer values are displayed as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ValueFormat {
    #[default]
    Decimal,
    Hexadecimal,
}

impl ValueFormat {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Decimal => "Decimal",
            Self::Hexadecimal => "Hexadecimal",
        }
    }
}

/// The decoded value of one item.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodedValue {
    /// An integer with its bit width, formattable as decimal/hex.
    Integer {
        value: i64,
        bits: u32,
    },
    /// Pre-formatted text (e.g. a protocol control word or composite label).
    Text(String),
    Undef,
    HighImp,
    DontCare,
}

impl DecodedValue {
    #[must_use]
    pub fn integer(value: i64, bits: u32) -> Self {
        Self::Integer { value, bits }
    }

    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// Format this value for display.
    #[must_use]
    pub fn format(&self, format: ValueFormat) -> String {
        match self {
            Self::Integer { value, bits } => match format {
                ValueFormat::Decimal => value.to_string(),
                ValueFormat::Hexadecimal => {
                    let bits = (*bits).clamp(1, 64);
                    let mask = if bits == 64 {
                        u64::MAX
                    } else {
                        (1u64 << bits) - 1
                    };
                    format!(
                        "0x{:0width$x}",
                        (*value as u64) & mask,
                        width = bits.div_ceil(4) as usize
                    )
                }
            },
            Self::Text(text) => text.clone(),
            Self::Undef => "x".to_string(),
            Self::HighImp => "z".to_string(),
            Self::DontCare => "-".to_string(),
        }
    }

    /// The color kind used to render this value.
    #[must_use]
    pub fn kind(&self) -> ValueKind {
        match self {
            Self::Integer { .. } | Self::Text(_) => ValueKind::Normal,
            Self::Undef => ValueKind::Undef,
            Self::HighImp => ValueKind::HighImp,
            Self::DontCare => ValueKind::DontCare,
        }
    }

    /// The numeric value, if this item can be plotted as analog data.
    #[must_use]
    pub fn numeric(&self) -> Option<f64> {
        match self {
            Self::Integer { value, .. } => Some(*value as f64),
            _ => None,
        }
    }

    /// The value to plot in analog mode: the number, or a NaN variant that
    /// carries the anomaly kind for coloring.
    #[must_use]
    pub fn analog_value(&self) -> f64 {
        match self {
            Self::Integer { value, .. } => *value as f64,
            Self::Undef => surfer_translation_types::NAN_UNDEF,
            Self::HighImp => surfer_translation_types::NAN_HIGHIMP,
            _ => f64::NAN,
        }
    }
}

/// One decoded item: a sample, a protocol word, or a message.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedItem {
    pub start: BigUint,
    pub end: BigUint,
    pub value: DecodedValue,
}

/// One display row of decoded items.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedRow {
    pub name: String,
    /// Numeric range for analog Y-axis scaling when items carry integer values.
    pub numeric_range: Option<NumericRange>,
    pub items: Vec<DecodedItem>,
}

/// The result of decoding one decoder item.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedData {
    pub rows: Vec<DecodedRow>,
}

/// A value of a decoder setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SettingValue {
    Bool(bool),
    Integer(i64),
    Enum(String),
}

impl SettingValue {
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_enum(&self) -> Option<&str> {
        match self {
            Self::Enum(value) => Some(value),
            _ => None,
        }
    }
}

/// Settings for a decoder instance: named values declared by its schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DecoderSettings {
    values: BTreeMap<String, SettingValue>,
}

impl DecoderSettings {
    #[must_use]
    pub fn from_schema(schema: &SettingsSchema) -> Self {
        Self {
            values: schema
                .fields
                .iter()
                .map(|field| (field.key.clone(), field.default.clone()))
                .collect(),
        }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&SettingValue> {
        self.values.get(key)
    }

    #[must_use]
    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.values
            .get(key)
            .and_then(SettingValue::as_bool)
            .unwrap_or(default)
    }

    #[must_use]
    pub fn get_integer(&self, key: &str, default: i64) -> i64 {
        self.values
            .get(key)
            .and_then(SettingValue::as_integer)
            .unwrap_or(default)
    }

    #[must_use]
    pub fn get_enum(&self, key: &str, default: &str) -> String {
        self.values
            .get(key)
            .and_then(SettingValue::as_enum)
            .unwrap_or(default)
            .to_string()
    }

    pub fn set(&mut self, key: String, value: SettingValue) {
        self.values.insert(key, value);
    }

    #[must_use]
    pub fn values(&self) -> &BTreeMap<String, SettingValue> {
        &self.values
    }
}

/// One option of an enum setting.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingOption {
    pub value: String,
    pub label: String,
}

/// The kind of a setting, which determines how it is edited.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    Bool,
    Integer { min: i64, max: i64 },
    Enum { options: Vec<SettingOption> },
}

/// One setting declared by a decoder schema.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingField {
    pub key: String,
    pub label: String,
    pub default: SettingValue,
    pub kind: SettingKind,
}

/// The settings declared by a decoder schema.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SettingsSchema {
    pub fields: Vec<SettingField>,
}

impl SettingsSchema {
    #[must_use]
    pub fn defaults(&self) -> DecoderSettings {
        DecoderSettings::from_schema(self)
    }

    /// The default value declared for the setting with `key`, if any.
    #[must_use]
    pub fn default_for(&self, key: &str) -> Option<&SettingValue> {
        self.fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| &field.default)
    }

    /// Validate a settings value against the schema.
    pub fn validate(&self, settings: &DecoderSettings) -> Result<()> {
        for field in &self.fields {
            let Some(value) = settings.get(&field.key) else {
                eyre::bail!("Missing setting '{}'", field.label);
            };
            match (&field.kind, value) {
                (SettingKind::Bool, SettingValue::Bool(_)) => {}
                (SettingKind::Integer { min, max }, SettingValue::Integer(value)) => {
                    if value < min || value > max {
                        eyre::bail!("Setting '{}' must be between {min} and {max}", field.label);
                    }
                }
                (SettingKind::Enum { options }, SettingValue::Enum(value)) => {
                    if !options.iter().any(|option| option.value == *value) {
                        eyre::bail!("Invalid value '{value}' for setting '{}'", field.label);
                    }
                }
                _ => eyre::bail!("Setting '{}' has the wrong type", field.label),
            }
        }
        Ok(())
    }
}

/// Description of one input a decoder needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSpec {
    /// Stable identifier used to bind a signal, e.g. `"bitclk"`.
    pub role: String,
    /// Human-readable description shown in the UI.
    pub description: String,
    /// Whether a signal must be assigned to this input.
    pub required: bool,
}

impl InputSpec {
    #[must_use]
    pub fn required(role: &str, description: &str) -> Self {
        Self {
            role: role.to_string(),
            description: description.to_string(),
            required: true,
        }
    }

    #[must_use]
    pub fn optional(role: &str, description: &str) -> Self {
        Self {
            role: role.to_string(),
            description: description.to_string(),
            required: false,
        }
    }
}

/// A signal assigned to one decoder input role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecoderInput {
    pub role: String,
    pub variable_ref: VariableRef,
}

/// A decoder input with its signal resolved and ready to be read.
pub struct DecoderInputSignal {
    pub role: String,
    pub variable_ref: VariableRef,
    pub num_bits: Option<u32>,
    pub accessor: SignalAccessor,
}

/// Everything a decoder needs to decode its inputs.
pub struct DecoderContext<'a> {
    pub inputs: &'a [DecoderInputSignal],
    pub settings: &'a DecoderSettings,
}

impl DecoderContext<'_> {
    /// Find the signal assigned to `role`.
    pub fn input(&self, role: &str) -> Result<&DecoderInputSignal> {
        self.optional_input(role)
            .ok_or_else(|| eyre::eyre!("No signal assigned to decoder input '{role}'"))
    }

    /// Find the signal assigned to `role`, if any.
    #[must_use]
    pub fn optional_input(&self, role: &str) -> Option<&DecoderInputSignal> {
        self.inputs.iter().find(|input| input.role == role)
    }
}

/// Resolve the configured input variables into readable signals.
pub fn resolve_inputs(
    waves: &WaveContainer,
    inputs: &[DecoderInput],
) -> Result<Vec<DecoderInputSignal>> {
    inputs
        .iter()
        .map(|input| {
            let meta = waves.variable_meta(&input.variable_ref)?;
            let accessor = waves.signal_accessor(waves.signal_id(&input.variable_ref)?)?;
            Ok(DecoderInputSignal {
                role: input.role.clone(),
                variable_ref: input.variable_ref.clone(),
                num_bits: meta.num_bits,
                accessor,
            })
        })
        .collect()
}

/// A decoder that turns input signals into structured data.
pub trait SignalDecoder: Send + Sync {
    /// Stable identifier used in state files and commands.
    fn id(&self) -> &str;

    /// Human-readable name shown in the UI.
    fn display_name(&self) -> &str;

    /// The inputs this decoder needs.
    fn inputs(&self) -> &[InputSpec];

    /// The settings this decoder accepts.
    fn settings_schema(&self) -> &SettingsSchema;

    /// Default settings for this decoder.
    fn default_settings(&self) -> DecoderSettings {
        self.settings_schema().defaults()
    }

    /// Validate settings, returning a human-readable error if they are invalid.
    fn validate_settings(&self, settings: &DecoderSettings) -> Result<()>;

    /// Number of display rows for the given inputs and settings.
    fn row_count(&self, inputs: &[DecoderInput], settings: &DecoderSettings) -> usize;

    /// Decode the input signals.
    fn decode(&self, ctx: &DecoderContext<'_>) -> Result<DecodedData>;
}

/// Schema files bundled with Surfer.
static BUILTIN_SCHEMAS: &[&str] = &[
    include_str!("builtin/tdm_audio.toml"),
    include_str!("builtin/spdif.toml"),
];

/// All decoders available to the application.
static DECODERS: LazyLock<Vec<Arc<dyn SignalDecoder>>> = LazyLock::new(|| {
    let mut decoders: Vec<Arc<dyn SignalDecoder>> = Vec::new();

    for text in BUILTIN_SCHEMAS {
        match DecoderSchema::parse(text).and_then(SchemaDecoder::new) {
            Ok(decoder) => decoders.push(Arc::new(decoder)),
            Err(error) => tracing::error!("Failed to load built-in decoder schema: {error:#}"),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    decoders.extend(load_user_decoders());

    debug_assert!(!decoders.is_empty(), "no decoder schemas loaded");

    decoders
});

/// All decoders compiled into or loaded by Surfer.
#[must_use]
pub fn all_decoders() -> &'static [Arc<dyn SignalDecoder>] {
    &DECODERS
}

/// Look up a decoder by its [`SignalDecoder::id`].
#[must_use]
pub fn decoder_by_id(id: &str) -> Option<Arc<dyn SignalDecoder>> {
    DECODERS.iter().find(|decoder| decoder.id() == id).cloned()
}

#[cfg(not(target_arch = "wasm32"))]
fn load_user_decoders() -> Vec<Arc<dyn SignalDecoder>> {
    use std::path::PathBuf;

    use tracing::{error, info, warn};

    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(project_dirs) = crate::config::PROJECT_DIR.as_ref() {
        dirs.push(project_dirs.config_dir().join(SIGNAL_DECODERS_DIR));
    }
    // Search upward for `.surfer` directories, like the configuration loader
    // does, so project-local schemas are found from any subdirectory.
    dirs.extend(
        crate::config::find_local_configs()
            .into_iter()
            .map(|local| local.join(SIGNAL_DECODERS_DIR).into_std_path_buf()),
    );

    let mut decoders: Vec<Arc<dyn SignalDecoder>> = Vec::new();
    for dir in dirs {
        info!("Looking for decoder schemas at {}", dir.display());
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                warn!("Cannot read decoder schema {}", path.display());
                continue;
            };
            match DecoderSchema::parse(&text).and_then(SchemaDecoder::new) {
                Ok(decoder) => {
                    info!(
                        "Loaded decoder schema {} from {}",
                        decoder.id(),
                        path.display()
                    );
                    decoders.push(Arc::new(decoder));
                }
                Err(error) => {
                    error!(
                        "Failed to load decoder schema {}: {error:#}",
                        path.display()
                    );
                }
            }
        }
    }

    decoders
}

#[cfg(not(target_arch = "wasm32"))]
static SIGNAL_DECODERS_DIR: &str = "signal_decoders";

/// Cache key for decoded data: the input signals and the decoder settings.
pub type DecoderCacheKey = (Vec<SignalId>, DecoderSettings);

/// Compute the cache key for a decoder configuration.
pub fn cache_key(
    waves: &WaveContainer,
    inputs: &[DecoderInput],
    settings: &DecoderSettings,
) -> Result<DecoderCacheKey> {
    let signal_ids = inputs
        .iter()
        .map(|input| waves.signal_id(&input.variable_ref))
        .collect::<Result<Vec<_>>>()?;
    Ok((signal_ids, settings.clone()))
}

/// Asynchronously built decode result, shared via `Arc`.
pub struct DecoderCacheEntry {
    inner: OnceLock<std::result::Result<Arc<DecodedData>, String>>,
    pub key: DecoderCacheKey,
    pub generation: u64,
}

impl DecoderCacheEntry {
    #[must_use]
    pub fn new(key: DecoderCacheKey, generation: u64) -> Self {
        Self {
            inner: OnceLock::new(),
            key,
            generation,
        }
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.inner.get().is_some()
    }

    #[must_use]
    pub fn get(&self) -> Option<&std::result::Result<Arc<DecodedData>, String>> {
        self.inner.get()
    }

    pub fn set(&self, result: std::result::Result<Arc<DecodedData>, String>) {
        let _ = self.inner.set(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_entry_stores_result() {
        let key = (vec![], DecoderSettings::default());
        let entry = DecoderCacheEntry::new(key.clone(), 7);

        assert!(!entry.is_ready());
        assert!(entry.get().is_none());

        entry.set(Ok(Arc::new(DecodedData { rows: vec![] })));

        assert!(entry.is_ready());
        assert!(matches!(entry.get(), Some(Ok(_))));
        assert_eq!(entry.generation, 7);
        assert_eq!(entry.key, key);
    }

    #[test]
    fn formats_values() {
        assert_eq!(
            DecodedValue::integer(-1148, 16).format(ValueFormat::Decimal),
            "-1148"
        );
        assert_eq!(
            DecodedValue::integer(-1148, 16).format(ValueFormat::Hexadecimal),
            "0xfb84"
        );
        assert_eq!(
            DecodedValue::integer(0xabc, 24).format(ValueFormat::Hexadecimal),
            "0x000abc"
        );
        assert_eq!(DecodedValue::Undef.format(ValueFormat::Decimal), "x");
    }
}
