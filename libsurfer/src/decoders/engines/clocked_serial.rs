//! Clocked serial audio engine (TDM / I2S).

use std::iter::Peekable;

use eyre::{Result, bail};
use num::BigUint;
use serde::{Deserialize, Serialize};
use surfer_translation_types::{NumericRange, VariableValue};

use crate::decoders::engines::{DecoderEngine, EngineParams};
use crate::decoders::{
    BitOrder, DecodedData, DecodedItem, DecodedRow, DecodedValue, DecoderContext, DecoderInput,
    DecoderInputSignal, Edge, SettingValue,
};

/// How the frame sync signal marks channel slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameSyncMode {
    /// A pulse marks the start of a frame; the channels follow in sequence.
    Pulse,
    /// The level of the frame sync selects the channel (I2S-style).
    Level,
}

/// Where a sample is positioned within its slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Justification {
    /// The sample starts at the beginning of the slot (left-justified).
    Left,
    /// The sample ends at the end of the slot (right-justified).
    Right,
}

/// Parameters for the clocked serial engine.
#[derive(Debug, Clone, PartialEq)]
pub struct ClockedSerialParams {
    /// Input role carrying the bit clock.
    pub clock: String,
    pub clock_edge: Edge,
    /// Input role carrying the serial data.
    pub data: String,
    /// Input role carrying the frame sync / word select.
    pub frame: String,
    pub frame_mode: FrameSyncMode,
    pub frame_edge: Edge,
    /// Active frame-sync level in [`FrameSyncMode::Level`]; pulse mode selects
    /// the frame start with `frame_edge` instead.
    pub frame_active_high: bool,
    /// Bits per sample.
    pub bits: u32,
    /// Channels (slots) per frame.
    pub channels: u32,
    pub bit_order: BitOrder,
    pub justification: Justification,
    /// Clock cycles between a frame sync edge and the start of the slot
    /// window. The sample is justified within the window.
    pub offset: u32,
    pub signed: bool,
    /// Add an error row for frames that end before every configured channel
    /// has been read.
    pub show_errors: bool,
    /// Clock cycles per slot. Defaults to `bits`.
    pub slot_width: Option<u32>,
}

/// Upper bound on channels, to keep user schemas from requesting enormous
/// allocations. Well above any real TDM bus.
const MAX_CHANNELS: u32 = 1024;

/// Upper bound on incomplete-frame errors reported per decode, so a
/// misconfigured bus cannot flood the error row.
const MAX_FRAME_ERRORS: usize = 100;

impl ClockedSerialParams {
    fn slot_width(&self) -> u32 {
        self.slot_width.unwrap_or(self.bits)
    }

    /// Resolve the typed parameters from generic engine parameters.
    pub fn from_params(params: &EngineParams) -> Result<Self> {
        let string = |key: &str| -> Option<String> {
            params
                .get(key)
                .and_then(SettingValue::as_enum)
                .map(str::to_string)
        };
        let integer = |key: &str, default: i64| -> i64 {
            params
                .get(key)
                .and_then(SettingValue::as_integer)
                .unwrap_or(default)
        };
        let boolean = |key: &str, default: bool| -> bool {
            params
                .get(key)
                .and_then(SettingValue::as_bool)
                .unwrap_or(default)
        };

        let clock =
            string("clock").ok_or_else(|| eyre::eyre!("Missing engine parameter 'clock'"))?;
        let data = string("data").ok_or_else(|| eyre::eyre!("Missing engine parameter 'data'"))?;
        let frame =
            string("frame").ok_or_else(|| eyre::eyre!("Missing engine parameter 'frame'"))?;

        let clock_edge = Edge::parse(&string("clock_edge").unwrap_or_else(|| "rising".into()))
            .ok_or_else(|| eyre::eyre!("Invalid 'clock_edge' parameter"))?;
        let frame_edge = Edge::parse(&string("frame_edge").unwrap_or_else(|| "rising".into()))
            .ok_or_else(|| eyre::eyre!("Invalid 'frame_edge' parameter"))?;
        let bit_order = BitOrder::parse(&string("bit_order").unwrap_or_else(|| "msb".into()))
            .ok_or_else(|| eyre::eyre!("Invalid 'bit_order' parameter"))?;
        let frame_mode = match string("frame_mode")
            .unwrap_or_else(|| "pulse".into())
            .as_str()
        {
            "pulse" => FrameSyncMode::Pulse,
            "level" => FrameSyncMode::Level,
            _ => bail!("Invalid 'frame_mode' parameter"),
        };
        let justification = match string("justification")
            .unwrap_or_else(|| "left".into())
            .as_str()
        {
            "left" => Justification::Left,
            "right" => Justification::Right,
            _ => bail!("Invalid 'justification' parameter"),
        };

        let bits = integer("bits", 16).clamp(0, u32::MAX as i64) as u32;
        let channels = integer("channels", 2).clamp(0, u32::MAX as i64) as u32;
        let offset = integer("offset", 0).clamp(0, u32::MAX as i64) as u32;
        let slot_width = match integer("slot_width", 0) {
            value if value > 0 => Some(value.clamp(1, u32::MAX as i64) as u32),
            _ => None,
        };

        Ok(Self {
            clock,
            clock_edge,
            data,
            frame,
            frame_mode,
            frame_edge,
            frame_active_high: boolean("frame_active_high", true),
            bits,
            channels,
            bit_order,
            justification,
            offset,
            signed: boolean("signed", true),
            show_errors: boolean("show_errors", true),
            slot_width,
        })
    }

    /// Validate the parameters.
    pub fn validate(&self) -> Result<()> {
        if self.bits == 0 || self.bits > 64 {
            bail!("bits must be between 1 and 64");
        }
        if self.channels == 0 {
            bail!("channels must be at least 1");
        }
        if self.channels > MAX_CHANNELS {
            bail!("channels must be at most {MAX_CHANNELS}");
        }
        if self.frame_mode == FrameSyncMode::Level && self.channels != 2 {
            bail!("level frame sync mode supports exactly 2 channels");
        }
        if let Some(slot_width) = self.slot_width
            && slot_width < self.bits
        {
            bail!("slot_width must be at least bits");
        }
        Ok(())
    }
}

