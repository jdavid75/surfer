//! Biphase-mark audio engine (AES3 / S/PDIF).
//!
//! Decodes the self-clocked biphase-mark-coded line signal into audio samples.
//! The bit period is recovered from the spacing between line transitions; the
//! preambles provide frame and block synchronisation.

use eyre::{Result, bail};
use num::BigUint;
use serde::{Deserialize, Serialize};
use surfer_translation_types::{NumericRange, VariableValue};

use crate::decoders::engines::{DecoderEngine, EngineParams};
use crate::decoders::{
    DecodedData, DecodedItem, DecodedRow, DecodedValue, DecoderContext, DecoderInput,
    DecoderInputSignal, SettingValue,
};

/// AES3 subframe preamble.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preamble {
    /// Channel A (subframe 1), not the first frame of a block.
    X,
    /// Channel B (subframe 2).
    Y,
    /// Channel A, first frame of a block.
    Z,
}

/// The three preamble patterns as 8 line states (EBU Tech 3250 §2.4). The
/// second variant is the bitwise complement, selected when the preceding line
/// state is 1; matching both makes decoding polarity-independent.
const PREAMBLES: [(Preamble, [u8; 8]); 3] = [
    (Preamble::X, [1, 1, 1, 0, 0, 0, 1, 0]),
    (Preamble::Y, [1, 1, 1, 0, 0, 1, 0, 0]),
    (Preamble::Z, [1, 1, 1, 0, 1, 0, 0, 0]),
];

/// Audio word length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordBits {
    Auto,
    Bits(u32),
}

impl WordBits {
    fn resolve(self, blocks: &[Block]) -> u32 {
        match self {
            Self::Bits(bits) => bits,
            Self::Auto => blocks
                .iter()
                .find_map(|block| block.status_a.word_bits.or(block.status_b.word_bits))
                .unwrap_or(24),
        }
    }
}

/// Parameters for the biphase-mark audio engine.
#[derive(Debug, Clone, PartialEq)]
pub struct BiphaseAudioParams {
    /// Input role carrying the biphase-mark-coded line.
    pub data: String,
    /// Optional input role carrying a bit clock; both edges are sampled.
    pub bitclk: String,
    pub word_bits: WordBits,
    pub signed: bool,
    pub show_validity: bool,
    pub show_user: bool,
    pub show_status: bool,
    pub show_parity_errors: bool,
    pub show_preambles: bool,
}

impl BiphaseAudioParams {
    /// Resolve the typed parameters from generic engine parameters.
    pub fn from_params(params: &EngineParams) -> Result<Self> {
        let string = |key: &str| -> Option<String> {
            params
                .get(key)
                .and_then(SettingValue::as_enum)
                .map(str::to_string)
        };
        let boolean = |key: &str, default: bool| -> bool {
            params
                .get(key)
                .and_then(SettingValue::as_bool)
                .unwrap_or(default)
        };

        let data = string("data").ok_or_else(|| eyre::eyre!("Missing engine parameter 'data'"))?;
        let bitclk = string("bitclk").unwrap_or_else(|| "bitclk".into());
        let word_bits = match string("word_bits")
            .unwrap_or_else(|| "auto".into())
            .as_str()
        {
            "auto" => WordBits::Auto,
            "16" => WordBits::Bits(16),
            "20" => WordBits::Bits(20),
            "24" => WordBits::Bits(24),
            _ => bail!("Invalid 'word_bits' parameter"),
        };

        Ok(Self {
            data,
            bitclk,
            word_bits,
            signed: boolean("signed", true),
            show_validity: boolean("show_validity", false),
            show_user: boolean("show_user", false),
            show_status: boolean("show_status", true),
            show_parity_errors: boolean("show_parity_errors", true),
            show_preambles: boolean("show_preambles", true),
        })
    }

    fn row_count(&self) -> usize {
        2 + usize::from(self.show_validity)
            + usize::from(self.show_user)
            + usize::from(self.show_status)
            + usize::from(self.show_parity_errors)
            + usize::from(self.show_preambles)
    }
}

/// Engine for AES3 / S/PDIF biphase-mark audio.
pub struct BiphaseAudioEngine;

impl DecoderEngine for BiphaseAudioEngine {
    fn id(&self) -> &'static str {
        "biphase_audio"
    }

    fn validate(&self, params: &EngineParams) -> Result<()> {
        BiphaseAudioParams::from_params(params)?;
        Ok(())
    }

    fn row_count(&self, _inputs: &[DecoderInput], params: &EngineParams) -> usize {
        BiphaseAudioParams::from_params(params).map_or(0, |params| params.row_count())
    }

    fn decode(&self, params: &EngineParams, ctx: &DecoderContext<'_>) -> Result<DecodedData> {
        self.decode_params(&BiphaseAudioParams::from_params(params)?, ctx)
    }
}

impl BiphaseAudioEngine {
    /// Decode with already-resolved typed parameters.
    pub fn decode_params(
        &self,
        params: &BiphaseAudioParams,
        ctx: &DecoderContext<'_>,
    ) -> Result<DecodedData> {
        let data = ctx.input(&params.data)?;
        check_single_bit(data, &params.data)?;

        let transitions = collect_transitions(&data.accessor, true)?;
        let (half_cells, half_period) = if let Some(bitclk) = ctx.optional_input(&params.bitclk) {
            check_single_bit(bitclk, &params.bitclk)?;
            let clock = collect_transitions(&bitclk.accessor, false)?;
            (sample_half_cells(&transitions, &clock), None)
        } else {
            let (half_cells, half_period) = decode_half_cells(&transitions)?;
            (half_cells, Some(half_period))
        };
        let subframes = find_subframes(&half_cells, half_period);
        if subframes.is_empty() {
            bail!("no S/PDIF / AES3 preambles found");
        }
        Ok(build_rows(params, &subframes))
    }
}

/// A framing header: either one of the three standard preambles or an
/// eight-half-cell region that begins like a preamble but matches none of
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderKind {
    Standard(Preamble),
    Unknown,
}

impl HeaderKind {
    /// The label shown for this header, matching the historical annotation
    /// vocabulary retained by the independent-decoder regression check.
    fn label(self) -> &'static str {
        match self {
            Self::Standard(Preamble::X) => "Preamble M",
            Self::Standard(Preamble::Y) => "Preamble W",
            Self::Standard(Preamble::Z) => "Preamble B",
            Self::Unknown => "Unknown Preamble",
        }
    }

    /// Channel 0 is subframe A, channel 1 is subframe B. A recognized header
    /// determines its channel outright; an unrecognized one inherits the
    /// channel expected from an anchored framing sequence, if any.
    fn channel(self, expected: Option<u8>) -> Option<u8> {
        match (self, expected) {
            (Self::Standard(Preamble::X | Preamble::Z), _) => Some(0),
            (Self::Standard(Preamble::Y), _) => Some(1),
            (Self::Unknown, expected) => expected,
        }
    }
}

/// A decoded AES3 subframe.
struct Subframe {
    header: HeaderKind,
    /// Channel 0 is subframe A, channel 1 is subframe B. The channel is
    /// unknown for an unrecognized framing header that cannot be anchored to
    /// a neighboring recognized header.
    channel: Option<u8>,
    /// Slots 4-31, bit index 0 = slot 4.
    bits: [u8; 28],
    start: u64,
    end: u64,
}

/// Parsed channel-status summary for one channel.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChannelStatus {
    professional: bool,
    non_audio: bool,
    sample_rate: Option<u32>,
    /// Sample-rate scaling flag (professional byte 4 bit 7): divide by 1.001.
    scaled: bool,
    word_bits: Option<u32>,
    emphasis: Option<Emphasis>,
    /// `None` for consumer data, which has no CRC.
    crc_ok: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Emphasis {
    Fifty15,
    J17,
}

/// A complete 192-frame channel-status block.
struct Block {
    start: u64,
    end: u64,
    status_a: ChannelStatus,
    status_b: ChannelStatus,
}

impl ChannelStatus {
    fn summary(&self) -> String {
        let mut parts = vec![if self.professional {
            "AES3".to_string()
        } else {
            "consumer".to_string()
        }];
        if self.non_audio {
            parts.push("non-PCM".to_string());
        }
        if let Some(rate) = self.sample_rate {
            let mut text = format_sample_rate(rate);
            if self.scaled {
                text.push_str(" \u{00f7}1.001");
            }
            parts.push(text);
        }
        if let Some(bits) = self.word_bits {
            parts.push(format!("{bits}-bit"));
        }
        match self.emphasis {
            Some(Emphasis::Fifty15) => parts.push("50/15 \u{00b5}s emphasis".to_string()),
            Some(Emphasis::J17) => parts.push("J.17 emphasis".to_string()),
            None => {}
        }
        if self.crc_ok == Some(false) {
            parts.push("CRC error".to_string());
        }
        parts.join(" ")
    }
}

fn format_sample_rate(rate: u32) -> String {
    let whole = rate / 1000;
    let mut fraction = format!("{:03}", rate % 1000);
    while fraction.ends_with('0') {
        fraction.pop();
    }
    if fraction.is_empty() {
        format!("{whole} kHz")
    } else {
        format!("{whole}.{fraction} kHz")
    }
}

/// Channel status CRC (Tech 3250 Appendix 1): reflected polynomial
/// `x^8 + x^4 + x^3 + x^2 + 1`, initial all ones, LSB first.
fn channel_status_crc(bytes: &[u8; 24]) -> u8 {
    let mut crc = 0xFFu8;
    for &byte in &bytes[..23] {
        for bit in 0..8 {
            let feedback = (crc & 1) ^ ((byte >> bit) & 1);
            crc >>= 1;
            if feedback != 0 {
                crc ^= 0xB8;
            }
        }
    }
    crc
}

fn parse_channel_status(bytes: &[u8; 24]) -> ChannelStatus {
    let professional = bytes[0] & 0x01 != 0;
    let non_audio = bytes[0] & 0x02 != 0;
    if professional {
        let emphasis = match (bytes[0] >> 2) & 0x07 {
            3 => Some(Emphasis::Fifty15),
            7 => Some(Emphasis::J17),
            _ => None,
        };
        let scaled = bytes[4] & 0x80 != 0;
        let sample_rate = match (bytes[4] >> 3) & 0x0F {
            0b0001 => Some(24_000),
            0b0010 => Some(96_000),
            0b0011 => Some(192_000),
            0b1001 => Some(22_050),
            0b1010 => Some(88_200),
            0b1011 => Some(176_400),
            _ => match (bytes[0] >> 6) & 0x03 {
                0b01 => Some(44_100),
                0b10 => Some(48_000),
                0b11 => Some(32_000),
                _ => None,
            },
        };
        let max_24 = (bytes[2] & 0x07) == 0b001;
        let word_bits = match (bytes[2] >> 3) & 0x07 {
            0 => Some(if max_24 { 24 } else { 20 }),
            1 => Some(if max_24 { 23 } else { 19 }),
            2 => Some(if max_24 { 22 } else { 18 }),
            3 => Some(if max_24 { 21 } else { 17 }),
            4 => Some(if max_24 { 20 } else { 16 }),
            5 => Some(if max_24 { 24 } else { 20 }),
            _ => None,
        };
        ChannelStatus {
            professional,
            non_audio,
            sample_rate,
            scaled,
            word_bits,
            emphasis,
            crc_ok: Some(channel_status_crc(bytes) == bytes[23]),
        }
    } else {
        let emphasis = match (bytes[0] >> 3) & 0x07 {
            1 => Some(Emphasis::Fifty15),
            _ => None,
        };
        let value = bytes[3] & 0x0F;
        let sample_rate = if bytes[3] & 0x80 != 0 {
            match value {
                11 => Some(128_000),
                13 => Some(705_600),
                _ => None,
            }
        } else {
            match value {
                0 => Some(44_100),
                2 => Some(48_000),
                3 => Some(32_000),
                4 => Some(22_050),
                5 => Some(384_000),
                6 => Some(24_000),
                8 => Some(88_200),
                9 => Some(768_000),
                10 => Some(96_000),
                12 => Some(176_400),
                13 => Some(352_800),
                14 => Some(192_000),
                _ => None,
            }
        };
        let max_24 = bytes[4] & 0x01 != 0;
        let word_bits = match (bytes[4] >> 1) & 0x07 {
            0 => Some(if max_24 { 24 } else { 20 }),
            1 => Some(if max_24 { 20 } else { 16 }),
            2 => Some(if max_24 { 22 } else { 18 }),
            4 => Some(if max_24 { 23 } else { 19 }),
            5 => Some(if max_24 { 24 } else { 20 }),
            6 => Some(if max_24 { 21 } else { 17 }),
            _ => None,
        };
        ChannelStatus {
            professional,
            non_audio,
            sample_rate,
            scaled: false,
            word_bits,
            emphasis,
            crc_ok: None,
        }
    }
}

