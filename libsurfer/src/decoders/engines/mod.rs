//! Decoder engines: parameterized protocol implementations driven by schemas.

pub mod biphase_audio;
pub mod clocked_serial;

use std::collections::BTreeMap;

use eyre::Result;

use super::{DecodedData, DecoderContext, DecoderInput, SettingValue};

/// Parameters passed to a decoder engine.
pub type EngineParams = BTreeMap<String, SettingValue>;

/// A parameterized protocol implementation.
pub trait DecoderEngine: Send + Sync {
    /// Stable engine identifier used in schemas.
    fn id(&self) -> &'static str;

    /// Validate the resolved parameters.
    fn validate(&self, params: &EngineParams) -> Result<()>;

    /// Number of display rows for the given inputs and parameters.
    fn row_count(&self, inputs: &[DecoderInput], params: &EngineParams) -> usize;

    /// Decode the input signals.
    fn decode(&self, params: &EngineParams, ctx: &DecoderContext<'_>) -> Result<DecodedData>;
}

/// All engines compiled into Surfer.
#[must_use]
pub fn all_engines() -> &'static [&'static dyn DecoderEngine] {
    &[
        &clocked_serial::ClockedSerialEngine,
        &biphase_audio::BiphaseAudioEngine,
    ]
}

/// Look up an engine by its [`DecoderEngine::id`].
#[must_use]
pub fn engine_by_id(id: &str) -> Option<&'static dyn DecoderEngine> {
    all_engines()
        .iter()
        .copied()
        .find(|engine| engine.id() == id)
}