impl Default for ClockedSerialParams {
    fn default() -> Self {
        Self {
            clock: "bitclk".to_string(),
            clock_edge: Edge::Rising,
            data: "data".to_string(),
            frame: "frame_sync".to_string(),
            frame_mode: FrameSyncMode::Pulse,
            frame_edge: Edge::Rising,
            frame_active_high: true,
            bits: 16,
            channels: 2,
            bit_order: BitOrder::MsbFirst,
            justification: Justification::Left,
            offset: 0,
            signed: true,
            show_errors: true,
            slot_width: None,
        }
    }
}

/// Engine for TDM/I2S-style clocked serial audio.
pub struct ClockedSerialEngine;

impl DecoderEngine for ClockedSerialEngine {
    fn id(&self) -> &'static str {
        "clocked_serial"
    }

    fn validate(&self, params: &EngineParams) -> Result<()> {
        ClockedSerialParams::from_params(params)?.validate()
    }

    fn row_count(&self, _inputs: &[DecoderInput], params: &EngineParams) -> usize {
        ClockedSerialParams::from_params(params)
            .map(|params| params.channels as usize + usize::from(params.show_errors))
            .unwrap_or(0)
    }

    fn decode(&self, params: &EngineParams, ctx: &DecoderContext<'_>) -> Result<DecodedData> {
        self.decode_params(&ClockedSerialParams::from_params(params)?, ctx)
    }
}

impl ClockedSerialEngine {
    /// Decode with already-resolved typed parameters.
    pub fn decode_params(
        &self,
        params: &ClockedSerialParams,
        ctx: &DecoderContext<'_>,
    ) -> Result<DecodedData> {
        type SignalChanges<'a> = Peekable<Box<dyn Iterator<Item = (u64, VariableValue)> + 'a>>;

        params.validate()?;

        let clock = ctx.input(&params.clock)?;
        let frame = ctx.input(&params.frame)?;
        let data = ctx.input(&params.data)?;

        check_single_bit(clock, &params.clock)?;
        check_single_bit(frame, &params.frame)?;
        check_single_bit(data, &params.data)?;

        let mut events: Vec<(InputRole, SignalChanges<'_>)> = vec![
            (
                InputRole::FrameSync,
                frame.accessor.iter_changes().peekable(),
            ),
            (InputRole::Data, data.accessor.iter_changes().peekable()),
            (InputRole::BitClk, clock.accessor.iter_changes().peekable()),
        ];

        let mut state = DecoderState::new(params);

        loop {
            let mut best: Option<(u64, usize)> = None;
            for (index, (_, iter)) in events.iter_mut().enumerate() {
                if let Some((time, _)) = iter.peek() {
                    let key = (*time, index);
                    let is_better = match best {
                        None => true,
                        Some(current) => key < current,
                    };
                    if is_better {
                        best = Some(key);
                    }
                }
            }

            let Some((_, index)) = best else {
                break;
            };

            let (time, value) = events[index].1.next().expect("event was peeked above");
            let bit = bit_of(&value);
            match events[index].0 {
                InputRole::FrameSync => state.on_frame_sync(time, bit),
                InputRole::Data => state.data = bit,
                InputRole::BitClk => state.on_bitclk(time, bit),
            }
        }

        Ok(state.finish())
    }
}

#[derive(Clone, Copy)]
enum InputRole {
    FrameSync,
    Data,
    BitClk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bit {
    Zero,
    One,
    Undef,
    HighImp,
    DontCare,
    Weak,
}

fn bit_of(value: &VariableValue) -> Bit {
    match value {
        VariableValue::BigUint(value) => {
            if *value == BigUint::from(0u8) {
                Bit::Zero
            } else if *value == BigUint::from(1u8) {
                Bit::One
            } else {
                Bit::Undef
            }
        }
        VariableValue::String(value) => match value.chars().next() {
            Some('0') => Bit::Zero,
            Some('1') => Bit::One,
            Some('x' | 'X' | 'u' | 'U') => Bit::Undef,
            Some('z' | 'Z') => Bit::HighImp,
            Some('-') => Bit::DontCare,
            Some('w' | 'W' | 'h' | 'H' | 'l' | 'L') => Bit::Weak,
            _ => Bit::Undef,
        },
    }
}

fn is_edge(previous: Bit, current: Bit, edge: Edge) -> bool {
    match edge {
        Edge::Rising => current == Bit::One && previous != Bit::One,
        Edge::Falling => current == Bit::Zero && previous != Bit::Zero,
    }
}

fn check_single_bit(input: &DecoderInputSignal, role: &str) -> Result<()> {
    if input.num_bits != Some(1) {
        let bits = input.num_bits.map_or_else(
            || "an unknown number of".to_string(),
            |bits| bits.to_string(),
        );
        bail!(
            "decoder input '{role}' must be a 1-bit signal, but '{}' has {bits} bits",
            input.variable_ref.name,
        );
    }
    Ok(())
}

struct DecoderState {
    params: ClockedSerialParams,
    bitclk_previous: Option<Bit>,
    frame_sync_previous: Option<Bit>,
    data: Bit,
    frame_active: bool,
    slot: u32,
    bits_read: u32,
    padding_remaining: u32,
    accum: u64,
    anomaly: Option<DecodedValue>,
    sample_start: u64,
    sample_last_bit: u64,
    /// Start of the current frame (pulse mode) or slot (level mode), used for
    /// error spans.
    frame_start: u64,
    /// Samples emitted in the current frame, for the incomplete-frame message.
    emitted_in_frame: u32,
    /// Incomplete-frame errors emitted so far, and whether the cap has been
    /// reported.
    frame_errors: usize,
    frame_errors_truncated: bool,
    errors_row: Option<usize>,
    rows: Vec<Vec<DecodedItem>>,
}

impl DecoderState {
    fn new(params: &ClockedSerialParams) -> Self {
        Self {
            params: params.clone(),
            bitclk_previous: None,
            frame_sync_previous: None,
            data: Bit::Zero,
            frame_active: false,
            slot: 0,
            bits_read: 0,
            padding_remaining: 0,
            accum: 0,
            anomaly: None,
            sample_start: 0,
            sample_last_bit: 0,
            frame_start: 0,
            emitted_in_frame: 0,
            frame_errors: 0,
            frame_errors_truncated: false,
            errors_row: params.show_errors.then_some(params.channels as usize),
            rows: vec![Vec::new(); params.channels as usize + usize::from(params.show_errors)],
        }
    }