fn block_text(block: &Block) -> String {
    let a = block.status_a.summary();
    let b = block.status_b.summary();
    if a == b {
        a
    } else {
        format!("A: {a} / B: {b}")
    }
}

/// Accumulate the per-channel channel-status bits and return complete blocks
/// plus block-count errors. Bits are stored LSB-first within each byte, and
/// the first bit after a `Z` preamble is byte 0 bit 0.
fn collect_blocks(subframes: &[Subframe]) -> (Vec<Block>, Vec<DecodedItem>) {
    let mut blocks = Vec::new();
    let mut errors = Vec::new();
    let mut status_a = [0u8; 24];
    let mut status_b = [0u8; 24];
    let mut bits_a = 0usize;
    let mut bits_b = 0usize;
    let mut frame_count = 0usize;
    let mut block_start = 0u64;
    let mut in_block = false;
    let mut emitted = false;

    for subframe in subframes {
        let Some(channel) = subframe.channel else {
            continue;
        };

        if subframe.header == HeaderKind::Standard(Preamble::Z) {
            // A block is only complete when both channels contributed 192
            // status bits; a missing subframe on one channel counts too.
            if in_block && (frame_count != 192 || bits_a != 192 || bits_b != 192) {
                errors.push(DecodedItem {
                    start: BigUint::from(subframe.start),
                    end: BigUint::from(subframe.end),
                    value: DecodedValue::text("block count"),
                });
            }
            status_a = [0u8; 24];
            status_b = [0u8; 24];
            bits_a = 0;
            bits_b = 0;
            frame_count = 0;
            block_start = subframe.start;
            in_block = true;
            emitted = false;
        }

        if channel == 0 {
            frame_count += 1;
            if !emitted && bits_a < 192 {
                status_a[bits_a / 8] |= subframe.bits[26] << (bits_a % 8);
                bits_a += 1;
            }
        } else if !emitted && bits_b < 192 {
            status_b[bits_b / 8] |= subframe.bits[26] << (bits_b % 8);
            bits_b += 1;
        }

        if in_block && !emitted && bits_a == 192 && bits_b == 192 {
            blocks.push(Block {
                start: block_start,
                end: subframe.end,
                status_a: parse_channel_status(&status_a),
                status_b: parse_channel_status(&status_b),
            });
            emitted = true;
        }
    }

    // A capture can end mid-block, so the next `Z` that would report the count
    // never arrives. Report the incomplete final block here.
    if in_block && !emitted {
        let end = subframes
            .iter()
            .rev()
            .find_map(|subframe| subframe.channel.map(|_| subframe.end))
            .unwrap_or(block_start);
        errors.push(DecodedItem {
            start: BigUint::from(block_start),
            end: BigUint::from(end),
            value: DecodedValue::text("block count"),
        });
    }

    (blocks, errors)
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

/// Collect `(time, level)` transitions of a 1-bit signal.
///
/// With `skip_unknown`, `x`/`z` values are ignored rather than rejected: a
/// capture commonly idles undefined before the driver starts, and the gaps
/// they leave behind are handled as stream discontinuities. A bit clock has
/// no such excuse, so its unknowns are an error.
fn collect_transitions(
    accessor: &crate::wave_container::SignalAccessor,
    skip_unknown: bool,
) -> Result<Vec<(u64, u8)>> {
    let mut transitions = Vec::new();
    for (time, value) in accessor.iter_changes() {
        let level = match &value {
            VariableValue::BigUint(value) => {
                if *value == BigUint::from(0u8) {
                    Some(0)
                } else if *value == BigUint::from(1u8) {
                    Some(1)
                } else {
                    None
                }
            }
            VariableValue::String(value) => match value.chars().next() {
                Some('0') => Some(0),
                Some('1') => Some(1),
                _ => None,
            },
        };
        match level {
            Some(level) => transitions.push((time, level)),
            None if skip_unknown => {}
            None => bail!("biphase clock contains non-binary values"),
        }
    }

    // Simultaneous events carry no biphase interval. Some exporters emit a
    // provisional value and then the intended value at the same timestamp, so
    // retain the final state rather than treating the zero-length gap as a
    // decoding error.
    let mut collapsed = Vec::with_capacity(transitions.len());
    for transition in transitions {
        if collapsed
            .last()
            .is_some_and(|(timestamp, _)| *timestamp == transition.0)
        {
            collapsed.pop();
        }
        collapsed.push(transition);
    }
    Ok(collapsed)
}

/// Sample the data line at every edge of an explicit bit clock.
///
/// A bit clock has one cycle per bit cell, so its two edges land on the two
/// half-cells of each biphase-mark bit.
fn sample_half_cells(data: &[(u64, u8)], clock: &[(u64, u8)]) -> Vec<(u64, u8)> {
    let mut half_cells = Vec::new();
    let mut data_level = data.first().map_or(0, |(_, level)| *level);
    let mut index = 0usize;
    for &(time, _) in clock {
        while index < data.len() && data[index].0 <= time {
            data_level = data[index].1;
            index += 1;
        }
        half_cells.push((time, data_level));
    }
    half_cells
}

/// Gaps longer than this many half periods are not biphase gaps but stream
/// discontinuities, e.g. the idle line before a capture's first frame.
/// A four-half-period run is retained because it can be a malformed
/// preamble header rather than an idle gap.
const MAX_GAP_UNITS: f64 = 4.0;

/// Relative tolerance when checking whether a gap is an integer number of
/// half periods. Wide enough for transition jitter, narrow enough to exclude
/// the half-period glitches (0.5, 1.5, 2.5) from candidate scoring.
const SCORE_TOLERANCE: f64 = 0.25;

/// The gap in half periods if `candidate` explains it as an integer 1, 2, or 3
/// within [`SCORE_TOLERANCE`].
fn gap_units(gap: u64, candidate: u64) -> Option<u64> {
    let units = gap as f64 / candidate as f64;
    let rounded = units.round();
    ((1.0..=3.0).contains(&rounded) && (units - rounded).abs() <= SCORE_TOLERANCE)
        .then_some(rounded as u64)
}

/// Number of gaps that `candidate` explains as an integer number of half
/// periods.
fn gap_score(gaps: &[u64], candidate: u64) -> usize {
    gaps.iter()
        .filter(|&&gap| gap_units(gap, candidate).is_some())
        .count()
}

/// Estimate the half-bit period from the gaps between transitions.
///
/// The gaps of a biphase-mark stream are one, two, or three half periods, so
/// each gap divided by one, two, or three is a candidate. The true half
/// period is the candidate that explains the most gaps as integer multiples,
/// which stays robust when transition jitter or half-period glitches make the
/// individual gap values differ from exact multiples.
fn estimate_half_period(gaps: &[u64]) -> Option<u64> {
    // The half period is constant, so a sample is enough to score candidates
    // and keeps this linear for captures with many transitions.
    const MAX_SAMPLE: usize = 512;
    let step = gaps.len().div_ceil(MAX_SAMPLE).max(1);
    let sample: Vec<u64> = gaps.iter().step_by(step).copied().collect();

    let mut candidates: Vec<u64> = sample
        .iter()
        .flat_map(|&gap| (1..=3u64).map(move |divisor| gap / divisor))
        .filter(|candidate| *candidate > 0)
        .collect();
    candidates.sort_unstable();
    candidates.dedup();

    let best_sample_score = candidates
        .iter()
        .map(|&candidate| gap_score(&sample, candidate))
        .max()?;

    // Re-score the best candidates on the full gap list: a sampling stride can
    // skip every one-unit gap, leaving the true half period tied with a
    // multiple. Ties then prefer a candidate that is itself a gap (the half
    // period appears as one in a valid stream) and, failing that, the smaller
    // candidate.
    candidates
        .into_iter()
        .filter(|&candidate| gap_score(&sample, candidate) == best_sample_score)
        .max_by_key(|&candidate| {
            (
                gap_score(gaps, candidate),
                gaps.iter().any(|&gap| gap_units(gap, candidate) == Some(1)),
                std::cmp::Reverse(candidate),
            )
        })
}

/// Turn line transitions into half-cell levels.
///
/// Biphase-mark gaps are one, two, or (in preambles) three half-bit periods.
/// The number of half cells up to each transition is rounded rather than each
/// gap independently: a clocked transmitter can shift a run of edges by half
/// a cell, and rounding the cumulative count keeps that shift from adding an
/// extra half cell and losing bit alignment. Gaps longer than
/// [`MAX_GAP_UNITS`] are dropped so that an idle line before or between
/// frames does not abort the decode.
fn decode_half_cells(transitions: &[(u64, u8)]) -> Result<(Vec<(u64, u8)>, u64)> {
    if transitions.len() < 2 {
        bail!("not enough transitions to recover the bit clock");
    }

    let gaps: Vec<u64> = transitions
        .windows(2)
        .map(|pair| pair[1].0 - pair[0].0)
        .collect();
    debug_assert!(!gaps.contains(&0));

    let half_period = estimate_half_period(&gaps)
        .ok_or_else(|| eyre::eyre!("could not recover the bit clock"))?;

    let mut half_cells = Vec::new();
    let mut origin = transitions[0].0;
    let mut previous_units = 0u64;
    for pair in transitions.windows(2) {
        let gap = pair[1].0 - pair[0].0;
        if gap as f64 > MAX_GAP_UNITS * half_period as f64 {
            // Discontinuity (idle line): restart the accounting at the next
            // transition and emit nothing for the dropped transition.
            origin = pair[1].0;
            previous_units = 0;
            continue;
        }
        let cumulative_units = (pair[1].0 - origin).saturating_add(half_period / 2) / half_period;
        let units = cumulative_units.saturating_sub(previous_units);
        previous_units = cumulative_units;
        for step in 0..units {
            half_cells.push((
                pair[0].0.saturating_add(step.saturating_mul(half_period)),
                pair[0].1,
            ));
        }
    }
    Ok((half_cells, half_period))
}

/// Match an 8-half-cell window against a preamble or its complement.
fn match_preamble(half_cells: &[(u64, u8)]) -> Option<Preamble> {
    if half_cells.len() < 8 {
        return None;
    }
    for (preamble, pattern) in PREAMBLES {
        let direct = half_cells[..8]
            .iter()
            .zip(pattern)
            .all(|((_, level), expected)| *level == expected);
        let inverted = half_cells[..8]
            .iter()
            .zip(pattern)
            .all(|((_, level), expected)| *level == 1 - expected);
        if direct || inverted {
            return Some(preamble);
        }
    }
    None
}

/// Classify the eight-half-cell window starting at `index`.
///
/// Valid halves never contain a run of three equal levels outside a
/// preamble. Malformed headers retain at least that property, including the
/// historic four-half-period violations, while carrying none of the
/// recognized eight-state patterns. The caller is responsible for only using
/// an unrecognized classification at a position that belongs to a framing
/// sequence.
fn classify_header(half_cells: &[(u64, u8)], index: usize) -> Option<HeaderKind> {
    if index + 8 > half_cells.len() {
        return None;
    }
    if let Some(preamble) = match_preamble(&half_cells[index..]) {
        return Some(HeaderKind::Standard(preamble));
    }
    if index > 0 && half_cells[index - 1].1 == half_cells[index].1 {
        return None;
    }
    let opener = half_cells[index..index + 8]
        .iter()
        .take_while(|&&(_, level)| level == half_cells[index].1)
        .count();
    (opener >= 3).then_some(HeaderKind::Unknown)
}

/// Decode the 28 slots after the eight-half-cell framing header at `index`.
fn parse_subframe(
    half_cells: &[(u64, u8)],
    index: usize,
    half_period: Option<u64>,
    header: HeaderKind,
    channel: Option<u8>,
) -> Subframe {
    let mut bits = [0u8; 28];
    let (pairs, _) = half_cells[index + 8..index + 64].as_chunks::<2>();
    for (bit, pair) in bits.iter_mut().zip(pairs) {
        *bit = u8::from(pair[0].1 != pair[1].1);
    }

    let last = half_cells[index + 63].0;
    let end = match half_period {
        Some(half_period) => last.saturating_add(half_period),
        None => half_cells.get(index + 64).map_or(last, |next| next.0),
    };
    Subframe {
        header,
        channel,
        bits,
        start: half_cells[index].0,
        end,
    }
}

/// Scan the half-cell stream for preambles and decode the 28 data bits after
/// each one.
///
/// Recognized headers determine their own channel. An unrecognized header is
/// parsed exactly like a standard one, but only when it lies where framing
/// says a header belongs: immediately before or after a neighboring
/// recognized header, or as part of a repeated 64-half-cell cadence.
/// Unrecognized headers outside such a sequence are ignored so that a single
/// ambiguous timing artifact cannot become a diagnosis.
///
/// `half_period` is the recovered half-bit period when the bit clock was
/// recovered from the data, and `None` when an explicit bit clock was
/// sampled; it is used to give the last subframe an end time.
fn find_subframes(half_cells: &[(u64, u8)], half_period: Option<u64>) -> Vec<Subframe> {
    const SUBFRAME_HALF_CELLS: usize = 64;

    let mut subframes = Vec::new();
    let mut candidate_starts = Vec::new();
    let mut index = 0usize;
    let mut expected: Option<(usize, u8)> = None;

    while index + SUBFRAME_HALF_CELLS <= half_cells.len() {
        if let Some(preamble) = match_preamble(&half_cells[index..]) {
            let header = HeaderKind::Standard(preamble);
            let channel = header
                .channel(None)
                .expect("recognized preambles have channels");
            for event in
                flush_preceding_unknowns(half_cells, half_period, &candidate_starts, index, channel)
            {
                subframes.push(event);
            }
            candidate_starts.clear();

            subframes.push(parse_subframe(
                half_cells,
                index,
                half_period,
                header,
                Some(channel),
            ));
            expected = Some((index + SUBFRAME_HALF_CELLS, 1 - channel));
            index += SUBFRAME_HALF_CELLS;
            continue;
        }

        if let Some((expected_index, expected_channel)) = expected
            && index == expected_index
            && classify_header(half_cells, index) == Some(HeaderKind::Unknown)
        {
            subframes.push(parse_subframe(
                half_cells,
                index,
                half_period,
                HeaderKind::Unknown,
                HeaderKind::Unknown.channel(Some(expected_channel)),
            ));
            expected = Some((index + SUBFRAME_HALF_CELLS, 1 - expected_channel));
            index += SUBFRAME_HALF_CELLS;
            continue;
        }

        expected = None;
        if classify_header(half_cells, index) == Some(HeaderKind::Unknown) {
            candidate_starts.push(index);
        }
        index += 1;
    }

    subframes.extend(commit_unanchored_unknowns(
        half_cells,
        half_period,
        &candidate_starts,
    ));
    subframes
}

/// Assign channels to the unknown candidate headers that form an unbroken
/// 64-half-cell cadence immediately before a recognized header.
fn flush_preceding_unknowns(
    half_cells: &[(u64, u8)],
    half_period: Option<u64>,
    candidates: &[usize],
    anchor_index: usize,
    anchor_channel: u8,
) -> Vec<Subframe> {
    const SUBFRAME_HALF_CELLS: usize = 64;

    let mut events = Vec::new();
    let mut next_index = anchor_index;
    let mut next_channel = anchor_channel;
    for &index in candidates.iter().rev() {
        if next_index.checked_sub(SUBFRAME_HALF_CELLS) != Some(index) {
            continue;
        }
        next_channel ^= 1;
        events.push(parse_subframe(
            half_cells,
            index,
            half_period,
            HeaderKind::Unknown,
            HeaderKind::Unknown.channel(Some(next_channel)),
        ));
        next_index = index;
    }
    events.reverse();
    events
}

/// Label runs of unknown candidate headers when no recognized header locks
/// the stream, without assigning their channels. A single isolated candidate
/// is not enough evidence to begin decoding them.
fn commit_unanchored_unknowns(
    half_cells: &[(u64, u8)],
    half_period: Option<u64>,
    candidates: &[usize],
) -> Vec<Subframe> {
    const SUBFRAME_HALF_CELLS: usize = 64;

    let mut subframes = Vec::new();
    let mut run = Vec::new();
    for &index in candidates.iter().chain(std::iter::once(&usize::MAX)) {
        if run.last() == Some(&(index.wrapping_sub(SUBFRAME_HALF_CELLS))) {
            run.push(index);
        } else {
            if run.len() >= 2 {
                for start in run.drain(..) {
                    subframes.push(parse_subframe(
                        half_cells,
                        start,
                        half_period,
                        HeaderKind::Unknown,
                        HeaderKind::Unknown.channel(None),
                    ));
                }
            }
            run.clear();
            if index != usize::MAX {
                run.push(index);
            }
        }
    }
    subframes
}

/// Extract the audio word (MSB at slot 27, LSB at slot 4/8/12).
fn extract_audio(bits: &[u8; 28], word_bits: u32) -> i64 {
    // The MSB is always slot 27; the LSB moves down as the word gets shorter,
    // so the word starts `24 - word_bits` slots after slot 4.
    let word_bits = word_bits.clamp(16, 24);
    let first = (24 - word_bits) as usize;
    let mut value = 0i64;
    for offset in 0..word_bits {
        value |= i64::from(bits[first + offset as usize]) << offset;
    }
    value
}

fn sign_extend(value: i64, bits: u32) -> i64 {
    if bits >= 64 {
        return value;
    }
    let shift = 64 - bits;
    (value << shift) >> shift
}

fn build_rows(params: &BiphaseAudioParams, subframes: &[Subframe]) -> DecodedData {
    let (blocks, block_count_errors) = collect_blocks(subframes);
    let word_bits = params.word_bits.resolve(&blocks);
    let numeric_range = Some(if params.signed {
        let half = 2.0f64.powi((word_bits - 1) as i32);
        NumericRange {
            min: -half,
            max: half - 1.0,
        }
    } else {
        NumericRange {
            min: 0.0,
            max: 2.0f64.powi(word_bits as i32) - 1.0,
        }
    });

    let mut rows = vec![
        DecodedRow {
            name: "A".to_string(),
            numeric_range,
            items: Vec::new(),
        },
        DecodedRow {
            name: "B".to_string(),
            numeric_range,
            items: Vec::new(),
        },
    ];
    let validity_row = if params.show_validity {
        let index = rows.len();
        rows.push(DecodedRow {
            name: "validity".to_string(),
            numeric_range: None,
            items: Vec::new(),
        });
        Some(index)
    } else {
        None
    };
    let user_row = if params.show_user {
        let index = rows.len();
        rows.push(DecodedRow {
            name: "user".to_string(),
            numeric_range: None,
            items: Vec::new(),
        });
        Some(index)
    } else {
        None
    };
    let status_row = if params.show_status {
        let index = rows.len();
        rows.push(DecodedRow {
            name: "status".to_string(),
            numeric_range: None,
            items: Vec::new(),
        });
        Some(index)
    } else {
        None
    };
    let errors_row = if params.show_parity_errors {
        let index = rows.len();
        rows.push(DecodedRow {
            name: "errors".to_string(),
            numeric_range: None,
            items: Vec::new(),
        });
        Some(index)
    } else {
        None
    };
    let preamble_row = if params.show_preambles {
        let index = rows.len();
        rows.push(DecodedRow {
            name: "preamble".to_string(),
            numeric_range: None,
            items: Vec::new(),
        });
        Some(index)
    } else {
        None
    };

    for subframe in subframes {
        if let Some(row) = preamble_row {
            rows[row].items.push(DecodedItem {
                start: BigUint::from(subframe.start),
                end: BigUint::from(subframe.end),
                value: DecodedValue::text(subframe.header.label()),
            });
        }
        if subframe.header == HeaderKind::Unknown
            && let Some(row) = errors_row
        {
            rows[row].items.push(DecodedItem {
                start: BigUint::from(subframe.start),
                end: BigUint::from(subframe.end),
                value: DecodedValue::text(HeaderKind::Unknown.label()),
            });
        }

        if let Some(channel) = subframe.channel {
            let mut sample = extract_audio(&subframe.bits, word_bits);
            if params.signed {
                sample = sign_extend(sample, word_bits);
            }
            rows[channel as usize].items.push(DecodedItem {
                start: BigUint::from(subframe.start),
                end: BigUint::from(subframe.end),
                value: DecodedValue::integer(sample, word_bits),
            });
        }

        if let Some(row) = validity_row {
            rows[row].items.push(DecodedItem {
                start: BigUint::from(subframe.start),
                end: BigUint::from(subframe.end),
                value: DecodedValue::integer(i64::from(subframe.bits[24]), 1),
            });
        }
        if let Some(row) = user_row {
            rows[row].items.push(DecodedItem {
                start: BigUint::from(subframe.start),
                end: BigUint::from(subframe.end),
                value: DecodedValue::integer(i64::from(subframe.bits[25]), 1),
            });
        }

        // Even parity over slots 4-31.
        let ones: u32 = subframe.bits.iter().map(|bit| u32::from(*bit)).sum();
        if !ones.is_multiple_of(2)
            && let Some(row) = errors_row
        {
            rows[row].items.push(DecodedItem {
                start: BigUint::from(subframe.start),
                end: BigUint::from(subframe.end),
                value: DecodedValue::text("parity"),
            });
        }
    }

    if let Some(row) = status_row {
        for block in &blocks {
            rows[row].items.push(DecodedItem {
                start: BigUint::from(block.start),
                end: BigUint::from(block.end),
                value: DecodedValue::text(block_text(block)),
            });
        }
        // With `auto` and no channel status the word length is a guess, so
        // say so rather than silently decoding the samples as 24-bit.
        if matches!(params.word_bits, WordBits::Auto)
            && blocks.is_empty()
            && let (Some(first), Some(last)) = (subframes.first(), subframes.last())
        {
            rows[row].items.push(DecodedItem {
                start: BigUint::from(first.start),
                end: BigUint::from(last.end),
                value: DecodedValue::text("word length: 24-bit assumed (no channel status)"),
            });
        }
    }
    if let Some(row) = errors_row {
        rows[row].items.extend(block_count_errors);
        // Block-count errors are detected when the following block starts, so
        // they arrive after the per-subframe errors that precede them in time.
        // The drawing code binary-searches this row, so keep it sorted.
        rows[row].items.sort_by(|a, b| a.start.cmp(&b.start));
    }

    DecodedData { rows }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Arc;

    use surver::WELLEN_SURFER_DEFAULT_OPTIONS;

    use super::*;
    use crate::decoders::{DecodedValue, DecoderInput, DecoderSettings, resolve_inputs};
    use crate::wave_container::{VariableRef, VariableRefExt, WaveContainer};
    use crate::wellen::{BodyResult, LoadSignalPayload, LoadSignalsResult};

    fn raw_data_vcd(changes: &str) -> String {
        format!(
            "$timescale 1ns $end\n\
             $scope module tb $end\n\
             $var wire 1 c data $end\n\
             $upscope $end\n\
             $enddefinitions $end\n\
             {changes}"
        )
    }

    fn raw_data_and_clock_vcd(changes: &str) -> String {
        format!(
            "$timescale 1ns $end\n\
             $scope module tb $end\n\
             $var wire 1 c data $end\n\
             $var wire 1 k bitclk $end\n\
             $upscope $end\n\
             $enddefinitions $end\n\
             {changes}"
        )
    }

    /// Encode a half-cell level sequence as a VCD where each half-cell lasts
    /// `half_period` time units. A terminating transition is appended so that
    /// the final real half-cell's group is complete.
    fn bmc_vcd(half_cells: &[u8], half_period: u64) -> String {
        let mut changes = String::new();
        let mut last: Option<u8> = None;
        for (index, level) in half_cells.iter().enumerate() {
            if last != Some(*level) {
                changes.push_str(&format!("#{}\n{}c\n", index as u64 * half_period, level));
                last = Some(*level);
            }
        }
        changes.push_str(&format!(
            "#{}\n{}c\n",
            half_cells.len() as u64 * half_period,
            1 - last.unwrap_or(0)
        ));
        raw_data_vcd(&changes)
    }

    /// Like [`bmc_vcd`], but each change time is offset by a deterministic
    /// pseudo-random jitter of up to `jitter` time units.
    fn jittered_bmc_vcd(half_cells: &[u8], half_period: u64, jitter: i64) -> String {
        let mut changes = String::new();
        let mut last: Option<u8> = None;
        let mut last_time = 0i64;
        let mut change_index = 0i64;
        for (index, level) in half_cells.iter().enumerate() {
            if last != Some(*level) {
                let offset = (change_index * 7) % (2 * jitter + 1) - jitter;
                change_index += 1;
                last_time = index as i64 * half_period as i64 + offset;
                changes.push_str(&format!("#{}\n{}c\n", last_time, level));
                last = Some(*level);
            }
        }
        changes.push_str(&format!(
            "#{}\n{}c\n",
            last_time + half_period as i64,
            1 - last.unwrap_or(0)
        ));
        raw_data_vcd(&changes)
    }

    /// Like [`bmc_vcd`], but with the line held at 0 for `lead_in` time units
    /// before the first half-cell, as captures that idle before the stream
    /// starts do.
    fn bmc_vcd_with_lead_in(half_cells: &[u8], half_period: u64, lead_in: u64) -> String {
        let mut changes = String::from("#0\n0c\n");
        let mut last = Some(0u8);
        for (index, level) in half_cells.iter().enumerate() {
            if last != Some(*level) {
                changes.push_str(&format!(
                    "#{}\n{}c\n",
                    lead_in + index as u64 * half_period,
                    level
                ));
                last = Some(*level);
            }
        }
        changes.push_str(&format!(
            "#{}\n{}c\n",
            lead_in + half_cells.len() as u64 * half_period,
            1 - last.unwrap_or(0)
        ));
        raw_data_vcd(&changes)
    }

    /// Like [`bmc_vcd`], but the changes with indices `start..start + len`
    /// are shifted half a period earlier, as a clocked transmitter with a
    /// one-tick-early cell enable produces. This yields the 0.5 and 1.5
    /// half-period gaps that would otherwise defeat the gap classifier.
    fn glitched_bmc_vcd(half_cells: &[u8], half_period: u64, start: usize, len: usize) -> String {
        let mut changes = String::new();
        let mut last: Option<u8> = None;
        let mut change_index = 0usize;
        for (index, level) in half_cells.iter().enumerate() {
            if last != Some(*level) {
                let mut time = index as u64 * half_period;
                if (start..start + len).contains(&change_index) {
                    time -= half_period / 2;
                }
                change_index += 1;
                changes.push_str(&format!("#{time}\n{level}c\n"));
                last = Some(*level);
            }
        }
        changes.push_str(&format!(
            "#{}\n{}c\n",
            half_cells.len() as u64 * half_period,
            1 - last.unwrap_or(0)
        ));
        raw_data_vcd(&changes)
    }

    /// Encode a half-cell sequence with a bit clock that toggles on every
    /// half-cell boundary. Data changes are emitted before clock edges that
    /// share their timestamp, so each edge samples the new half-cell.
    fn bmc_bitclk_vcd(half_cells: &[u8], half_period: u64) -> String {
        let mut events: Vec<(u64, bool, char, u8)> = Vec::new();
        let mut last: Option<u8> = None;
        for (index, level) in half_cells.iter().enumerate() {
            if last != Some(*level) {
                events.push((index as u64 * half_period, false, 'c', *level));
                last = Some(*level);
            }
        }
        events.push((
            half_cells.len() as u64 * half_period,
            false,
            'c',
            1 - last.unwrap_or(0),
        ));
        for index in 0..=half_cells.len() {
            events.push((index as u64 * half_period, true, 'k', (index % 2) as u8));
        }
        events.sort_by_key(|(time, is_clock, _, _)| (*time, *is_clock));
        let mut changes = String::new();
        for (time, _, id, level) in events {
            changes.push_str(&format!("#{time}\n{level}{id}\n"));
        }
        raw_data_and_clock_vcd(&changes)
    }

    fn append_preamble(preamble: Preamble, level: &mut u8, half_cells: &mut Vec<u8>) {
        let (_, pattern) = PREAMBLES
            .iter()
            .find(|(candidate, _)| *candidate == preamble)
            .expect("known preamble");
        // The first preamble state must differ from the previous line state.
        let chosen = if *level == pattern[0] {
            pattern.map(|state| 1 - state)
        } else {
            *pattern
        };
        half_cells.extend_from_slice(&chosen);
        *level = *chosen.last().expect("pattern is non-empty");
    }

    /// Corrupt the encoded preamble at a subframe boundary into a
    /// four-half-cell opening run while retaining the eight-half-cell
    /// preamble length. This reproduces the historic non-standard signatures.
    fn malform_preamble(half_cells: &mut [u8], start: usize) {
        half_cells[start + 3] = 1 - half_cells[start + 3];
    }

    fn append_bits(bits: &[u8], level: &mut u8, half_cells: &mut Vec<u8>) {
        for bit in bits {
            *level = 1 - *level; // boundary transition
            half_cells.push(*level);
            if *bit == 1 {
                *level = 1 - *level; // mid-cell transition
            }
            half_cells.push(*level);
        }
    }

    /// Build slots 4-31 for a subframe.
    fn subframe_bits(
        sample: i64,
        word_bits: u32,
        validity: u8,
        user: u8,
        channel_status: u8,
    ) -> [u8; 28] {
        let mut bits = [0u8; 28];
        let word_bits = word_bits.clamp(16, 24);
        let first = (24 - word_bits) as usize;
        let raw = sample as u64;
        for offset in 0..word_bits {
            bits[first + offset as usize] = ((raw >> offset) & 1) as u8;
        }
        bits[24] = validity; // slot 28
        bits[25] = user; // slot 29
        bits[26] = channel_status; // slot 30
        let ones: u32 = bits[..27].iter().map(|bit| u32::from(*bit)).sum();
        bits[27] = (ones % 2) as u8; // even parity over slots 4-31
        bits
    }

    fn encode_subframe(
        preamble: Preamble,
        bits: &[u8; 28],
        level: &mut u8,
        half_cells: &mut Vec<u8>,
    ) {
        append_preamble(preamble, level, half_cells);
        append_bits(bits, level, half_cells);
    }

    fn status_bit(status: &[u8; 24], frame: usize) -> u8 {
        (status[frame / 8] >> (frame % 8)) & 1
    }

    /// Encode a complete block: channel A uses `Z`/`X` preambles, channel B
    /// uses `Y`; `sample(frame)` returns the (left, right) samples.
    fn encode_block(
        status_a: &[u8; 24],
        status_b: &[u8; 24],
        frames: usize,
        sample: impl Fn(usize) -> (i64, i64),
        word_bits: u32,
        level: &mut u8,
        half_cells: &mut Vec<u8>,
    ) {
        for frame in 0..frames {
            let (left, right) = sample(frame);
            let preamble = if frame == 0 { Preamble::Z } else { Preamble::X };
            encode_subframe(
                preamble,
                &subframe_bits(left, word_bits, 0, 0, status_bit(status_a, frame)),
                level,
                half_cells,
            );
            encode_subframe(
                Preamble::Y,
                &subframe_bits(right, word_bits, 0, 0, status_bit(status_b, frame)),
                level,
                half_cells,
            );
        }
    }

    /// Consumer channel status with the given byte-3 sample-rate code and
    /// byte-4 word-length code (bit 0 selects the 24-bit coding range).
    fn consumer_status(rate_code: u8, word_len_code: u8, max_24: bool) -> [u8; 24] {
        let mut status = [0u8; 24];
        status[3] = rate_code;
        status[4] = word_len_code << 1 | u8::from(max_24);
        status
    }

    fn load_container(vcd: &str) -> WaveContainer {
        load_container_with_signals(vcd.as_bytes().to_vec(), &["tb.data"])
    }

    fn load_container_with_signals(bytes: Vec<u8>, names: &[&str]) -> WaveContainer {
        let header =
            wellen::viewers::read_header(Cursor::new(bytes), &WELLEN_SURFER_DEFAULT_OPTIONS)
                .expect("failed to read wave header");
        let hierarchy = Arc::new(header.hierarchy);
        let mut container = WaveContainer::new_waveform(hierarchy.clone());

        let refs = names
            .iter()
            .map(|name| VariableRef::from_hierarchy_string(name));
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

    fn params(word_bits: WordBits) -> BiphaseAudioParams {
        BiphaseAudioParams {
            data: "data".to_string(),
            bitclk: "bitclk".to_string(),
            word_bits,
            signed: true,
            show_validity: false,
            show_user: false,
            show_status: true,
            show_parity_errors: true,
            show_preambles: true,
        }
    }

    fn decode(vcd: &str, params: BiphaseAudioParams) -> DecodedData {
        decode_container(&load_container(vcd), params)
    }

    /// Like [`decode`], but returns the decoder error instead of panicking, so
    /// robustness tests can accept malformed input.
    fn try_decode(vcd: &str, params: BiphaseAudioParams) -> eyre::Result<DecodedData> {
        let container = load_container(vcd);
        let inputs = vec![DecoderInput {
            role: "data".to_string(),
            variable_ref: VariableRef::from_hierarchy_string("tb.data"),
        }];
        let resolved = resolve_inputs(&container, &inputs).expect("resolve inputs");
        BiphaseAudioEngine.decode_params(
            &params,
            &DecoderContext {
                inputs: &resolved,
                settings: &DecoderSettings::default(),
            },
        )
    }

    fn decode_container(container: &WaveContainer, params: BiphaseAudioParams) -> DecodedData {
        let inputs = vec![DecoderInput {
            role: "data".to_string(),
            variable_ref: VariableRef::from_hierarchy_string("tb.data"),
        }];
        let resolved = resolve_inputs(container, &inputs).expect("resolve inputs");
        BiphaseAudioEngine
            .decode_params(
                &params,
                &DecoderContext {
                    inputs: &resolved,
                    settings: &DecoderSettings::default(),
                },
            )
            .expect("decoding failed")
    }

    fn decode_with_clock(container: &WaveContainer, params: BiphaseAudioParams) -> DecodedData {
        let inputs = vec![
            DecoderInput {
                role: "data".to_string(),
                variable_ref: VariableRef::from_hierarchy_string("tb.data"),
            },
            DecoderInput {
                role: "bitclk".to_string(),
                variable_ref: VariableRef::from_hierarchy_string("tb.bitclk"),
            },
        ];
        let resolved = resolve_inputs(container, &inputs).expect("resolve inputs");
        BiphaseAudioEngine
            .decode_params(
                &params,
                &DecoderContext {
                    inputs: &resolved,
                    settings: &DecoderSettings::default(),
                },
            )
            .expect("decoding failed")
    }

    fn numeric(value: &DecodedValue) -> i64 {
        match value {
            DecodedValue::Integer { value, .. } => *value,
            other => panic!("expected an integer value, got {other:?}"),
        }
    }

    #[test]
    fn decodes_stereo_frame() {
        let mut level = 0u8;
        let mut half = Vec::new();
        let a = subframe_bits(0x123456, 24, 0, 0, 0);
        let b = subframe_bits(-2, 24, 0, 1, 1);
        encode_subframe(Preamble::X, &a, &mut level, &mut half);
        encode_subframe(Preamble::Y, &b, &mut level, &mut half);

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Bits(24)));

        assert_eq!(decoded.rows.len(), 5); // A, B, status, errors, preamble
        assert_eq!(decoded.rows[0].name, "A");
        assert_eq!(numeric(&decoded.rows[0].items[0].value), 0x123456);
        assert_eq!(decoded.rows[1].name, "B");
        assert_eq!(numeric(&decoded.rows[1].items[0].value), -2);
        assert_eq!(decoded.rows[2].name, "status");
        assert!(decoded.rows[3].items.is_empty());
        assert_eq!(decoded.rows[4].name, "preamble");
        assert_eq!(
            decoded.rows[4].items[0].value,
            DecodedValue::text("Preamble M")
        );
        assert_eq!(
            decoded.rows[4].items[1].value,
            DecodedValue::text("Preamble W")
        );
    }

    #[test]
    fn flags_unknown_preamble_at_expected_position() {
        // The third subframe has a four-half-cell preamble opening, one of
        // the historic malformed signatures. Its position is already framed
        // by the neighboring recognized headers, so name the fault, assign
        // the subframe to the expected channel, and keep decoding.
        let mut level = 0u8;
        let mut half = Vec::new();
        let a1 = subframe_bits(0x111111, 24, 0, 0, 0);
        let b1 = subframe_bits(0x222222, 24, 0, 0, 0);
        let a2 = subframe_bits(0x333333, 24, 0, 0, 0);
        let b2 = subframe_bits(0x444444, 24, 0, 0, 0);
        encode_subframe(Preamble::Z, &a1, &mut level, &mut half);
        encode_subframe(Preamble::Y, &b1, &mut level, &mut half);
        encode_subframe(Preamble::X, &a2, &mut level, &mut half);
        encode_subframe(Preamble::Y, &b2, &mut level, &mut half);
        malform_preamble(&mut half, 2 * 64);

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Bits(24)));
        let labels: Vec<&str> = decoded.rows[4]
            .items
            .iter()
            .map(|item| match &item.value {
                DecodedValue::Text(label) => label.as_str(),
                other => panic!("expected preamble text, got {other:?}"),
            })
            .collect();

        assert_eq!(
            labels,
            ["Preamble B", "Preamble W", "Unknown Preamble", "Preamble W"]
        );
        // The capture also ends mid-block, so the errors row carries both the
        // unknown-preamble fault and a final block-count error, in time order.
        assert_eq!(decoded.rows[3].items.len(), 2);
        assert_eq!(
            decoded.rows[3].items[0].value,
            DecodedValue::text("block count")
        );
        assert_eq!(
            decoded.rows[3].items[1].value,
            DecodedValue::text("Unknown Preamble")
        );
        assert_eq!(
            decoded.rows[0]
                .items
                .iter()
                .map(|item| numeric(&item.value))
                .collect::<Vec<_>>(),
            [0x111111, 0x333333]
        );
        assert_eq!(
            decoded.rows[1]
                .items
                .iter()
                .map(|item| numeric(&item.value))
                .collect::<Vec<_>>(),
            [0x222222, 0x444444]
        );

        let mut hidden = params(WordBits::Bits(24));
        hidden.show_preambles = false;
        let decoded = decode(&bmc_vcd(&half, 4), hidden);
        assert_eq!(decoded.rows.len(), 4);
        assert!(
            decoded.rows[3]
                .items
                .iter()
                .any(|item| item.value == DecodedValue::text("Unknown Preamble"))
        );
    }

    #[test]
    fn assigns_unrecognized_preamble_before_first_anchor() {
        // The malformed header precedes every recognized preamble. Its
        // channel is resolved once the next header at the expected cadence
        // frames the unknown position, so its payload is not discarded.
        let mut level = 0u8;
        let mut half = Vec::new();
        let a0 = subframe_bits(0x111111, 24, 0, 0, 0);
        let b1 = subframe_bits(0x222222, 24, 0, 0, 0);
        let a2 = subframe_bits(0x333333, 24, 0, 0, 0);
        let b2 = subframe_bits(0x444444, 24, 0, 0, 0);
        encode_subframe(Preamble::X, &a0, &mut level, &mut half);
        encode_subframe(Preamble::Y, &b1, &mut level, &mut half);
        encode_subframe(Preamble::X, &a2, &mut level, &mut half);
        encode_subframe(Preamble::Y, &b2, &mut level, &mut half);
        malform_preamble(&mut half, 0);

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Bits(24)));
        let labels: Vec<&str> = decoded.rows[4]
            .items
            .iter()
            .map(|item| match &item.value {
                DecodedValue::Text(label) => label.as_str(),
                other => panic!("expected preamble text, got {other:?}"),
            })
            .collect();

        assert_eq!(
            labels,
            ["Unknown Preamble", "Preamble W", "Preamble M", "Preamble W"]
        );
        assert_eq!(
            decoded.rows[3].items[0].value,
            DecodedValue::text("Unknown Preamble")
        );
        assert_eq!(
            decoded.rows[0]
                .items
                .iter()
                .map(|item| numeric(&item.value))
                .collect::<Vec<_>>(),
            [0x111111, 0x333333]
        );
        assert_eq!(
            decoded.rows[1]
                .items
                .iter()
                .map(|item| numeric(&item.value))
                .collect::<Vec<_>>(),
            [0x222222, 0x444444]
        );
    }

    #[test]
    fn labels_unanchored_unknown_preamble_cadence() {
        // Without any recognized preamble, audio channels cannot be resolved.
        // A repeated 64-half-cell header cadence is nevertheless reported as
        // an explicit Unknown Preamble fault rather than being discarded.
        let mut level = 0u8;
        let mut half = Vec::new();
        let a = subframe_bits(0x111111, 24, 0, 0, 0);
        let b = subframe_bits(0x222222, 24, 0, 0, 0);
        encode_subframe(Preamble::X, &a, &mut level, &mut half);
        encode_subframe(Preamble::Y, &b, &mut level, &mut half);
        malform_preamble(&mut half, 0);
        malform_preamble(&mut half, 64);

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Bits(24)));
        let labels: Vec<&str> = decoded.rows[4]
            .items
            .iter()
            .map(|item| match &item.value {
                DecodedValue::Text(label) => label.as_str(),
                other => panic!("expected preamble text, got {other:?}"),
            })
            .collect();

        assert_eq!(labels, ["Unknown Preamble", "Unknown Preamble"]);
        assert!(decoded.rows[0].items.is_empty());
        assert!(decoded.rows[1].items.is_empty());
        assert_eq!(decoded.rows[3].items.len(), 2);
        assert_eq!(
            decoded.rows[3].items[0].value,
            DecodedValue::text("Unknown Preamble")
        );
        assert_eq!(
            decoded.rows[3].items[1].value,
            DecodedValue::text("Unknown Preamble")
        );
    }

    #[test]
    fn reports_word_length_assumption_without_channel_status() {
        // Without a complete channel-status block, `auto` assumes 24-bit;
        // that assumption is surfaced on the status row.
        let mut level = 0u8;
        let mut half = Vec::new();
        let a = subframe_bits(1, 24, 0, 0, 0);
        encode_subframe(Preamble::X, &a, &mut level, &mut half);

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Auto));

        assert_eq!(
            decoded.rows[2].items[0].value,
            DecodedValue::text("word length: 24-bit assumed (no channel status)")
        );
    }

    #[test]
    fn decodes_all_word_lengths() {
        // Values are two's complement, so a set MSB means a negative sample.
        for (word_bits, sample, expected) in [
            (16u32, -1234i64, -1234i64),
            (20, 0xABCDE, 0xABCDE - (1 << 20)),
            (24, 0x123456, 0x123456),
        ] {
            let mut level = 0u8;
            let mut half = Vec::new();
            let a = subframe_bits(sample, word_bits, 0, 0, 0);
            encode_subframe(Preamble::X, &a, &mut level, &mut half);

            let decoded = decode(&bmc_vcd(&half, 3), params(WordBits::Bits(word_bits)));

            assert_eq!(
                numeric(&decoded.rows[0].items[0].value),
                expected,
                "wrong value for {word_bits}-bit"
            );
        }
    }

    #[test]
    fn decodes_all_channel_status_word_lengths() {
        // Every word length the channel-status tables can declare, including
        // the 17-19 and 21-23 lengths that are not directly selectable.
        // (max_24, byte-4 word-length code, expected bits)
        for (max_24, code, word_bits) in [
            (true, 0u8, 24u32),
            (true, 1, 20),
            (true, 2, 22),
            (true, 4, 23),
            (true, 5, 24),
            (true, 6, 21),
            (false, 0, 20),
            (false, 1, 16),
            (false, 2, 18),
            (false, 4, 19),
            (false, 5, 20),
            (false, 6, 17),
        ] {
            let status = consumer_status(0x02, code, max_24);
            let positive = 0x2_3456 & ((1i64 << (word_bits - 1)) - 1);
            let negative = -positive;
            let mut level = 0u8;
            let mut half = Vec::new();
            encode_block(
                &status,
                &status,
                192,
                |_| (positive, negative),
                word_bits,
                &mut level,
                &mut half,
            );

            let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Auto));

            assert_eq!(
                numeric(&decoded.rows[0].items[0].value),
                positive,
                "wrong value for {word_bits}-bit (max_24={max_24}, code={code})"
            );
            assert_eq!(
                numeric(&decoded.rows[1].items[0].value),
                negative,
                "wrong negative value for {word_bits}-bit (max_24={max_24}, code={code})"
            );
        }
    }

    #[test]
    fn extracts_lsb_first_slots() {
        // Spec vector: for 24-bit words, slot 4 is the LSB and slot 27 the MSB.
        let mut level = 0u8;
        let mut half = Vec::new();
        let mut bits = [0u8; 28];
        bits[0] = 1; // slot 4 = LSB
        bits[23] = 1; // slot 27 = MSB
        let ones: u32 = bits[..27].iter().map(|bit| u32::from(*bit)).sum();
        bits[27] = (ones % 2) as u8;
        encode_subframe(Preamble::X, &bits, &mut level, &mut half);

        let mut params = params(WordBits::Bits(24));
        params.signed = false;
        let decoded = decode(&bmc_vcd(&half, 2), params);

        assert_eq!(numeric(&decoded.rows[0].items[0].value), (1i64 << 23) | 1);
    }

    #[test]
    fn matches_complementary_preambles() {
        let mut level = 0u8;
        let mut half = Vec::new();
        let a = subframe_bits(42, 24, 0, 0, 0);
        encode_subframe(Preamble::Z, &a, &mut level, &mut half);

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Bits(24)));
        let inverted: Vec<u8> = half.iter().map(|state| 1 - state).collect();
        let decoded_inverted = decode(&bmc_vcd(&inverted, 4), params(WordBits::Bits(24)));

        assert_eq!(decoded, decoded_inverted);
    }

    #[test]
    fn detects_parity_errors() {
        let mut level = 0u8;
        let mut half = Vec::new();
        let mut a = subframe_bits(7, 24, 0, 0, 0);
        a[27] ^= 1; // break even parity
        encode_subframe(Preamble::X, &a, &mut level, &mut half);

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Bits(24)));

        assert_eq!(decoded.rows[3].items.len(), 1);
        assert_eq!(decoded.rows[3].items[0].value, DecodedValue::text("parity"));
    }

    #[test]
    fn decodes_validity_and_user_rows() {
        let mut level = 0u8;
        let mut half = Vec::new();
        let a = subframe_bits(1, 24, 1, 1, 0);
        encode_subframe(Preamble::X, &a, &mut level, &mut half);

        let mut params = params(WordBits::Bits(24));
        params.show_validity = true;
        params.show_user = true;
        let decoded = decode(&bmc_vcd(&half, 4), params);

        assert_eq!(decoded.rows[2].name, "validity");
        assert_eq!(numeric(&decoded.rows[2].items[0].value), 1);
        assert_eq!(decoded.rows[3].name, "user");
        assert_eq!(numeric(&decoded.rows[3].items[0].value), 1);
    }

    #[test]
    fn locks_on_mid_frame_start() {
        let mut level = 0u8;
        let mut half = Vec::new();
        let b = subframe_bits(11, 24, 0, 0, 0);
        let a = subframe_bits(22, 24, 0, 0, 0);
        encode_subframe(Preamble::Y, &b, &mut level, &mut half);
        encode_subframe(Preamble::X, &a, &mut level, &mut half);

        let decoded = decode(&bmc_vcd(&half, 5), params(WordBits::Bits(24)));

        assert_eq!(numeric(&decoded.rows[0].items[0].value), 22);
        assert_eq!(numeric(&decoded.rows[1].items[0].value), 11);
    }

    #[test]
    fn decodes_many_frames() {
        let mut level = 0u8;
        let mut half = Vec::new();
        for frame in 0..8i64 {
            let a = subframe_bits(frame, 24, 0, 0, 0);
            let b = subframe_bits(-frame, 24, 0, 0, 0);
            let preamble = if frame == 0 { Preamble::Z } else { Preamble::X };
            encode_subframe(preamble, &a, &mut level, &mut half);
            encode_subframe(Preamble::Y, &b, &mut level, &mut half);
        }

        let decoded = decode(&bmc_vcd(&half, 7), params(WordBits::Bits(24)));

        assert_eq!(decoded.rows[0].items.len(), 8);
        assert_eq!(decoded.rows[1].items.len(), 8);
        for frame in 0..8usize {
            assert_eq!(numeric(&decoded.rows[0].items[frame].value), frame as i64);
            assert_eq!(
                numeric(&decoded.rows[1].items[frame].value),
                -(frame as i64)
            );
        }
    }

    #[test]
    fn recovers_various_half_periods() {
        for half_period in [2u64, 5, 10, 100] {
            let mut level = 0u8;
            let mut half = Vec::new();
            let a = subframe_bits(0x1234, 24, 0, 0, 0);
            let b = subframe_bits(-7, 24, 0, 0, 0);
            encode_subframe(Preamble::Z, &a, &mut level, &mut half);
            encode_subframe(Preamble::Y, &b, &mut level, &mut half);

            let decoded = decode(&bmc_vcd(&half, half_period), params(WordBits::Bits(24)));

            assert_eq!(
                numeric(&decoded.rows[0].items[0].value),
                0x1234,
                "half period {half_period}"
            );
            assert_eq!(
                numeric(&decoded.rows[1].items[0].value),
                -7,
                "half period {half_period}"
            );
        }
    }

    #[test]
    fn tolerates_transition_jitter() {
        let mut level = 0u8;
        let mut half = Vec::new();
        for frame in 0..16i64 {
            let a = subframe_bits(frame, 24, 0, 0, 0);
            let b = subframe_bits(-frame, 24, 0, 0, 0);
            let preamble = if frame == 0 { Preamble::Z } else { Preamble::X };
            encode_subframe(preamble, &a, &mut level, &mut half);
            encode_subframe(Preamble::Y, &b, &mut level, &mut half);
        }

        let decoded = decode(&jittered_bmc_vcd(&half, 20, 2), params(WordBits::Bits(24)));

        assert_eq!(decoded.rows[0].items.len(), 16);
        assert_eq!(decoded.rows[1].items.len(), 16);
        for frame in 0..16usize {
            assert_eq!(numeric(&decoded.rows[0].items[frame].value), frame as i64);
            assert_eq!(
                numeric(&decoded.rows[1].items[frame].value),
                -(frame as i64)
            );
        }
    }

    #[test]
    fn rejects_stream_without_preambles() {
        // Data bits alone never contain the three-cell run of a preamble, so
        // a biphase-looking signal without framing is rejected rather than
        // decoded as arbitrary samples.
        let mut level = 0u8;
        let mut half = Vec::new();
        append_bits(
            &[1, 0, 1, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 1],
            &mut level,
            &mut half,
        );

        let container = load_container(&bmc_vcd(&half, 4));
        let resolved = resolve_inputs(
            &container,
            &[DecoderInput {
                role: "data".to_string(),
                variable_ref: VariableRef::from_hierarchy_string("tb.data"),
            }],
        )
        .expect("resolve inputs");
        let settings = DecoderSettings::default();
        let result = BiphaseAudioEngine.decode_params(
            &params(WordBits::Bits(24)),
            &DecoderContext {
                inputs: &resolved,
                settings: &settings,
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn collapses_simultaneous_transitions() {
        // Exporters can emit a provisional value and the intended value at
        // the same timestamp. With no elapsed time there is no biphase gap;
        // the decoder uses the final line state and otherwise decodes
        // exactly the valid stream.
        let mut level = 0u8;
        let mut half = Vec::new();
        let a = subframe_bits(123, 24, 0, 0, 0);
        let b = subframe_bits(-456, 24, 0, 0, 0);
        encode_subframe(Preamble::Z, &a, &mut level, &mut half);
        encode_subframe(Preamble::Y, &b, &mut level, &mut half);

        let valid = bmc_vcd(&half, 4);
        let duplicate_level = 1 - half[0];
        let malformed = valid.replacen("#0\n", &format!("#0\n{duplicate_level}c\n#0\n"), 1);
        let valid_decoded = decode(&valid, params(WordBits::Bits(24)));

        assert_eq!(
            decode(&malformed, params(WordBits::Bits(24))),
            valid_decoded
        );
    }

    #[test]
    fn recovers_after_long_lead_in() {
        // Captures often idle for a long time before the first frame; the
        // idle gap must not be mistaken for a biphase gap or skew the
        // half-period estimate.
        let mut level = 0u8;
        let mut half = Vec::new();
        for frame in 0..8i64 {
            let a = subframe_bits(frame, 24, 0, 0, 0);
            let b = subframe_bits(-frame, 24, 0, 0, 0);
            let preamble = if frame == 0 { Preamble::Z } else { Preamble::X };
            encode_subframe(preamble, &a, &mut level, &mut half);
            encode_subframe(Preamble::Y, &b, &mut level, &mut half);
        }

        let decoded = decode(
            &bmc_vcd_with_lead_in(&half, 5, 50_000),
            params(WordBits::Bits(24)),
        );

        assert_eq!(decoded.rows[0].items.len(), 8);
        assert_eq!(decoded.rows[1].items.len(), 8);
        for frame in 0..8usize {
            assert_eq!(numeric(&decoded.rows[0].items[frame].value), frame as i64);
            assert_eq!(
                numeric(&decoded.rows[1].items[frame].value),
                -(frame as i64)
            );
        }
    }

    #[test]
    fn tolerates_undefined_lead_in() {
        // Simulators often leave the line undefined before the driver starts;
        // those samples are skipped rather than failing the decode.
        let mut level = 0u8;
        let mut half = Vec::new();
        let a = subframe_bits(123, 24, 0, 0, 0);
        let b = subframe_bits(-456, 24, 0, 0, 0);
        encode_subframe(Preamble::Z, &a, &mut level, &mut half);
        encode_subframe(Preamble::Y, &b, &mut level, &mut half);

        let vcd = bmc_vcd_with_lead_in(&half, 4, 1000).replace("#0\n0c\n", "#0\nxc\n");
        let decoded = decode(&vcd, params(WordBits::Bits(24)));

        assert_eq!(numeric(&decoded.rows[0].items[0].value), 123);
        assert_eq!(numeric(&decoded.rows[1].items[0].value), -456);
    }

    #[test]
    fn tolerates_transmitter_glitches() {
        // A run of edges shifted half a cell early produces 0.5 and 1.5
        // half-period gaps. They are still one and two half cells of data,
        // so the stream must decode unchanged.
        let mut level = 0u8;
        let mut half = Vec::new();
        for frame in 0..16i64 {
            let a = subframe_bits(frame, 24, 0, 0, 0);
            let b = subframe_bits(-frame, 24, 0, 0, 0);
            let preamble = if frame == 0 { Preamble::Z } else { Preamble::X };
            encode_subframe(preamble, &a, &mut level, &mut half);
            encode_subframe(Preamble::Y, &b, &mut level, &mut half);
        }

        let decoded = decode(
            &glitched_bmc_vcd(&half, 20, 30, 4),
            params(WordBits::Bits(24)),
        );

        assert_eq!(decoded.rows[0].items.len(), 16);
        assert_eq!(decoded.rows[1].items.len(), 16);
        for frame in 0..16usize {
            assert_eq!(numeric(&decoded.rows[0].items[frame].value), frame as i64);
            assert_eq!(
                numeric(&decoded.rows[1].items[frame].value),
                -(frame as i64)
            );
        }
    }

    #[test]
    fn decodes_with_explicit_bit_clock() {
        let mut level = 0u8;
        let mut half = Vec::new();
        for frame in 0..8i64 {
            let a = subframe_bits(frame, 24, 0, 0, 0);
            let b = subframe_bits(-frame, 24, 0, 0, 0);
            let preamble = if frame == 0 { Preamble::Z } else { Preamble::X };
            encode_subframe(preamble, &a, &mut level, &mut half);
            encode_subframe(Preamble::Y, &b, &mut level, &mut half);
        }

        let container = load_container_with_signals(
            bmc_bitclk_vcd(&half, 4).into_bytes(),
            &["tb.data", "tb.bitclk"],
        );
        let decoded = decode_with_clock(&container, params(WordBits::Bits(24)));

        assert_eq!(decoded.rows[0].items.len(), 8);
        assert_eq!(decoded.rows[1].items.len(), 8);
        for frame in 0..8usize {
            assert_eq!(numeric(&decoded.rows[0].items[frame].value), frame as i64);
            assert_eq!(
                numeric(&decoded.rows[1].items[frame].value),
                -(frame as i64)
            );
        }
    }

    #[test]
    fn decodes_example_file() {
        let path = project_root::get_project_root()
            .expect("project root")
            .join("examples/spdif_2ch_24bit.vcd");
        let bytes = std::fs::read(&path).expect("read example");
        let decoded = decode_container(
            &load_container_with_signals(bytes, &["tb.data"]),
            params(WordBits::Bits(24)),
        );

        let frames = 192usize;
        assert_eq!(decoded.rows[0].items.len(), frames);
        assert_eq!(decoded.rows[1].items.len(), frames);
        for frame in 0..frames {
            let angle = 2.0 * std::f64::consts::PI * frame as f64 / 16.0;
            let left = ((2f64.powi(23) - 1.0) * angle.sin()) as i64;
            let right = ((2f64.powi(23) - 1.0) * angle.cos()) as i64;
            assert_eq!(
                numeric(&decoded.rows[0].items[frame].value),
                left,
                "left frame {frame}"
            );
            assert_eq!(
                numeric(&decoded.rows[1].items[frame].value),
                right,
                "right frame {frame}"
            );
        }
        assert_eq!(
            decoded.rows[2].items[0].value,
            DecodedValue::text("consumer 48 kHz 24-bit")
        );
    }

    #[test]
    fn bundled_schema_loads() {
        let decoder = crate::decoders::decoder_by_id("spdif").expect("bundled spdif decoder");
        assert_eq!(decoder.display_name(), "S/PDIF / AES3");
        assert_eq!(decoder.inputs().len(), 2);
        assert!(decoder.inputs()[0].required);
        assert!(!decoder.inputs()[1].required);
    }

    #[test]
    fn crc_matches_spec_examples() {
        // Tech 3250 Appendix 1, Example 1: byte 0 bits 0, 2, 3, 4, 5; byte 1
        // bit 1; byte 4 bit 1.
        let mut bytes = [0u8; 24];
        bytes[0] = 0b0011_1101;
        bytes[1] = 0b0000_0010;
        bytes[4] = 0b0000_0010;
        assert_eq!(channel_status_crc(&bytes), 0x9B);

        // Example 2: byte 0 bit 0 only.
        let mut bytes = [0u8; 24];
        bytes[0] = 0b0000_0001;
        assert_eq!(channel_status_crc(&bytes), 0x32);
    }

    #[test]
    fn formats_sample_rates() {
        assert_eq!(format_sample_rate(48_000), "48 kHz");
        assert_eq!(format_sample_rate(44_100), "44.1 kHz");
        assert_eq!(format_sample_rate(22_050), "22.05 kHz");
        assert_eq!(format_sample_rate(705_600), "705.6 kHz");
    }

    #[test]
    fn parses_consumer_channel_status() {
        let status = consumer_status(0x02, 5, true); // 48 kHz, 24-bit
        let mut level = 0u8;
        let mut half = Vec::new();
        encode_block(
            &status,
            &status,
            192,
            |frame| (frame as i64, 0),
            24,
            &mut level,
            &mut half,
        );

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Bits(24)));

        assert_eq!(decoded.rows[2].name, "status");
        assert_eq!(decoded.rows[2].items.len(), 1);
        assert_eq!(
            decoded.rows[2].items[0].value,
            DecodedValue::text("consumer 48 kHz 24-bit")
        );
        assert!(decoded.rows[3].items.is_empty());
    }

    #[test]
    fn resolves_word_bits_from_channel_status() {
        // Consumer, 44.1 kHz, 20-bit coding range and 20-bit word length.
        let status = consumer_status(0x00, 5, false);
        let mut level = 0u8;
        let mut half = Vec::new();
        encode_block(
            &status,
            &status,
            192,
            |_| (0xABCDE, 0),
            20,
            &mut level,
            &mut half,
        );

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Auto));

        assert_eq!(
            numeric(&decoded.rows[0].items[0].value),
            0xABCDE - (1 << 20)
        );
        assert_eq!(
            decoded.rows[2].items[0].value,
            DecodedValue::text("consumer 44.1 kHz 20-bit")
        );
    }

    #[test]
    fn parses_professional_channel_status() {
        let mut status = [0u8; 24];
        status[0] = 0x01 | (0b10 << 6); // professional, 48 kHz
        status[2] = 0x2C; // 24-bit range (bit 2), 24-bit word (bits 3 and 5)
        status[23] = channel_status_crc(&status);

        let mut level = 0u8;
        let mut half = Vec::new();
        encode_block(&status, &status, 192, |_| (0, 0), 24, &mut level, &mut half);

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Bits(24)));

        assert_eq!(
            decoded.rows[2].items[0].value,
            DecodedValue::text("AES3 48 kHz 24-bit")
        );
    }

    #[test]
    fn flags_status_crc_errors() {
        let mut status = [0u8; 24];
        status[0] = 0x01 | (0b10 << 6);
        status[2] = 0x2C; // 24-bit range, 24-bit word
        status[23] = channel_status_crc(&status) ^ 0x01;

        let mut level = 0u8;
        let mut half = Vec::new();
        encode_block(&status, &status, 192, |_| (0, 0), 24, &mut level, &mut half);

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Bits(24)));

        assert_eq!(
            decoded.rows[2].items[0].value,
            DecodedValue::text("AES3 48 kHz 24-bit CRC error")
        );
    }

    #[test]
    fn parses_extended_professional_sample_rates() {
        for (code, expected) in [
            (0b0001u8, "AES3 24 kHz 24-bit"),
            (0b0010, "AES3 96 kHz 24-bit"),
            (0b0011, "AES3 192 kHz 24-bit"),
            (0b1001, "AES3 22.05 kHz 24-bit"),
            (0b1010, "AES3 88.2 kHz 24-bit"),
            (0b1011, "AES3 176.4 kHz 24-bit"),
        ] {
            let mut status = [0u8; 24];
            status[0] = 0x01; // professional, rate not indicated in byte 0
            status[2] = 0x2C; // 24-bit range, 24-bit word
            status[4] = code << 3;
            status[23] = channel_status_crc(&status);

            assert_eq!(
                parse_channel_status(&status).summary(),
                expected,
                "extended code {code:04b}"
            );
        }
    }

    #[test]
    fn flags_block_count_errors() {
        let status = consumer_status(0x02, 5, true);
        let mut level = 0u8;
        let mut half = Vec::new();
        encode_block(&status, &status, 192, |_| (0, 0), 24, &mut level, &mut half);
        encode_block(&status, &status, 191, |_| (0, 0), 24, &mut level, &mut half);
        encode_block(&status, &status, 192, |_| (0, 0), 24, &mut level, &mut half);

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Bits(24)));

        assert_eq!(decoded.rows[2].items.len(), 2);
        assert_eq!(decoded.rows[3].items.len(), 1);
        assert_eq!(
            decoded.rows[3].items[0].value,
            DecodedValue::text("block count")
        );
    }

    #[test]
    fn flags_incomplete_final_block() {
        let status = consumer_status(0x02, 5, true);
        let mut level = 0u8;
        let mut half = Vec::new();
        encode_block(&status, &status, 192, |_| (0, 0), 24, &mut level, &mut half);
        encode_block(&status, &status, 50, |_| (0, 0), 24, &mut level, &mut half);

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Bits(24)));

        assert_eq!(decoded.rows[2].items.len(), 1);
        assert_eq!(decoded.rows[3].items.len(), 1);
        assert_eq!(
            decoded.rows[3].items[0].value,
            DecodedValue::text("block count")
        );
    }

    #[test]
    fn keeps_error_row_sorted() {
        let status = consumer_status(0x02, 5, true);
        let mut level = 0u8;
        let mut half = Vec::new();
        encode_block(&status, &status, 192, |_| (0, 0), 24, &mut level, &mut half);
        encode_block(&status, &status, 191, |_| (0, 0), 24, &mut level, &mut half);
        // The following block is complete except for one parity error in
        // frame 10. The 191-frame block's count error is only detected when
        // this block's `Z` arrives, i.e. before the parity error in time.
        for frame in 0..192 {
            let preamble = if frame == 0 { Preamble::Z } else { Preamble::X };
            let mut a = subframe_bits(0, 24, 0, 0, status_bit(&status, frame));
            if frame == 10 {
                a[27] ^= 1;
            }
            encode_subframe(preamble, &a, &mut level, &mut half);
            encode_subframe(
                Preamble::Y,
                &subframe_bits(0, 24, 0, 0, status_bit(&status, frame)),
                &mut level,
                &mut half,
            );
        }

        let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Bits(24)));

        let errors = &decoded.rows[3].items;
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(errors[0].value, DecodedValue::text("block count"));
        assert_eq!(errors[1].value, DecodedValue::text("parity"));
        assert!(errors[0].start < errors[1].start, "{errors:?}");
    }

    #[test]
    fn recovers_half_period_when_sample_misses_short_gaps() {
        // With 1024 gaps the estimator samples every second one, and every
        // sampled gap is a full period. The true half period and its double
        // then tie on the sample; the full gap list must break the tie toward
        // the half period rather than the multiple.
        let gaps: Vec<u64> = (0..1024)
            .map(|index| if index % 2 == 0 { 20 } else { 30 })
            .collect();
        assert_eq!(estimate_half_period(&gaps), Some(10));

        assert_eq!(estimate_half_period(&[10, 20, 30, 10, 10]), Some(10));
        assert_eq!(estimate_half_period(&[]), None);
    }

    #[test]
    fn survives_deterministic_mutations() {
        // Single-half-cell mutations must produce a clean result (decoded or
        // error), never a panic or hang.
        let status = consumer_status(0x02, 5, true);
        let mut level = 0u8;
        let mut half = Vec::new();
        encode_block(
            &status,
            &status,
            8,
            |frame| (frame as i64, -(frame as i64)),
            24,
            &mut level,
            &mut half,
        );

        for index in (0..half.len()).step_by(7) {
            let mut mutated = half.clone();
            mutated[index] = 1 - mutated[index];
            let _ = try_decode(&bmc_vcd(&mutated, 4), params(WordBits::Bits(24)));
        }
    }

    #[test]
    fn hides_status_row_when_disabled() {
        let mut level = 0u8;
        let mut half = Vec::new();
        let a = subframe_bits(1, 24, 0, 0, 0);
        encode_subframe(Preamble::X, &a, &mut level, &mut half);

        let mut params = params(WordBits::Bits(24));
        params.show_status = false;
        let decoded = decode(&bmc_vcd(&half, 4), params);

        assert_eq!(decoded.rows.len(), 4); // A, B, errors, preamble
        assert_eq!(decoded.rows[2].name, "errors");
        assert_eq!(decoded.rows[3].name, "preamble");
    }

    /// Tests transcribed from EBU Tech 3250 (`docs/development/tech3250.pdf`).
    ///
    /// These deliberately avoid the encoder helpers' tables: the expected
    /// values are the specification's, so a wrong reading in the
    /// implementation (or in the helpers) fails here. See
    /// `docs/development/spdif-spec-test-deficiencies.md` for the gaps this
    /// module covers.
    mod spec_3250 {
        use super::*;

        /// A professional channel-status block with the given bytes set.
        fn professional_status(bytes: &[(usize, u8)]) -> [u8; 24] {
            let mut status = [0u8; 24];
            status[0] = 0x01; // professional
            for (index, value) in bytes {
                status[*index] = *value;
            }
            status
        }

        fn with_crc(mut status: [u8; 24]) -> [u8; 24] {
            status[23] = channel_status_crc(&status);
            status
        }

        #[test]
        fn byte2_word_length_table() {
            // Tech 3250 §4, byte 2, bits 3-5. The table prints bit 3 first
            // (bit 0 of a channel-status byte is the first transmitted and
            // the LSB), so `0b100` here means bit 3 set. `0x04` (byte bit 2)
            // selects the 24-bit coding range.
            // (bit3, bit4, bit5, word length with max 24, with max 20)
            let rows = [
                (0u8, 0u8, 0u8, None, None),
                (0, 0, 1, Some(23u32), Some(19u32)),
                (0, 1, 0, Some(22), Some(18)),
                (0, 1, 1, Some(21), Some(17)),
                (1, 0, 0, Some(20), Some(16)),
                (1, 0, 1, Some(24), Some(20)),
            ];
            for (b3, b4, b5, with_24, with_20) in rows {
                for (range_24, expected) in [(true, with_24), (false, with_20)] {
                    let mut status = professional_status(&[(2, b3 << 3 | b4 << 4 | b5 << 5)]);
                    if range_24 {
                        status[2] |= 0x04;
                    }
                    // Bits 3-5 = 000 is "not indicated"; the receiver
                    // defaults to the maximum of the coding range.
                    let expected = expected.unwrap_or(if range_24 { 24 } else { 20 });
                    assert_eq!(
                        parse_channel_status(&with_crc(status)).word_bits,
                        Some(expected),
                        "byte 2 bits 3-5 = ({b3},{b4},{b5}), 24-bit range = {range_24}"
                    );
                }
            }
        }

        #[test]
        fn byte2_reserved_aux_states_do_not_select_the_24bit_range() {
            // Tech 3250 §4, byte 2, bits 0-2: only `0 0 1` (byte bit 2)
            // selects the 24-bit range. The other states are the 20-bit
            // default or reserved; reserved states fall back to the default.
            for low in [0b001u8, 0b011, 0b101, 0b111] {
                let status = professional_status(&[(2, low | (0b101 << 3))]);
                assert_eq!(
                    parse_channel_status(&with_crc(status)).word_bits,
                    Some(20),
                    "byte 2 bits 0-2 = {low:03b} are reserved"
                );
            }
        }

        #[test]
        fn byte2_reserved_word_length_codes_are_not_decoded() {
            // Tech 3250 §4, byte 2, bits 3-5: `1 1 0` and `1 1 1` are
            // reserved.
            for (b3, b4, b5) in [(1u8, 1u8, 0u8), (1, 1, 1)] {
                let status = professional_status(&[(2, 0x04 | b3 << 3 | b4 << 4 | b5 << 5)]);
                assert_eq!(
                    parse_channel_status(&with_crc(status)).word_bits,
                    None,
                    "byte 2 bits 3-5 = ({b3},{b4},{b5}) are reserved"
                );
            }
        }

        #[test]
        fn byte0_emphasis_table() {
            // Tech 3250 §4, byte 0, bits 2-4. `1 0 0` is "no emphasis" and
            // must be distinguishable from `0 0 0`, "not indicated".
            for (b2, b3, b4, expected) in [
                (0u8, 0u8, 0u8, None),
                (1, 0, 0, Some("no emphasis")),
                (1, 1, 0, Some("50/15")),
                (1, 1, 1, Some("J.17")),
            ] {
                let mut status = professional_status(&[]);
                status[0] |= b2 << 2 | b3 << 3 | b4 << 4;
                let summary = parse_channel_status(&with_crc(status)).summary();
                match expected {
                    Some(fragment) => assert!(
                        summary.contains(fragment),
                        "byte 0 bits 2-4 = ({b2},{b3},{b4}): {summary}"
                    ),
                    None => assert!(
                        !summary.contains("emphasis"),
                        "byte 0 bits 2-4 = 000: {summary}"
                    ),
                }
            }
        }

        #[test]
        fn byte0_professional_sample_rate_table() {
            // Tech 3250 §4, byte 0, bits 6-7.
            for (b6, b7, expected) in [
                (0u8, 0u8, None),
                (0, 1, Some(48_000u32)),
                (1, 0, Some(44_100)),
                (1, 1, Some(32_000)),
            ] {
                let mut status = professional_status(&[]);
                status[0] |= b6 << 6 | b7 << 7;
                assert_eq!(
                    parse_channel_status(&with_crc(status)).sample_rate,
                    expected,
                    "byte 0 bits 6-7 = ({b6},{b7})"
                );
            }
        }

        #[test]
        fn byte4_professional_sample_rate_table() {
            // Tech 3250 §4, byte 4, bits 3-6, with byte 0 bits 6-7 = 00.
            // (bit3, bit4, bit5, bit6, expected)
            for (b3, b4, b5, b6, expected) in [
                (0u8, 0u8, 0u8, 0u8, None),
                (1, 0, 0, 0, Some(24_000u32)),
                (0, 1, 0, 0, Some(96_000)),
                (1, 1, 0, 0, Some(192_000)),
                (1, 0, 0, 1, Some(22_050)),
                (0, 1, 0, 1, Some(88_200)),
                (1, 1, 0, 1, Some(176_400)),
                (1, 1, 1, 1, None), // user defined
            ] {
                let status =
                    professional_status(&[(2, 0x2C), (4, b3 << 3 | b4 << 4 | b5 << 5 | b6 << 6)]);
                assert_eq!(
                    parse_channel_status(&with_crc(status)).sample_rate,
                    expected,
                    "byte 4 bits 3-6 = ({b3},{b4},{b5},{b6})"
                );
            }
        }

        #[test]
        fn byte4_scaling_flag_is_reported() {
            // Tech 3250 §4, byte 4 bit 7: the indicated rate is divided by
            // 1.001.
            let status = professional_status(&[
                (0, 0x01 | (0b10 << 6)), // 48 kHz in byte 0
                (2, 0x2C),
                (4, 0x80),
            ]);
            let summary = parse_channel_status(&with_crc(status)).summary();
            assert!(summary.contains("48 kHz \u{00f7}1.001"), "{summary}");
        }

        #[test]
        fn spec_preamble_patterns() {
            // Tech 3250 §2.4: X = 11100010, Y = 11100100, Z = 11101000,
            // with the complement used when the preceding state is 1. The
            // literal patterns are used here, not the `PREAMBLES` constant.
            for (label, pattern) in [
                ("Preamble M", [1u8, 1, 1, 0, 0, 0, 1, 0]), // X
                ("Preamble W", [1, 1, 1, 0, 0, 1, 0, 0]),   // Y
                ("Preamble B", [1, 1, 1, 0, 1, 0, 0, 0]),   // Z
            ] {
                for inverted in [false, true] {
                    let mut half: Vec<u8> = pattern
                        .iter()
                        .map(|state| if inverted { 1 - state } else { *state })
                        .collect();
                    let mut level = *half.last().expect("preamble is non-empty");
                    append_bits(&[0u8; 28], &mut level, &mut half);

                    let decoded = decode(&bmc_vcd(&half, 4), params(WordBits::Bits(24)));
                    assert_eq!(
                        decoded.rows[4].items[0].value,
                        DecodedValue::text(label),
                        "{label}, inverted = {inverted}"
                    );
                }
            }
        }

        #[test]
        fn spec_subframe_word_placement() {
            // Tech 3250 §2.2.1: MSB in slot 27; LSB in slot 4 (24-bit),
            // slot 8 (20-bit) or slot 12 (16-bit, the unused LSBs zero).
            for (word_bits, lsb_slot, sample) in [
                (24u32, 4usize, 0x123456i64),
                (20, 8, 0xABCDE),
                (16, 12, 0x1234),
            ] {
                let mut bits = [0u8; 28]; // index 0 = slot 4
                for offset in 0..word_bits {
                    bits[lsb_slot - 4 + offset as usize] = ((sample >> offset) & 1) as u8;
                }
                let ones: u32 = bits[..27].iter().map(|bit| u32::from(*bit)).sum();
                bits[27] = (ones % 2) as u8;

                let mut level = 0u8;
                let mut half = Vec::new();
                encode_subframe(Preamble::X, &bits, &mut level, &mut half);

                let mut decoder_params = params(WordBits::Bits(word_bits));
                decoder_params.signed = false;
                let decoded = decode(&bmc_vcd(&half, 2), decoder_params);
                assert_eq!(
                    numeric(&decoded.rows[0].items[0].value),
                    sample,
                    "{word_bits}-bit word"
                );
            }
        }

        #[test]
        fn spec_parity_even_over_slots_4_to_31() {
            // Tech 3250 §2.2.1: slots 4-31 carry an even number of ones.
            for ones in [0usize, 1, 2, 13, 27] {
                let mut bits = [0u8; 28];
                for bit in bits.iter_mut().take(27).take(ones) {
                    *bit = 1;
                }
                let data_ones: u32 = bits[..27].iter().map(|bit| u32::from(*bit)).sum();
                bits[27] = (data_ones % 2) as u8;

                let mut level = 0u8;
                let mut half = Vec::new();
                encode_subframe(Preamble::X, &bits, &mut level, &mut half);
                let decoded = decode(&bmc_vcd(&half, 2), params(WordBits::Bits(24)));
                assert!(
                    decoded.rows[3].items.is_empty(),
                    "{ones} data ones should have even parity"
                );

                bits[27] ^= 1;
                let mut level = 0u8;
                let mut half = Vec::new();
                encode_subframe(Preamble::X, &bits, &mut level, &mut half);
                let decoded = decode(&bmc_vcd(&half, 2), params(WordBits::Bits(24)));
                assert_eq!(
                    decoded.rows[3].items[0].value,
                    DecodedValue::text("parity"),
                    "{ones} data ones with the parity bit flipped"
                );
            }
        }

        #[test]
        fn spec_minimum_implementation_reports_crc_error() {
            // Tech 3250 §5.2.1: a minimum implementation sends byte 0 =
            // 0x01 and leaves byte 23 at the default 0. A receiver
            // implementing the CRCC must report that as a CRC error.
            let status = professional_status(&[]);
            let parsed = parse_channel_status(&status);
            assert_eq!(parsed.crc_ok, Some(false));
            assert!(
                parsed.summary().contains("CRC error"),
                "{}",
                parsed.summary()
            );
        }
    }
}