    fn on_frame_sync(&mut self, time: u64, bit: Bit) {
        let previous = self.frame_sync_previous.replace(bit);
        match self.params.frame_mode {
            FrameSyncMode::Pulse => {
                let starts_frame = match previous {
                    Some(previous) => is_edge(previous, bit, self.params.frame_edge),
                    None => {
                        bit == match self.params.frame_edge {
                            Edge::Rising => Bit::One,
                            Edge::Falling => Bit::Zero,
                        }
                    }
                };
                if starts_frame {
                    if self.frame_active {
                        self.note_incomplete_frame(time);
                    }
                    self.start_slot(time, 0);
                }
            }
            FrameSyncMode::Level => {
                if previous == Some(bit) {
                    return;
                }
                let channel = match (bit, self.params.frame_active_high) {
                    (Bit::One, true) | (Bit::Zero, false) => 0,
                    (Bit::One, false) | (Bit::Zero, true) => 1,
                    _ => return,
                };
                if self.frame_active {
                    self.note_incomplete_frame(time);
                }
                self.start_slot(time, channel);
            }
        }
    }

    /// Record a frame (pulse mode) or sample (level mode) that ended before
    /// all of its data was read, e.g. because the frame sync period is too
    /// short for the configured `channels`/`bits`/`slot_width`.
    fn note_incomplete_frame(&mut self, time: u64) {
        let Some(row) = self.errors_row else {
            return;
        };
        if self.frame_errors_truncated {
            return;
        }
        if self.frame_errors >= MAX_FRAME_ERRORS {
            self.rows[row].push(DecodedItem {
                start: BigUint::from(self.frame_start),
                end: BigUint::from(time),
                value: DecodedValue::text("further frame errors suppressed"),
            });
            self.frame_errors_truncated = true;
            return;
        }
        let value = match self.params.frame_mode {
            FrameSyncMode::Pulse => DecodedValue::text(format!(
                "incomplete frame: {} of {} channels",
                self.emitted_in_frame, self.params.channels
            )),
            FrameSyncMode::Level => DecodedValue::text("incomplete sample"),
        };
        self.rows[row].push(DecodedItem {
            start: BigUint::from(self.frame_start),
            end: BigUint::from(time),
            value,
        });
        self.frame_errors += 1;
    }

    fn start_slot(&mut self, time: u64, channel: u32) {
        self.frame_active = true;
        self.slot = channel;
        self.bits_read = 0;
        self.padding_remaining =
            self.params
                .offset
                .saturating_add(match self.params.justification {
                    Justification::Left => 0,
                    Justification::Right => {
                        self.params.slot_width().saturating_sub(self.params.bits)
                    }
                });
        self.accum = 0;
        self.anomaly = None;
        self.frame_start = time;
        self.sample_start = time;
        self.emitted_in_frame = 0;
        self.sample_last_bit = time;
    }

    fn on_bitclk(&mut self, time: u64, bit: Bit) {
        let Some(previous) = self.bitclk_previous.replace(bit) else {
            return;
        };
        if !is_edge(previous, bit, self.params.clock_edge) || !self.frame_active {
            return;
        }
        if self.padding_remaining > 0 {
            self.padding_remaining -= 1;
            return;
        }
        self.read_bit(time, self.data);
    }

    fn read_bit(&mut self, time: u64, data: Bit) {
        self.sample_last_bit = time;

        match data {
            Bit::Zero | Bit::One => {
                let bit = u64::from(data == Bit::One);
                match self.params.bit_order {
                    BitOrder::MsbFirst => self.accum = (self.accum << 1) | bit,
                    BitOrder::LsbFirst => self.accum |= bit << self.bits_read,
                }
            }
            Bit::Undef | Bit::Weak => self.note_anomaly(DecodedValue::Undef),
            Bit::HighImp => self.note_anomaly(DecodedValue::HighImp),
            Bit::DontCare => self.note_anomaly(DecodedValue::DontCare),
        }

        self.bits_read += 1;
        if self.bits_read == self.params.bits {
            self.emit_sample();
        }
    }

    fn note_anomaly(&mut self, anomaly: DecodedValue) {
        fn priority(value: &DecodedValue) -> u8 {
            match value {
                DecodedValue::HighImp => 3,
                DecodedValue::Undef => 2,
                DecodedValue::DontCare => 1,
                _ => 0,
            }
        }

        let replace = match &self.anomaly {
            None => true,
            Some(current) => priority(&anomaly) > priority(current),
        };
        if replace {
            self.anomaly = Some(anomaly);
        }
    }

    fn emit_sample(&mut self) {
        let value = match self.anomaly.take() {
            Some(anomaly) => anomaly,
            None => {
                let raw = self.accum;
                let bits = self.params.bits;
                if self.params.signed && bits < 64 {
                    let shift = 64 - bits;
                    DecodedValue::integer(((raw << shift) as i64) >> shift, bits)
                } else {
                    DecodedValue::integer(raw as i64, bits)
                }
            }
        };

        self.rows[self.slot as usize].push(DecodedItem {
            start: BigUint::from(self.sample_start),
            end: BigUint::from(self.sample_last_bit),
            value,
        });
        self.emitted_in_frame += 1;

        self.accum = 0;
        self.bits_read = 0;

        match self.params.frame_mode {
            FrameSyncMode::Pulse => {
                let next_slot = self.slot + 1;
                if next_slot < self.params.channels {
                    self.slot = next_slot;
                    // Every sample in a frame is displayed from the frame-sync
                    // edge, so values change together on the frame boundary.
                    self.sample_start = self.frame_start;
                    self.padding_remaining =
                        self.params.slot_width().saturating_sub(self.params.bits);
                } else {
                    self.frame_active = false;
                }
            }
            FrameSyncMode::Level => {
                self.frame_active = false;
            }
        }
    }

    fn finish(self) -> DecodedData {
        let numeric_range = if self.params.bits <= 63 {
            Some(if self.params.signed {
                let half = 2.0f64.powi((self.params.bits - 1) as i32);
                NumericRange {
                    min: -half,
                    max: half - 1.0,
                }
            } else {
                NumericRange {
                    min: 0.0,
                    max: 2.0f64.powi(self.params.bits as i32) - 1.0,
                }
            })
        } else {
            None
        };

        let errors_row = self.errors_row;
        DecodedData {
            rows: self
                .rows
                .into_iter()
                .enumerate()
                .map(|(index, items)| DecodedRow {
                    name: if errors_row == Some(index) {
                        "errors".to_string()
                    } else {
                        format!("ch{index}")
                    },
                    numeric_range,
                    items,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Arc;

    use surver::WELLEN_SURFER_DEFAULT_OPTIONS;

    use super::*;
    use crate::decoders::{DecodedValue, DecoderInput, resolve_inputs};
    use crate::wave_container::{VariableRef, VariableRefExt, WaveContainer};
    use crate::wellen::{BodyResult, LoadSignalPayload, LoadSignalsResult};

    fn raw_vcd(changes: &str) -> String {
        format!(
            "$timescale 1ns $end\n\
             $scope module tb $end\n\
             $var wire 1 a bitclk $end\n\
             $var wire 1 b frame_sync $end\n\
             $var wire 1 c data $end\n\
             $upscope $end\n\
             $enddefinitions $end\n\
             {changes}"
        )
    }

    fn binary_vcd(events: &[(u64, u8, u8, u8)]) -> String {
        let mut changes = String::new();
        for (time, bitclk, frame_sync, data) in events {
            changes.push_str(&format!("#{time}\n{bitclk}a\n{frame_sync}b\n{data}c\n"));
        }
        raw_vcd(&changes)
    }

    /// A pulse-mode waveform. `bits` are the transmitted sample bits; the
    /// generator inserts `offset` padding clock cycles before the first bit.
    fn pulse_tdm_vcd(bits: &[u8], offset: u32) -> String {
        pulse_tdm_frames(&[bits], offset)
    }

    /// A pulse-mode waveform with one frame sync pulse per frame.
    fn pulse_tdm_frames(frames: &[&[u8]], offset: u32) -> String {
        let mut events = Vec::new();
        let mut time = 0u64;
        for bits in frames {
            let edges = offset + bits.len() as u32;
            for edge in 0..edges {
                let data = if edge < offset {
                    0
                } else {
                    bits[(edge - offset) as usize]
                };
                let frame_sync = u8::from(edge == 0);
                events.push((time, 0, frame_sync, data));
                time += 1;
                events.push((time, 1, frame_sync, data));
                time += 1;
            }
        }
        binary_vcd(&events)
    }

    /// A level-mode (I2S-style) waveform where channel 0 is selected by the low
    /// frame sync level and channel 1 by the high level.
    fn level_tdm_vcd(channel_bits: [&[u8]; 2], offset: u32) -> String {
        let mut events = Vec::new();
        let mut time = 0u64;
        for (channel, bits) in channel_bits.iter().enumerate() {
            let frame_sync = channel as u8;
            let edges = offset + bits.len() as u32;
            for edge in 0..edges {
                let data = if edge < offset {
                    0
                } else {
                    bits[(edge - offset) as usize]
                };
                events.push((time, 0, frame_sync, data));
                time += 1;
                events.push((time, 1, frame_sync, data));
                time += 1;
            }
        }
        binary_vcd(&events)
    }

    fn load_container(vcd: &str) -> WaveContainer {
        load_container_bytes(vcd.as_bytes().to_vec())
    }

    fn load_container_bytes(bytes: Vec<u8>) -> WaveContainer {
        let header =
            wellen::viewers::read_header(Cursor::new(bytes), &WELLEN_SURFER_DEFAULT_OPTIONS)
                .expect("failed to read wave header");
        let hierarchy = Arc::new(header.hierarchy);
        let mut container = WaveContainer::new_waveform(hierarchy.clone());

        let refs =
            ["tb.bitclk", "tb.frame_sync", "tb.data"].map(VariableRef::from_hierarchy_string);
        container
            .load_variables(refs.into_iter())
            .expect("failed to queue signals");

        let body = wellen::viewers::read_body(header.body, &hierarchy, None)
            .expect("failed to read wave body");
        let WaveContainer::Wellen(waves) = &mut container else {
            panic!("expected a wellen container");
        };
        let cmd = waves
            .add_body(BodyResult::Local(body))
            .expect("failed to add body")
            .expect("expected a load signals command");

        let (signals, from_unique_id, payload) = cmd.destruct();
        let LoadSignalPayload::Local(mut source, hierarchy) = payload else {
            panic!("expected local signal payload");
        };
        let loaded = source.load_signals(&signals, &hierarchy, true);
        let result = LoadSignalsResult::local(source, loaded, from_unique_id);
        container
            .on_signals_loaded(result)
            .expect("failed to install signals");

        container
    }

    fn decode_container(container: &WaveContainer, params: ClockedSerialParams) -> DecodedData {
        let inputs = ["bitclk", "frame_sync", "data"]
            .iter()
            .map(|role| DecoderInput {
                role: (*role).to_string(),
                variable_ref: VariableRef::from_hierarchy_string(&format!("tb.{role}")),
            })
            .collect::<Vec<_>>();
        let resolved = resolve_inputs(container, &inputs).expect("resolve inputs");
        ClockedSerialEngine
            .decode_params(
                &params,
                &DecoderContext {
                    inputs: &resolved,
                    settings: &crate::decoders::DecoderSettings::default(),
                },
            )
            .expect("decoding failed")
    }

    fn decode(vcd: &str, params: ClockedSerialParams) -> DecodedData {
        decode_container(&load_container(vcd), params)
    }

    fn numeric(value: &DecodedValue) -> i64 {
        match value {
            DecodedValue::Integer { value, .. } => *value,
            other => panic!("expected an integer value, got {other:?}"),
        }
    }

    #[test]
    fn decodes_pulse_mode_msb_first() {
        let bits = [1, 0, 1, 0, 0, 1, 1, 0];
        let params = ClockedSerialParams {
            bits: 4,
            channels: 2,
            signed: false,
            ..Default::default()
        };

        let decoded = decode(&pulse_tdm_vcd(&bits, 0), params);

        assert_eq!(decoded.rows.len(), 3);
        assert_eq!(decoded.rows[0].name, "ch0");
        assert_eq!(decoded.rows[0].items.len(), 1);
        assert_eq!(numeric(&decoded.rows[0].items[0].value), 0b1010);
        assert_eq!(decoded.rows[0].items[0].start, BigUint::from(0u32));
        assert_eq!(decoded.rows[0].items[0].end, BigUint::from(7u32));
        assert_eq!(numeric(&decoded.rows[1].items[0].value), 0b0110);
        assert_eq!(decoded.rows[1].items[0].start, BigUint::from(0u32));
        assert_eq!(decoded.rows[1].items[0].end, BigUint::from(15u32));
        assert_eq!(decoded.rows[2].name, "errors");
        assert!(decoded.rows[2].items.is_empty());
    }

    #[test]
    fn decodes_falling_frame_sync_edge() {
        let changes = "#0\n0a\n0b\n1c\n#1\n1a\n#2\n0a\n1b\n0c\n#3\n1a\n";
        let params = ClockedSerialParams {
            bits: 2,
            channels: 1,
            frame_edge: Edge::Falling,
            signed: false,
            ..Default::default()
        };

        let decoded = decode(&raw_vcd(changes), params);

        assert_eq!(numeric(&decoded.rows[0].items[0].value), 0b10);
        assert_eq!(decoded.rows[0].items[0].start, BigUint::from(0u32));
        assert_eq!(decoded.rows[0].items[0].end, BigUint::from(3u32));
    }

    #[test]
    fn ignores_inactive_frame_sync_edge_timing() {
        let params = ClockedSerialParams {
            bits: 2,
            channels: 1,
            signed: false,
            ..Default::default()
        };

        let early_inactive = raw_vcd("#0\n0a\n1b\n1c\n#1\n1a\n#2\n0a\n0b\n0c\n#3\n1a\n");
        let late_inactive = raw_vcd("#0\n0a\n1b\n1c\n#1\n1a\n#2\n0a\n0c\n#3\n1a\n0b\n");

        let early = decode(&early_inactive, params.clone());
        let late = decode(&late_inactive, params);

        assert_eq!(early, late);
        assert_eq!(numeric(&early.rows[0].items[0].value), 0b10);
    }

    #[test]
    fn decodes_multiple_frames() {
        let frame0 = [1, 0, 1, 0, 0, 1, 1, 0];
        let frame1 = [0, 0, 0, 1, 1, 1, 1, 1];
        let params = ClockedSerialParams {
            bits: 4,
            channels: 2,
            signed: false,
            ..Default::default()
        };

        let decoded = decode(&pulse_tdm_frames(&[&frame0, &frame1], 0), params);

        assert_eq!(decoded.rows[0].items.len(), 2);
        assert_eq!(numeric(&decoded.rows[0].items[0].value), 0b1010);
        assert_eq!(numeric(&decoded.rows[0].items[1].value), 0b0001);
        assert_eq!(numeric(&decoded.rows[1].items[0].value), 0b0110);
        assert_eq!(numeric(&decoded.rows[1].items[1].value), 0b1111);
    }

    #[test]
    fn decodes_signed_sample_with_offset() {
        let bits = [1, 1, 1, 1];
        let params = ClockedSerialParams {
            bits: 4,
            channels: 1,
            offset: 1,
            signed: true,
            ..Default::default()
        };

        let decoded = decode(&pulse_tdm_vcd(&bits, 1), params);

        assert_eq!(decoded.rows[0].items.len(), 1);
        assert_eq!(numeric(&decoded.rows[0].items[0].value), -1);
        assert_eq!(decoded.rows[0].items[0].start, BigUint::from(0u32));
        assert_eq!(decoded.rows[0].items[0].end, BigUint::from(9u32));
    }

    #[test]
    fn decodes_lsb_first() {
        let bits = [0, 1, 0, 1];
        let params = ClockedSerialParams {
            bits: 4,
            channels: 1,
            bit_order: BitOrder::LsbFirst,
            signed: false,
            ..Default::default()
        };

        let decoded = decode(&pulse_tdm_vcd(&bits, 0), params);

        assert_eq!(numeric(&decoded.rows[0].items[0].value), 0b1010);
    }

    #[test]
    fn decodes_level_mode_i2s() {
        let params = ClockedSerialParams {
            frame_mode: FrameSyncMode::Level,
            frame_active_high: false,
            bits: 4,
            channels: 2,
            offset: 1,
            signed: false,
            ..Default::default()
        };

        let decoded = decode(&level_tdm_vcd([&[1, 0, 1, 0], &[0, 1, 1, 0]], 1), params);

        assert_eq!(decoded.rows.len(), 3);
        assert_eq!(numeric(&decoded.rows[0].items[0].value), 0b1010);
        assert_eq!(numeric(&decoded.rows[1].items[0].value), 0b0110);
        assert!(decoded.rows[2].items.is_empty());
    }

    #[test]
    fn flags_frames_too_short_for_configured_channels() {
        // The waveform carries four slots per frame; configuring sixteen must
        // not silently drop the rest. Each frame that ends before all
        // channels are read is reported.
        let spec = FormatSpec {
            mode: FrameSyncMode::Pulse,
            justification: Justification::Left,
            clock_edge: Edge::Rising,
            frame_edge: Edge::Rising,
            active_high: true,
            bits: 4,
            channels: 4,
            slot_width: 8,
            offset: 0,
        };
        let samples: Vec<Vec<i64>> = vec![vec![0; 4]; 3];
        let vcd = format_vcd(&spec, &samples);

        let params = ClockedSerialParams {
            bits: 4,
            channels: 16,
            slot_width: Some(8),
            ..Default::default()
        };
        let decoded = decode(&vcd, params);

        assert_eq!(decoded.rows.len(), 17);
        assert_eq!(decoded.rows[16].name, "errors");
        // The final frame has no following sync edge to detect it, so three
        // frames yield two errors.
        assert_eq!(decoded.rows[16].items.len(), 2);
        for item in &decoded.rows[16].items {
            assert_eq!(
                item.value,
                DecodedValue::text("incomplete frame: 4 of 16 channels")
            );
        }
    }

    #[test]
    fn flags_samples_that_do_not_fit_the_frame() {
        // Four clock edges per frame, but eight bits requested per sample.
        let vcd = pulse_tdm_frames(&[&[1, 0, 1, 0], &[1, 0, 1, 0], &[1, 0, 1, 0]], 0);
        let params = ClockedSerialParams {
            bits: 8,
            channels: 1,
            slot_width: Some(8),
            ..Default::default()
        };
        let decoded = decode(&vcd, params);

        assert_eq!(decoded.rows.len(), 2);
        assert_eq!(decoded.rows[1].name, "errors");
        assert_eq!(decoded.rows[1].items.len(), 2);
        assert_eq!(
            decoded.rows[1].items[0].value,
            DecodedValue::text("incomplete frame: 0 of 1 channels")
        );
    }

    #[test]
    fn flags_incomplete_level_mode_slots() {
        // Each word-select slot carries four clock edges, but eight bits are
        // requested.
        let vcd = level_tdm_vcd([&[1, 0, 1, 0], &[0, 1, 1, 0]], 0);
        let params = ClockedSerialParams {
            frame_mode: FrameSyncMode::Level,
            frame_active_high: false,
            bits: 8,
            channels: 2,
            ..Default::default()
        };
        let decoded = decode(&vcd, params);

        assert_eq!(decoded.rows.len(), 3);
        assert_eq!(decoded.rows[2].name, "errors");
        assert_eq!(decoded.rows[2].items.len(), 1);
        assert_eq!(
            decoded.rows[2].items[0].value,
            DecodedValue::text("incomplete sample")
        );
    }

    #[test]
    fn caps_incomplete_frame_errors() {
        // Every frame is too short for the requested sample, so each frame
        // sync produces an error. Only MAX_FRAME_ERRORS are kept, followed by
        // a single truncation marker.
        const FRAME: [u8; 4] = [1, 0, 1, 0];
        let frames: Vec<&[u8]> = (0..150).map(|_| &FRAME[..]).collect();
        let vcd = pulse_tdm_frames(&frames, 0);
        let params = ClockedSerialParams {
            bits: 8,
            channels: 1,
            slot_width: Some(8),
            ..Default::default()
        };
        let decoded = decode(&vcd, params);

        let errors = &decoded.rows[1].items;
        assert_eq!(errors.len(), MAX_FRAME_ERRORS + 1);
        assert_eq!(
            errors[MAX_FRAME_ERRORS].value,
            DecodedValue::text("further frame errors suppressed")
        );
        assert_eq!(
            errors[0].value,
            DecodedValue::text("incomplete frame: 0 of 1 channels")
        );
    }

    #[test]
    fn hides_error_row_when_disabled() {
        let vcd = pulse_tdm_frames(&[&[1, 0, 1, 0], &[1, 0, 1, 0]], 0);
        let params = ClockedSerialParams {
            bits: 8,
            channels: 1,
            slot_width: Some(8),
            show_errors: false,
            ..Default::default()
        };
        let decoded = decode(&vcd, params);

        assert_eq!(decoded.rows.len(), 1);
        assert_eq!(decoded.rows[0].name, "ch0");
    }

    #[test]
    fn marks_undefined_and_high_impedance_samples() {
        let params = ClockedSerialParams {
            bits: 2,
            channels: 1,
            ..Default::default()
        };

        let undefined = raw_vcd("#0\n0a\n1b\n1c\n#1\n1a\n#2\n0a\n0b\nxc\n#3\n1a\n");
        let decoded = decode(&undefined, params.clone());
        assert_eq!(decoded.rows[0].items[0].value, DecodedValue::Undef);

        let high_impedance = raw_vcd("#0\n0a\n1b\n1c\n#1\n1a\n#2\n0a\n0b\nzc\n#3\n1a\n");
        let decoded = decode(&high_impedance, params);
        assert_eq!(decoded.rows[0].items[0].value, DecodedValue::HighImp);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn decodes_fst_same_as_vcd() {
        let frame0 = [1, 0, 1, 0, 0, 1, 1, 0];
        let frame1 = [0, 0, 0, 1, 1, 1, 1, 1];
        let params = ClockedSerialParams {
            bits: 4,
            channels: 2,
            signed: false,
            ..Default::default()
        };
        let vcd = pulse_tdm_frames(&[&frame0, &frame1], 0);
        let vcd_decoded = decode(&vcd, params.clone());

        let container = load_container(&vcd);
        let refs =
            ["tb.bitclk", "tb.frame_sync", "tb.data"].map(VariableRef::from_hierarchy_string);
        let export = container
            .prepare_fst_export(&refs)
            .expect("failed to prepare FST export");
        let dir = std::env::temp_dir().join("surfer_tdm_decoder_fst_test");
        std::fs::create_dir_all(&dir).expect("failed to create temp dir");
        let path = camino::Utf8PathBuf::from_path_buf(dir.join("tdm_audio.fst"))
            .expect("temp path is not valid UTF-8");
        export.write_to_file(&path).expect("failed to write FST");

        let fst = std::fs::read(&path).expect("failed to read FST");
        let fst_decoded = decode_container(&load_container_bytes(fst), params);

        std::fs::remove_file(&path).ok();

        assert_eq!(vcd_decoded, fst_decoded);
    }

    #[test]
    fn rejects_invalid_settings() {
        let container = load_container(&pulse_tdm_vcd(&[0, 0], 0));
        let inputs = ["bitclk", "frame_sync", "data"]
            .iter()
            .map(|role| DecoderInput {
                role: (*role).to_string(),
                variable_ref: VariableRef::from_hierarchy_string(&format!("tb.{role}")),
            })
            .collect::<Vec<_>>();
        let resolved = resolve_inputs(&container, &inputs).expect("resolve inputs");

        let invalid = [
            // Level frame sync supports exactly two channels.
            ClockedSerialParams {
                frame_mode: FrameSyncMode::Level,
                channels: 4,
                ..Default::default()
            },
            // A user schema must not be able to request an unbounded channel
            // count, which would allocate one row per channel.
            ClockedSerialParams {
                channels: u32::MAX,
                ..Default::default()
            },
        ];

        for params in invalid {
            let result = ClockedSerialEngine.decode_params(
                &params,
                &DecoderContext {
                    inputs: &resolved,
                    settings: &crate::decoders::DecoderSettings::default(),
                },
            );

            assert!(result.is_err(), "{params:?} should be rejected");
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct FormatSpec {
        mode: FrameSyncMode,
        justification: Justification,
        clock_edge: Edge,
        frame_edge: Edge,
        active_high: bool,
        bits: u32,
        channels: u32,
        slot_width: u32,
        offset: u32,
    }

    fn sample_bit(sample: i64, bits: u32, index: u32) -> u8 {
        let mask = if bits == 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        let raw = (sample as u64) & mask;
        ((raw >> (bits - 1 - index)) & 1) as u8
    }

    fn sample_value(bits: u32, frame: usize, channel: usize) -> i64 {
        let magnitude = 1i64 << (bits - 1);
        let raw = ((frame * 7 + channel * 13 + 1) as i64 * 104_729) % magnitude;
        if (frame + channel).is_multiple_of(2) {
            raw
        } else {
            -raw
        }
    }

    fn pulse_edge_bit(spec: &FormatSpec, frame: &[i64], edge_index: u32) -> u8 {
        if edge_index < spec.offset {
            return 0;
        }
        let x = edge_index - spec.offset;
        let slot = x / spec.slot_width;
        let position = x % spec.slot_width;
        if slot >= spec.channels {
            return 0;
        }
        match spec.justification {
            Justification::Left => {
                if position < spec.bits {
                    sample_bit(frame[slot as usize], spec.bits, position)
                } else {
                    0
                }
            }
            Justification::Right => {
                if position >= spec.slot_width - spec.bits {
                    sample_bit(
                        frame[slot as usize],
                        spec.bits,
                        position - (spec.slot_width - spec.bits),
                    )
                } else {
                    0
                }
            }
        }
    }

    fn slot_edge_bit(spec: &FormatSpec, sample: i64, edge_index: u32) -> u8 {
        if edge_index < spec.offset {
            return 0;
        }
        let position = edge_index - spec.offset;
        match spec.justification {
            Justification::Left => {
                if position < spec.bits {
                    sample_bit(sample, spec.bits, position)
                } else {
                    0
                }
            }
            Justification::Right => {
                if position >= spec.slot_width - spec.bits {
                    sample_bit(sample, spec.bits, position - (spec.slot_width - spec.bits))
                } else {
                    0
                }
            }
        }
    }

    fn format_vcd(spec: &FormatSpec, samples: &[Vec<i64>]) -> String {
        let mut events: Vec<(u64, u8, u8, u8)> = Vec::new();
        let start_parity = match spec.clock_edge {
            Edge::Rising => 0,
            Edge::Falling => 1,
        };
        let fs_active = u8::from(spec.frame_edge == Edge::Rising);

        match spec.mode {
            FrameSyncMode::Pulse => {
                let frame_width = spec.offset + spec.channels * spec.slot_width;
                let frame_duration = 2 * u64::from(frame_width) + 2;
                for (frame_index, frame) in samples.iter().enumerate() {
                    let frame_start = start_parity + frame_index as u64 * frame_duration;
                    let mut data = 0u8;
                    for offset in 0..frame_duration {
                        let time = frame_start + offset;
                        let clk = (time % 2) as u8;
                        let fs = if offset < 2 { fs_active } else { 1 - fs_active };
                        if offset % 2 == 1 {
                            let edge_index = (offset - 1) / 2;
                            if edge_index < u64::from(frame_width) {
                                data = pulse_edge_bit(spec, frame, edge_index as u32);
                            }
                        }
                        events.push((time, clk, fs, data));
                    }
                }
            }
            FrameSyncMode::Level => {
                // The offset shifts the slot window, so a right-justified
                // sample extends past the nominal slot width.
                let slot_edges = match spec.justification {
                    Justification::Left => spec.slot_width,
                    Justification::Right => spec.offset + spec.slot_width,
                };
                let slot_duration = 2 * u64::from(slot_edges) + 2;
                let frame_duration = u64::from(spec.channels) * slot_duration;
                for (frame_index, frame) in samples.iter().enumerate() {
                    let frame_start = start_parity + frame_index as u64 * frame_duration;
                    for channel in 0..spec.channels {
                        let slot_start = frame_start + u64::from(channel) * slot_duration;
                        let fs = if (channel == 0) == spec.active_high {
                            1
                        } else {
                            0
                        };
                        let mut data = 0u8;
                        for offset in 0..slot_duration {
                            let time = slot_start + offset;
                            let clk = (time % 2) as u8;
                            if offset % 2 == 1 {
                                let edge_index = (offset - 1) / 2;
                                if edge_index < u64::from(slot_edges) {
                                    data = slot_edge_bit(
                                        spec,
                                        frame[channel as usize],
                                        edge_index as u32,
                                    );
                                }
                            }
                            events.push((time, clk, fs, data));
                        }
                    }
                }
            }
        }

        binary_vcd(&events)
    }

    fn check_format(spec: FormatSpec) {
        let frame_count = 3usize;
        let samples: Vec<Vec<i64>> = (0..frame_count)
            .map(|frame| {
                (0..spec.channels)
                    .map(|channel| sample_value(spec.bits, frame, channel as usize))
                    .collect()
            })
            .collect();

        let params = ClockedSerialParams {
            clock_edge: spec.clock_edge,
            frame_mode: spec.mode,
            frame_edge: spec.frame_edge,
            frame_active_high: spec.active_high,
            bits: spec.bits,
            channels: spec.channels,
            bit_order: BitOrder::MsbFirst,
            justification: spec.justification,
            offset: spec.offset,
            signed: true,
            slot_width: Some(spec.slot_width),
            ..Default::default()
        };

        let decoded = decode(&format_vcd(&spec, &samples), params);

        assert_eq!(decoded.rows.len(), spec.channels as usize + 1);
        assert!(
            decoded.rows[spec.channels as usize].items.is_empty(),
            "unexpected frame errors for {spec:?}"
        );
        for (channel, row) in decoded.rows.iter().take(spec.channels as usize).enumerate() {
            assert_eq!(
                row.items.len(),
                frame_count,
                "wrong sample count for {spec:?}"
            );
            for (frame, item) in row.items.iter().enumerate() {
                assert_eq!(
                    numeric(&item.value),
                    samples[frame][channel],
                    "wrong value for {spec:?} frame {frame} channel {channel}"
                );
            }
        }
    }

    #[test]
    fn decodes_tdm_format_matrix() {
        for &bits in &[16u32, 20, 24, 32] {
            for &channels in &[2u32, 4, 8, 16] {
                for &justification in &[Justification::Left, Justification::Right] {
                    for &slot_width in &[bits, 32] {
                        for &offset in &[0u32, 1] {
                            for &clock_edge in &[Edge::Rising, Edge::Falling] {
                                for &frame_edge in &[Edge::Rising, Edge::Falling] {
                                    check_format(FormatSpec {
                                        mode: FrameSyncMode::Pulse,
                                        justification,
                                        clock_edge,
                                        frame_edge,
                                        active_high: true,
                                        bits,
                                        channels,
                                        slot_width,
                                        offset,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn decodes_i2s_format_matrix() {
        for &bits in &[16u32, 20, 24, 32] {
            for &justification in &[Justification::Left, Justification::Right] {
                for &offset in &[0u32, 1] {
                    for &clock_edge in &[Edge::Rising, Edge::Falling] {
                        for &active_high in &[false, true] {
                            let slot_width = if bits == 32 { 64 } else { 32 };
                            check_format(FormatSpec {
                                mode: FrameSyncMode::Level,
                                justification,
                                clock_edge,
                                frame_edge: Edge::Rising,
                                active_high,
                                bits,
                                channels: 2,
                                slot_width,
                                offset,
                            });
                        }
                    }
                }
            }
        }
    }

    fn expected_sine(bits: u32, frame: usize, channel: usize, channels: u32, frames: usize) -> i64 {
        let amplitude = ((1i64 << (bits - 1)) - 1) as f64;
        (amplitude
            * (2.0
                * std::f64::consts::PI
                * (frame as f64 / frames as f64 + channel as f64 / channels as f64))
                .sin()) as i64
    }

    fn check_example(file: &str, params: ClockedSerialParams, channels: usize, frames: usize) {
        let path = project_root::get_project_root()
            .expect("failed to find project root")
            .join("examples")
            .join(file);
        let bytes = std::fs::read(&path).expect("failed to read example");
        let decoded = decode_container(&load_container_bytes(bytes), params.clone());

        assert_eq!(decoded.rows.len(), channels + 1, "{file}");
        assert!(
            decoded.rows[channels].items.is_empty(),
            "unexpected frame errors in {file}"
        );
        for (channel, row) in decoded.rows.iter().take(channels).enumerate() {
            assert_eq!(row.items.len(), frames, "{file}");
            for (frame, item) in row.items.iter().enumerate() {
                let expected = expected_sine(params.bits, frame, channel, channels as u32, frames);
                assert_eq!(
                    numeric(&item.value),
                    expected,
                    "{file} frame {frame} channel {channel}"
                );
            }
        }
    }

    #[test]
    fn decodes_left_justified_example() {
        check_example(
            "tdm_audio_left_8ch_24bit.vcd",
            ClockedSerialParams {
                bits: 24,
                channels: 8,
                slot_width: Some(32),
                ..Default::default()
            },
            8,
            3,
        );
    }

    #[test]
    fn decodes_right_justified_example() {
        check_example(
            "tdm_audio_right_8ch_24bit.vcd",
            ClockedSerialParams {
                bits: 24,
                channels: 8,
                slot_width: Some(32),
                justification: Justification::Right,
                ..Default::default()
            },
            8,
            3,
        );
    }

    #[test]
    fn decodes_i2s_example() {
        check_example(
            "i2s_audio_2ch_24bit.vcd",
            ClockedSerialParams {
                frame_mode: FrameSyncMode::Level,
                frame_active_high: false,
                bits: 24,
                channels: 2,
                slot_width: Some(32),
                offset: 1,
                ..Default::default()
            },
            2,
            3,
        );
    }

    #[test]
    fn decodes_falling_edge_examples() {
        let base = ClockedSerialParams {
            bits: 24,
            channels: 8,
            slot_width: Some(32),
            ..Default::default()
        };
        check_example(
            "tdm_audio_left_8ch_24bit_falling_bitclk.vcd",
            ClockedSerialParams {
                clock_edge: Edge::Falling,
                ..base.clone()
            },
            8,
            3,
        );
        check_example(
            "tdm_audio_left_8ch_24bit_falling_fs.vcd",
            ClockedSerialParams {
                frame_edge: Edge::Falling,
                ..base.clone()
            },
            8,
            3,
        );
        check_example(
            "tdm_audio_left_8ch_24bit_falling_both.vcd",
            ClockedSerialParams {
                clock_edge: Edge::Falling,
                frame_edge: Edge::Falling,
                ..base.clone()
            },
            8,
            3,
        );
        check_example(
            "tdm_audio_right_8ch_24bit_falling_both.vcd",
            ClockedSerialParams {
                clock_edge: Edge::Falling,
                frame_edge: Edge::Falling,
                justification: Justification::Right,
                ..base
            },
            8,
            3,
        );
    }
}
