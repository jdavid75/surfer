use ecolor::Color32;
use egui::{FontId, PointerButton, Response, Sense, Ui};
use emath::{Align2, Pos2, Rect, RectTransform, Vec2};
use epaint::{
    CornerRadius, CubicBezierShape, PathShape, PathStroke, RectShape, Rgba, Shape, Stroke,
};
use eyre::WrapErr as _;
use ftr_parser::types::{Transaction, TxGenerator};
use itertools::Itertools;
use num::bigint::{ToBigInt, ToBigUint};
use num::{BigInt, BigUint, ToPrimitive};
use rayon::prelude::{IntoParallelIterator, IntoParallelRefIterator, ParallelIterator};
use std::collections::HashMap;
use std::f32::consts::PI;
use surfer_translation_types::{
    NumericRange, SubFieldFlatTranslationResult, TranslatedValue, ValueKind, VariableInfo,
};
use tracing::{error, warn};

use crate::CachedDrawData::TransactionDrawData;
use crate::analog_renderer::{AnalogDrawingCommand, variable_analog_draw_commands};
use crate::clock_highlighting::draw_clock_edge_marks;
use crate::config::{FocusHighlight, SurferTheme};
use crate::data_container::DataContainer;
use crate::displayed_item::{
    AnalogSettings, DisplayedFieldRef, DisplayedItemRef, DisplayedVariable,
};
use crate::item_drawing_info::ItemDrawingInfo;
use crate::time::TimeFormatter;
use crate::tooltips::handle_transaction_tooltip;
use crate::trace_style::{TraceStyle, TraceValue};
use crate::transaction_container::{TransactionRef, TransactionStreamRef};
use crate::translation::{TranslationResultExt, TranslatorList, ValueKindExt, VariableInfoExt};
use crate::view::{DrawConfig, DrawingContext};
use crate::viewport::Viewport;
use crate::wave_container::{QueryResult, VariableRefExt};
use crate::wave_data::{TimeRange, WaveData};
use crate::{
    CachedDrawData, CachedTransactionDrawData, CachedWaveDrawData, Message, SystemState,
    displayed_item::DisplayedItem,
};

pub struct DrawnRegion {
    pub inner: Option<TranslatedValue>,
    /// True if a transition should be drawn even if there is no change in the value
    /// between the previous and next pixels. Only used by the bool drawing logic to
    /// draw draw a vertical line and prevent apparent aliasing
    force_anti_alias: bool,
    trace_value: TraceValue,
}

pub enum DrawingCommands {
    Digital(DigitalDrawingCommands),
    Analog(AnalogDrawingCommands),
    Decoder(DecoderDrawingCommands),
}

#[derive(Default)]
pub struct DecoderDrawingCommands {
    pub loading: bool,
    pub error: Option<String>,
    pub samples: Option<Vec<Vec<DecoderCell>>>,
    pub analog: Option<Vec<Option<AnalogDrawingCommands>>>,
}

pub struct DecoderCell {
    pub x_start: f32,
    pub x_end: f32,
    pub row: String,
    pub start: BigUint,
    pub end: BigUint,
    pub text: String,
    pub kind: ValueKind,
}

impl DecoderDrawingCommands {
    /// Indices of the items that can affect the visible range, plus one item
    /// on each side so that cells and segments crossing the viewport edge are
    /// still drawn.
    fn visible_item_range(
        items: &[crate::decoders::DecodedItem],
        range: &TimeRange,
    ) -> std::ops::Range<usize> {
        let start = items
            .partition_point(|item| BigInt::from(item.start.clone()) < range.start)
            .saturating_sub(1);
        let end = (items.partition_point(|item| BigInt::from(item.start.clone()) <= range.end) + 1)
            .min(items.len());
        start..end.max(start)
    }

    fn samples_from_decoded(
        format: crate::decoders::ValueFormat,
        decoded: &crate::decoders::DecodedData,
        viewport: &Viewport,
        view_width: f32,
        range: &TimeRange,
    ) -> Vec<Vec<DecoderCell>> {
        decoded
            .rows
            .iter()
            .map(|row| {
                let visible = Self::visible_item_range(&row.items, range);
                row.items[visible.clone()]
                    .iter()
                    .enumerate()
                    .map(|(offset, item)| {
                        let index = visible.start + offset;
                        let x_start = viewport.pixel_from_time(
                            &BigInt::from(item.start.clone()),
                            view_width,
                            range,
                        );
                        let end_time = row
                            .items
                            .get(index + 1)
                            .map_or(&item.end, |next| &next.start);
                        let x_end = viewport.pixel_from_time(
                            &BigInt::from(end_time.clone()),
                            view_width,
                            range,
                        );
                        DecoderCell {
                            x_start,
                            x_end,
                            row: row.name.clone(),
                            start: item.start.clone(),
                            end: end_time.clone(),
                            text: item.value.format(format),
                            kind: item.value.kind(),
                        }
                    })
                    .collect()
            })
            .collect()
    }

    fn analog_from_decoded(
        decoded: &crate::decoders::DecodedData,
        analog_settings: AnalogSettings,
        viewport: &Viewport,
        view_width: f32,
        range: &TimeRange,
    ) -> Vec<Option<AnalogDrawingCommands>> {
        let min_valid_pixel = viewport.pixel_from_time(&BigInt::from(0), view_width, range);
        let max_valid_pixel = viewport.pixel_from_time(&range.end, view_width, range);
        decoded
            .rows
            .iter()
            .map(|row| {
                if !row
                    .items
                    .iter()
                    .any(|item| matches!(item.value, crate::decoders::DecodedValue::Integer { .. }))
                {
                    return None;
                }
                // The global range is over the whole row, so `Global` scaling
                // stays stable while scrolling, but only the visible commands
                // are materialized.
                let mut global_min = f64::INFINITY;
                let mut global_max = f64::NEG_INFINITY;
                for item in &row.items {
                    let value = item.value.analog_value();
                    if value.is_finite() {
                        global_min = global_min.min(value);
                        global_max = global_max.max(value);
                    }
                }

                let visible = Self::visible_item_range(&row.items, range);
                let mut values = Vec::with_capacity(visible.len());
                let mut viewport_min = f64::INFINITY;
                let mut viewport_max = f64::NEG_INFINITY;
                for index in visible {
                    let item = &row.items[index];
                    let start_px = viewport.pixel_from_time(
                        &BigInt::from(item.start.clone()),
                        view_width,
                        range,
                    );
                    let end_time = row
                        .items
                        .get(index + 1)
                        .map_or(&item.end, |next| &next.start);
                    let end_px = viewport.pixel_from_time(
                        &BigInt::from(end_time.clone()),
                        view_width,
                        range,
                    );
                    let value = item.value.analog_value();
                    let next_value = row
                        .items
                        .get(index + 1)
                        .map_or(value, |next| next.value.analog_value());
                    if value.is_finite() && end_px >= 0.0 && start_px <= view_width {
                        viewport_min = viewport_min.min(value);
                        viewport_max = viewport_max.max(value);
                    }
                    values.push(AnalogDrawingCommand::Flat {
                        start_px,
                        start_val: value,
                        end_px,
                        end_val: next_value,
                    });
                }
                if !global_min.is_finite() {
                    global_min = 0.0;
                    global_max = 1.0;
                }
                if !viewport_min.is_finite() {
                    viewport_min = global_min;
                    viewport_max = global_max;
                }
                Some(AnalogDrawingCommands::Ready {
                    viewport_min,
                    viewport_max,
                    global_min,
                    global_max,
                    type_limits: row.numeric_range,
                    values,
                    min_valid_pixel,
                    max_valid_pixel,
                    analog_settings,
                })
            })
            .collect()
    }
}

pub enum AnalogDrawingCommands {
    /// Cache is still being built
    Loading,
    /// Cache is ready with drawing data
    Ready {
        /// Viewport min/max for the visible signal range (used for Y-axis scaling)
        viewport_min: f64,
        viewport_max: f64,
        /// Global min/max across entire signal (used for global Y-axis scaling)
        global_min: f64,
        global_max: f64,
        /// Type limits min/max from the translator (used for `TypeLimits` Y-axis scaling)
        type_limits: Option<NumericRange>,
        /// Per-pixel drawing commands with flat spans and ranges
        values: Vec<AnalogDrawingCommand>,
        /// Pixel position of timestamp 0 (start of signal data).
        min_valid_pixel: f32,
        /// Pixel position of last timestamp (end of signal data).
        max_valid_pixel: f32,
        analog_settings: AnalogSettings,
    },
}
#[derive(Clone, PartialEq, Debug)]
pub enum DigitalDrawingType {
    Bool,
    Clock,
    Event,
    Vector,
}

impl From<&VariableInfo> for DigitalDrawingType {
    fn from(info: &VariableInfo) -> Self {
        match info {
            VariableInfo::Bool => DigitalDrawingType::Bool,
            VariableInfo::Clock => DigitalDrawingType::Clock,
            VariableInfo::Event => DigitalDrawingType::Event,
            _ => DigitalDrawingType::Vector,
        }
    }
}
/// List of values to draw for a variable.
///
/// It is an ordered list of values that should be drawn at the *start time*
/// until the *start time* of the next value.
pub struct DigitalDrawingCommands {
    pub drawing_type: DigitalDrawingType,
    pub values: Vec<(f32, DrawnRegion)>,
}

impl DigitalDrawingCommands {
    #[must_use]
    pub fn new_from_variable_info(info: &VariableInfo) -> Self {
        DigitalDrawingCommands {
            drawing_type: DigitalDrawingType::from(info),
            values: vec![],
        }
    }

    pub fn push(&mut self, val: (f32, DrawnRegion)) {
        self.values.push(val);
    }
}

pub struct TxDrawingCommands {
    min: Pos2,
    max: Pos2,
    gen_ref: TransactionStreamRef, // makes it easier to later access the actual Transaction object
}

pub(crate) struct VariableDrawCommands {
    pub(crate) draw_clock_edges: bool,
    pub(crate) clock_edges: Vec<f32>,
    pub(crate) display_id: DisplayedItemRef,
    pub(crate) local_commands: HashMap<Vec<String>, DrawingCommands>,
    pub(crate) local_msgs: Vec<Message>,
}

/// Common setup for variable draw commands: extracts metadata and determines rendering mode.
/// Routes to either analog or digital command generation.
#[allow(clippy::too_many_arguments)]
fn variable_draw_commands(
    displayed_variable: &DisplayedVariable,
    display_id: DisplayedItemRef,
    timestamps: &[(f32, num::BigUint)],
    waves: &WaveData,
    translators: &TranslatorList,
    view_width: f32,
    viewport_idx: usize,
    trace_style: TraceStyle,
) -> Option<VariableDrawCommands> {
    let wave_container = waves.inner.as_waves()?;

    let signal_id = wave_container
        .signal_id(&displayed_variable.variable_ref)
        .ok()?;
    if !wave_container.is_signal_loaded(&signal_id) {
        return None;
    }

    let meta = match wave_container
        .variable_meta(&displayed_variable.variable_ref)
        .context("failed to get variable meta")
    {
        Ok(meta) => meta,
        Err(e) => {
            warn!("{e:#?}");
            return None;
        }
    };

    let displayed_field_ref: DisplayedFieldRef = display_id.into();
    let translator = waves.variable_translator_with_meta(&displayed_field_ref, translators, &meta);
    let info = translator.variable_info(&meta).unwrap();

    let is_analog_mode = displayed_variable.analog.is_some();
    let is_bool = matches!(
        info,
        VariableInfo::Bool | VariableInfo::Clock | VariableInfo::Event
    );

    if is_analog_mode && !is_bool {
        variable_analog_draw_commands(
            displayed_variable,
            display_id,
            waves,
            translators,
            view_width,
            viewport_idx,
        )
    } else {
        variable_digital_draw_commands(
            displayed_variable,
            display_id,
            timestamps,
            waves,
            translators,
            wave_container,
            &meta,
            translator,
            &info,
            view_width,
            viewport_idx,
            trace_style,
        )
    }
}

/// Generate draw commands for digital waveform rendering.
#[allow(clippy::too_many_arguments)]
fn variable_digital_draw_commands(
    displayed_variable: &DisplayedVariable,
    display_id: DisplayedItemRef,
    timestamps: &[(f32, num::BigUint)],
    waves: &WaveData,
    translators: &TranslatorList,
    wave_container: &crate::wave_container::WaveContainer,
    meta: &crate::wave_container::VariableMeta,
    translator: &crate::translation::DynTranslator,
    info: &VariableInfo,
    view_width: f32,
    viewport_idx: usize,
    trace_style: TraceStyle,
) -> Option<VariableDrawCommands> {
    let range = waves.time_range();
    let mut clock_edges = vec![];
    let mut local_msgs = vec![];
    let displayed_field_ref: DisplayedFieldRef = display_id.into();

    let mut local_commands: HashMap<Vec<String>, DigitalDrawingCommands> = HashMap::new();

    let mut prev_values = HashMap::new();

    // In order to insert a final draw command at the end of a trace,
    // we need to know if this is the last timestamp to draw
    let end_pixel = timestamps.iter().last().map(|t| t.0).unwrap_or_default();
    // The first pixel we actually draw is the second pixel in the
    // list, since we skip one pixel to have a previous value
    let start_pixel = timestamps.get(1).map(|t| t.0).unwrap_or_default();

    // Iterate over all the time stamps to draw on
    let mut next_change = timestamps.first().map(|t| t.0).unwrap_or_default();
    for ((_, prev_time), (pixel, time)) in timestamps.iter().zip(timestamps.iter().skip(1)) {
        let is_last_timestep = pixel == &end_pixel;
        let is_first_timestep = pixel == &start_pixel;

        if *pixel < next_change && !is_first_timestep && !is_last_timestep {
            continue;
        }

        let query_result = wave_container.query_variable(&displayed_variable.variable_ref, time);
        next_change = match &query_result {
            Ok(Some(QueryResult {
                next: Some(timestamp),
                ..
            })) => waves.viewports[viewport_idx].pixel_from_time(
                &timestamp.to_bigint().unwrap(),
                view_width,
                range,
            ),
            // If we don't have a next timestamp, we don't need to recheck until the last time
            // step
            Ok(_) => timestamps.last().map(|t| t.0).unwrap_or_default(),
            // If we get an error here, we'll let the next match block handle it, but we'll take
            // note that we need to recheck every pixel until the end
            _ => timestamps.first().map(|t| t.0).unwrap_or_default(),
        };

        let (change_time, val) = match query_result {
            Ok(Some(QueryResult {
                current: Some((change_time, val)),
                ..
            })) => (change_time, val),
            Ok(Some(QueryResult { current: None, .. }) | None) => continue,
            Err(e) => {
                error!("Variable query error {e:#?}");
                continue;
            }
        };

        // Check if the value remains unchanged between this pixel
        // and the last
        if &change_time < prev_time && !is_first_timestep && !is_last_timestep {
            continue;
        }

        let translation_result = match translator.translate(meta, &val) {
            Ok(result) => result,
            Err(e) => {
                error!(
                    "{translator_name} for {variable_name} failed. Disabling:",
                    translator_name = translator.name(),
                    variable_name = displayed_variable.variable_ref.full_path_string_no_index()
                );
                error!("{e:#}");
                local_msgs.push(Message::ResetVariableFormat(displayed_field_ref));
                return None;
            }
        };

        let fields = translation_result.format_flat(
            &displayed_variable.format,
            &displayed_variable.field_formats,
            translators,
        );

        let trace_value = TraceValue::from_value(&val, meta.num_bits, trace_style);

        for SubFieldFlatTranslationResult { names, value } in fields {
            let entry = local_commands.entry(names.clone()).or_insert_with(|| {
                DigitalDrawingCommands::new_from_variable_info(info.get_subinfo(&names))
            });

            let prev = prev_values.get(&names);

            // If the value changed between this and the previous pixel, we want to
            // draw a transition even if the translated value didn't change.  We
            // only want to do this for root variables, because resolving when a
            // sub-field change is tricky without more information from the
            // translators
            let anti_alias = &change_time > prev_time
                && names.is_empty()
                && wave_container.wants_anti_aliasing();
            let new_value = prev != Some(&value);

            // This is not the value we drew last time
            if new_value || is_last_timestep || anti_alias {
                prev_values
                    .entry(names.clone())
                    .or_insert(value.clone())
                    .clone_from(&value);

                if entry.drawing_type == DigitalDrawingType::Clock {
                    match value.as_ref().map(|result| result.value.as_str()) {
                        Some("1") if !is_last_timestep && !is_first_timestep => {
                            clock_edges.push(*pixel);
                        }
                        Some(_) => {}
                        None => {}
                    }
                }

                entry.push((
                    *pixel,
                    DrawnRegion {
                        inner: value,
                        force_anti_alias: anti_alias && !new_value,
                        trace_value,
                    },
                ));
            }
        }
    }
    let draw_clock_edges = match clock_edges.as_slice() {
        [] => false,
        [_single] => true,
        [first, second, ..] => second - first > 20.,
    };

    Some(VariableDrawCommands {
        draw_clock_edges,
        clock_edges,
        display_id,
        local_commands: local_commands
            .into_iter()
            .map(|(k, v)| (k, DrawingCommands::Digital(v)))
            .collect(),
        local_msgs,
    })
}

impl SystemState {
    pub fn invalidate_draw_commands(&mut self) {
        if let Some(waves) = &self.user.waves {
            for viewport in 0..waves.viewports.len() {
                self.draw_data.borrow_mut()[viewport] = None;
            }
        }
    }

    pub fn generate_draw_commands(
        &self,
        cfg: &DrawConfig,
        msgs: &mut Vec<Message>,
        viewport_idx: usize,
    ) {
        #[cfg(feature = "performance_plot")]
        self.timing.borrow_mut().start("Generate draw commands");
        if let Some(waves) = &self.user.waves {
            let draw_data = match waves.inner {
                DataContainer::Waves(_) => {
                    self.generate_wave_draw_commands(waves, cfg, msgs, viewport_idx)
                }
                DataContainer::Transactions(_) => {
                    self.generate_transaction_draw_commands(waves, cfg, msgs, viewport_idx)
                }
                DataContainer::Empty => None,
            };
            self.draw_data.borrow_mut()[viewport_idx] = draw_data;
        }
        #[cfg(feature = "performance_plot")]
        self.timing.borrow_mut().end("Generate draw commands");
    }

    fn generate_wave_draw_commands(
        &self,
        waves: &WaveData,
        cfg: &DrawConfig,
        msgs: &mut Vec<Message>,
        viewport_idx: usize,
    ) -> Option<CachedDrawData> {
        let mut draw_commands = HashMap::new();

        let max_timestamp = waves.safe_max_timestamp();
        let max_time = max_timestamp.to_f64().unwrap_or(f64::MAX);
        let mut clock_edges_by_clock = vec![];
        let viewport = waves.viewports[viewport_idx];
        let range = waves.time_range();
        // Compute which timestamp to draw in each pixel. We'll draw from -extra_draw_width to
        // width + extra_draw_width in order to draw initial transitions outside the screen
        let timestamps = (-cfg.extra_draw_width..(cfg.canvas_size.x as i32 + cfg.extra_draw_width))
            .into_par_iter()
            .filter_map(|x| {
                let time = viewport
                    .as_absolute_time(f64::from(x), cfg.canvas_size.x, range)
                    .0;
                if time < 0. || time > max_time {
                    None
                } else {
                    Some((x as f32, time.to_biguint().unwrap_or_default()))
                }
            })
            .collect::<Vec<_>>();

        let trace_style = self.trace_style();
        let translators = &self.translators;
        let commands = waves
            .items_tree
            .iter_visible()
            .map(|node| (node.item_ref, waves.displayed_items.get(&node.item_ref)))
            .filter_map(|(id, item)| match item {
                Some(DisplayedItem::Variable(variable_ref)) => Some((id, variable_ref)),
                _ => None,
            })
            .collect::<Vec<_>>()
            .par_iter()
            .cloned()
            // Iterate over the variables, generating draw commands for all the
            // subfields
            .filter_map(|(id, displayed_variable)| {
                variable_draw_commands(
                    displayed_variable,
                    id,
                    &timestamps,
                    waves,
                    translators,
                    cfg.canvas_size.x,
                    viewport_idx,
                    trace_style,
                )
            })
            .collect::<Vec<_>>();

        let mut clock_variable_count = 0usize;
        for VariableDrawCommands {
            draw_clock_edges,
            clock_edges: mut new_clock_edges,
            display_id,
            local_commands,
            mut local_msgs,
        } in commands
        {
            msgs.append(&mut local_msgs);
            for (field, val) in local_commands {
                draw_commands.insert(
                    DisplayedFieldRef {
                        item: display_id,
                        field,
                    },
                    val,
                );
            }

            let is_clock_variable = !new_clock_edges.is_empty();
            if is_clock_variable {
                if draw_clock_edges {
                    clock_edges_by_clock
                        .push((clock_variable_count, std::mem::take(&mut new_clock_edges)));
                }
                clock_variable_count += 1;
            }
        }

        for (id, item) in waves
            .items_tree
            .iter_visible()
            .map(|node| (node.item_ref, waves.displayed_items.get(&node.item_ref)))
        {
            let Some(DisplayedItem::Decoder(displayed_decoder)) = item else {
                continue;
            };
            let Some(wave_container) = waves.inner.as_waves() else {
                continue;
            };
            if !wave_container.supports_decoders() {
                continue;
            }
            let field = DisplayedFieldRef {
                item: id,
                field: vec![],
            };
            let cache_key = match crate::decoders::cache_key(
                wave_container,
                &displayed_decoder.inputs,
                &displayed_decoder.settings,
            ) {
                Ok(cache_key) => cache_key,
                Err(error) => {
                    draw_commands.insert(
                        field,
                        DrawingCommands::Decoder(DecoderDrawingCommands {
                            error: Some(error.to_string()),
                            ..Default::default()
                        }),
                    );
                    continue;
                }
            };

            let decoded = match &displayed_decoder.cache {
                Some(entry)
                    if entry.generation == waves.cache_generation && entry.key == cache_key =>
                {
                    match entry.get() {
                        Some(Ok(decoded)) => decoded.clone(),
                        Some(Err(error)) => {
                            draw_commands.insert(
                                field,
                                DrawingCommands::Decoder(DecoderDrawingCommands {
                                    error: Some(error.clone()),
                                    ..Default::default()
                                }),
                            );
                            continue;
                        }
                        None => {
                            draw_commands.insert(
                                field,
                                DrawingCommands::Decoder(DecoderDrawingCommands {
                                    loading: true,
                                    ..Default::default()
                                }),
                            );
                            continue;
                        }
                    }
                }
                _ => {
                    msgs.push(Message::BuildDecoderCache {
                        display_id: id,
                        cache_key,
                    });
                    draw_commands.insert(
                        field,
                        DrawingCommands::Decoder(DecoderDrawingCommands {
                            loading: true,
                            ..Default::default()
                        }),
                    );
                    continue;
                }
            };

            let commands = DecoderDrawingCommands {
                loading: false,
                error: None,
                samples: displayed_decoder.show_samples.then(|| {
                    DecoderDrawingCommands::samples_from_decoded(
                        displayed_decoder.value_format,
                        &decoded,
                        &viewport,
                        cfg.canvas_size.x,
                        range,
                    )
                }),
                analog: displayed_decoder.analog.map(|analog_settings| {
                    DecoderDrawingCommands::analog_from_decoded(
                        &decoded,
                        analog_settings,
                        &viewport,
                        cfg.canvas_size.x,
                        range,
                    )
                }),
            };
            draw_commands.insert(field, DrawingCommands::Decoder(commands));
        }

        let clock_edges = self.get_clock_hightlight_data(clock_edges_by_clock);

        let ticks = self.get_ticks_for_viewport_idx(waves, viewport_idx, cfg);

        Some(CachedDrawData::WaveDrawData(CachedWaveDrawData {
            draw_commands,
            clock_edges,
            ticks,
        }))
    }

    fn generate_transaction_draw_commands(
        &self,
        waves: &WaveData,
        cfg: &DrawConfig,
        msgs: &mut Vec<Message>,
        viewport_idx: usize,
    ) -> Option<CachedDrawData> {
        let mut draw_commands = HashMap::new();
        let mut stream_to_displayed_txs = HashMap::new();
        let mut inc_relation_tx_ids = vec![];
        let mut out_relation_tx_ids = vec![];

        let (focused_tx_ref, old_focused_tx) = &waves.focused_transaction;
        let mut new_focused_tx: Option<&Transaction> = None;

        let viewport = waves.viewports[viewport_idx];
        let range = waves.time_range();

        let displayed_streams = waves
            .items_tree
            .iter_visible()
            .map(|node| node.item_ref)
            .collect::<Vec<_>>()
            .par_iter()
            .map(|id| waves.displayed_items.get(id))
            .filter_map(|item| match item {
                Some(DisplayedItem::Stream(stream_ref)) => Some(stream_ref),
                _ => None,
            })
            .collect::<Vec<_>>();

        let first_visible_timestamp = viewport
            .curr_left
            .absolute(range)
            .0
            .to_biguint()
            .unwrap_or(BigUint::ZERO);

        for displayed_stream in displayed_streams {
            let tx_stream_ref = &displayed_stream.transaction_stream_ref;

            let mut generators: Vec<&TxGenerator> = vec![];
            let mut displayed_transactions = vec![];

            if tx_stream_ref.is_stream() {
                let stream = waves
                    .inner
                    .as_transactions()
                    .unwrap()
                    .get_stream(tx_stream_ref.stream_id)
                    .unwrap();

                for gen_id in &stream.generators {
                    generators.push(
                        waves
                            .inner
                            .as_transactions()
                            .unwrap()
                            .get_generator(*gen_id)
                            .unwrap(),
                    );
                }
            } else {
                generators.push(
                    waves
                        .inner
                        .as_transactions()
                        .unwrap()
                        .get_generator(tx_stream_ref.gen_id.unwrap())
                        .unwrap(),
                );
            }

            for generator in generators {
                // find first visible transaction
                let first_visible_transaction_index =
                    match generator.transactions.binary_search_by_key(
                        &first_visible_timestamp,
                        ftr_parser::types::Transaction::get_end_time,
                    ) {
                        Ok(i) | Err(i) => i,
                    }
                    .saturating_sub(1);
                let transactions = generator
                    .transactions
                    .iter()
                    .skip(first_visible_transaction_index);

                let mut last_px = f32::NAN;

                for tx in transactions {
                    let start_time = tx.get_start_time();
                    let end_time = tx.get_end_time();
                    let curr_tx_id = tx.get_tx_id();

                    // stop drawing after last visible transaction
                    if start_time.to_f64().unwrap() > viewport.curr_right.absolute(range).0 {
                        break;
                    }

                    if let Some(focused_tx_ref) = focused_tx_ref
                        && curr_tx_id == focused_tx_ref.id
                    {
                        new_focused_tx = Some(tx);
                    }

                    let min_px = viewport.pixel_from_time(
                        &start_time.to_bigint().unwrap(),
                        cfg.canvas_size.x - 1.,
                        range,
                    );
                    let max_px = viewport.pixel_from_time(
                        &end_time.to_bigint().unwrap(),
                        cfg.canvas_size.x - 1.,
                        range,
                    );

                    // skip transactions that are rendered completely in the previous pixel
                    if (min_px == max_px) && (min_px == last_px) {
                        last_px = max_px;
                        continue;
                    }
                    last_px = max_px;

                    displayed_transactions.push(TransactionRef { id: curr_tx_id });
                    let min = Pos2::new(min_px, cfg.line_height * tx.row as f32 + 4.0);
                    let max = Pos2::new(max_px, cfg.line_height * (tx.row + 1) as f32 - 4.0);

                    let tx_ref = TransactionRef { id: curr_tx_id };
                    draw_commands.insert(
                        tx_ref,
                        TxDrawingCommands {
                            min,
                            max,
                            gen_ref: TransactionStreamRef::new_gen(
                                tx_stream_ref.stream_id,
                                generator.id,
                                generator.name.clone(),
                            ),
                        },
                    );
                }
            }
            stream_to_displayed_txs.insert(tx_stream_ref.clone(), displayed_transactions);
        }

        if let Some(focused_tx) = new_focused_tx {
            for rel in &focused_tx.inc_relations {
                inc_relation_tx_ids.push(TransactionRef {
                    id: rel.source_tx_id,
                });
            }
            for rel in &focused_tx.out_relations {
                out_relation_tx_ids.push(TransactionRef { id: rel.sink_tx_id });
            }
            if old_focused_tx.is_none() || Some(focused_tx) != old_focused_tx.as_ref() {
                msgs.push(Message::FocusTransaction(
                    focused_tx_ref.clone(),
                    Some(focused_tx.clone()),
                ));
            }
        }

        Some(TransactionDrawData(CachedTransactionDrawData {
            draw_commands,
            stream_to_displayed_txs,
            inc_relation_tx_ids,
            out_relation_tx_ids,
        }))
    }

    /// Calculate the offset reserved for the default timeline header, so the name/value
    /// columns and the canvas all start their rows at the same y.
    pub(crate) fn default_timeline_offset(&self) -> f32 {
        if self.show_default_timeline() {
            self.user.config.layout.waveforms_text_size + self.user.config.layout.waveforms_gap * 4.
        } else {
            0.0
        }
    }

    pub fn draw_items(&mut self, ui: &mut Ui, msgs: &mut Vec<Message>, viewport_idx: usize) {
        let Some(waves) = &self.user.waves else {
            return;
        };

        let (response, mut painter) =
            ui.allocate_painter(ui.available_size(), Sense::click_and_drag());

        let frame_size = response.rect.size();
        let frame_height = frame_size.y;
        let frame_width = frame_size.x;

        if frame_width < 1. || frame_height < 1. {
            return;
        }

        let cfg = match waves.inner {
            DataContainer::Waves(_) => DrawConfig::new(
                Vec2::new(frame_width, frame_height),
                self.user.config.layout.waveforms_line_height,
                self.user.config.layout.waveforms_text_size,
            ),
            DataContainer::Transactions(_) => DrawConfig::new(
                Vec2::new(frame_width, frame_height),
                self.user.config.layout.transactions_line_height,
                self.user.config.layout.waveforms_text_size,
            ),
            DataContainer::Empty => return,
        };
        // the draw commands have been invalidated, recompute
        if self.draw_data.borrow()[viewport_idx].is_none()
            || Some(response.rect) != *self.last_canvas_rect.borrow()
        {
            self.generate_draw_commands(&cfg, msgs, viewport_idx);
            *self.last_canvas_rect.borrow_mut() = Some(response.rect);
        }

        let to_screen =
            RectTransform::from_to(Rect::from_min_size(Pos2::ZERO, frame_size), response.rect);
        let y_zero = to_screen.transform_pos(Pos2::ZERO).y;
        let pointer_pos_global = ui.input(|i| i.pointer.interact_pos());
        let pointer_pos_mouse_gesture =
            pointer_pos_global.map(|p| to_screen.inverse().transform_pos(p));
        let range = waves.time_range();

        if response.clicked_by(PointerButton::Primary)
            || response.clicked_by(PointerButton::Secondary)
            || response.drag_started()
        {
            msgs.push(Message::SetActiveViewport(viewport_idx));
        }

        if ui.ui_contains_pointer() {
            let pointer_pos = pointer_pos_global.unwrap();
            let scroll_delta = ui.input(|i| i.smooth_scroll_delta);
            let mouse_ptr_pos = to_screen.inverse().transform_pos(pointer_pos);
            if scroll_delta != Vec2::ZERO {
                msgs.push(Message::CanvasScroll {
                    delta: scroll_delta,
                    viewport_idx,
                });
            }

            let zoom_delta = ui.input(egui::InputState::zoom_delta);
            if zoom_delta != 1. {
                let mouse_ptr = Some(waves.viewports[viewport_idx].as_time_bigint(
                    mouse_ptr_pos.x,
                    frame_width,
                    range,
                ));

                msgs.push(Message::CanvasZoom {
                    mouse_ptr,
                    delta: zoom_delta,
                    viewport_idx,
                });
            }
        }

        ui.input(|i| {
            // If we have a single touch, we'll interpret that as a pan
            let touch = i.any_touches() && i.multi_touch().is_none();
            let right_mouse = i.pointer.button_down(PointerButton::Secondary);
            if touch || right_mouse {
                msgs.push(Message::CanvasScroll {
                    delta: Vec2 {
                        x: i.pointer.delta().y,
                        y: i.pointer.delta().x,
                    },
                    viewport_idx,
                });
            }
        });

        let modifiers = ui.input(|i| i.modifiers);
        let do_measure = self.do_measure(&modifiers);
        let handle_cursor = !modifiers.command
            && ((response.dragged_by(PointerButton::Primary) && !do_measure)
                || response.clicked_by(PointerButton::Primary));
        let needs_pointer_pos_canvas = self.annotation_kind.is_none() || handle_cursor;
        let pointer_pos_canvas = if needs_pointer_pos_canvas {
            pointer_pos_global.map(|p| to_screen.inverse().transform_pos(p))
        } else {
            None
        };

        // Handle cursor
        if handle_cursor
            && let Some(snap_point) =
                self.snap_to_edge(pointer_pos_canvas, waves, frame_width, viewport_idx)
        {
            msgs.push(Message::CursorSet(snap_point));
        }

        // Draw background
        painter.rect_filled(
            response.rect,
            CornerRadius::ZERO,
            self.user.config.theme.canvas_colors.background,
        );

        // Check for mouse gesture starting
        if response.drag_started_by(PointerButton::Middle)
            || modifiers.command && response.drag_started_by(PointerButton::Primary)
        {
            msgs.push(Message::SetMouseGestureDragStart(
                ui.input(|i| i.pointer.press_origin())
                    .map(|p| to_screen.inverse().transform_pos(p)),
                None,
            ));
        }
        let timeline_offset = self.default_timeline_offset();

        if self.annotation_kind.is_some() && response.drag_started_by(PointerButton::Primary) {
            let start = ui
                .input(|i| i.pointer.press_origin())
                .map(|p| to_screen.inverse().transform_pos(p));
            let time =
                waves.viewports[viewport_idx].as_time_bigint(start.unwrap().x, frame_width, range);
            msgs.push(Message::SetMouseGestureDragStart(start, Some(time)));
        }

        // Check for measure drag starting. Snap the start X to the nearest transition
        // using the same logic as when placing cursors, but keep the original Y.
        if do_measure && response.drag_started_by(PointerButton::Primary) {
            let press_origin_local = ui
                .input(|i| i.pointer.press_origin())
                .map(|p| to_screen.inverse().transform_pos(p));

            let snapped_pos = if let Some(start_pos) = press_origin_local {
                // Snap to nearest edge/time then convert back to pixel X
                if let Some(snap_time) =
                    self.snap_to_edge(Some(start_pos), waves, frame_width, viewport_idx)
                {
                    let x = waves.viewports[viewport_idx].pixel_from_time(
                        &snap_time,
                        frame_width,
                        range,
                    );
                    Some(Pos2 { x, y: start_pos.y })
                } else {
                    Some(start_pos)
                }
            } else {
                None
            };

            msgs.push(Message::SetMeasureDragStart(snapped_pos));
        }

        let mut ctx = DrawingContext {
            painter: &mut painter,
            cfg: &cfg,
            to_screen: &|x, y| to_screen.transform_pos(Pos2::new(x, y)),
            theme: &self.user.config.theme,
        };

        // `waves.drawing_infos` holds offset-free (canonical) positions; the canvas isn't
        // inside a `ScrollArea` like the name/value columns, so scrolling is applied here
        // explicitly instead.
        let row_offset = timeline_offset - waves.scroll_offset;

        let background_offset = y_zero + row_offset;
        let visible_top = -row_offset;
        let visible_bottom = ctx.cfg.canvas_size.y - row_offset;
        for drawing_info in waves.visible_drawing_infos(visible_top, visible_bottom) {
            // Use vidx so all sub-fields of a compound share the same stripe index
            let background_color =
                self.get_background_color(waves, drawing_info.vidx(), drawing_info.vidx().0);

            self.draw_background(drawing_info, background_offset, &ctx, background_color);
        }

        #[cfg(feature = "performance_plot")]
        self.timing.borrow_mut().start("Wave drawing");

        match &self.draw_data.borrow()[viewport_idx] {
            Some(CachedDrawData::WaveDrawData(draw_data)) => {
                self.draw_wave_data(waves, draw_data, row_offset, ui, &mut ctx);
            }
            Some(CachedDrawData::TransactionDrawData(draw_data)) => {
                self.draw_transaction_data(
                    waves,
                    draw_data,
                    viewport_idx,
                    ui,
                    msgs,
                    row_offset,
                    &mut ctx,
                );
            }
            None => {}
        }
        #[cfg(feature = "performance_plot")]
        self.timing.borrow_mut().end("Wave drawing");

        let viewport = &waves.viewports[viewport_idx];
        waves.draw_graphics(&mut ctx, viewport, &self.user.config.theme);

        //Draw cursor and allow measure if no annotation is currently being drawn
        if self.annotation_kind.is_none() {
            waves.draw_cursor(&self.user.config.theme, &mut ctx, viewport);

            self.draw_measure_widget(
                ui,
                waves,
                pointer_pos_canvas,
                pointer_pos_mouse_gesture,
                &response,
                msgs,
                &mut ctx,
                viewport_idx,
            );
        }

        waves.draw_markers(
            &self.user.config.theme,
            &mut ctx,
            &waves.viewports[viewport_idx],
        );

        self.draw_marker_boxes(waves, &mut ctx, viewport, row_offset);

        if self.show_default_timeline() {
            let rect = Rect {
                min: Pos2 { x: 0.0, y: y_zero },
                max: Pos2 {
                    x: response.rect.max.x,
                    y: y_zero + timeline_offset,
                },
            };
            ctx.painter.rect_filled(
                rect,
                CornerRadius::ZERO,
                self.user.config.theme.canvas_colors.background,
            );
            self.draw_default_timeline(waves, &ctx, viewport_idx);
        }

        let time_formatter = TimeFormatter::new(
            &waves.inner.metadata().timescale,
            &self.user.wanted_timeunit,
            &self.get_time_format(),
        );

        self.draw_mouse_gesture_widget(
            ui,
            waves,
            pointer_pos_mouse_gesture,
            &response,
            msgs,
            &mut ctx,
            viewport_idx,
            timeline_offset,
        );

        waves.draw_annotations(
            ui,
            &waves.viewports[viewport_idx],
            viewport_idx,
            &mut ctx,
            &self.user.config.theme,
            msgs,
            timeline_offset,
            response.rect,
            to_screen,
            &time_formatter,
        );

        self.handle_canvas_context_menu(&response, waves, to_screen, &mut ctx, msgs, viewport_idx);
    }

    fn draw_wave_data(
        &self,
        waves: &WaveData,
        draw_data: &CachedWaveDrawData,
        row_offset: f32,
        ui: &mut Ui,
        ctx: &mut DrawingContext,
    ) {
        let clock_edges = &draw_data.clock_edges;
        let draw_commands = &draw_data.draw_commands;
        let draw_clock_edges = clock_edges.has_edges();
        let draw_clock_rising_marker =
            draw_clock_edges && self.user.config.theme.clock_rising_marker;
        let ticks = &draw_data.ticks;
        if !ticks.is_empty() && self.show_ticks() {
            let stroke = Stroke::from(&self.user.config.theme.ticks.style);

            for (_, x, _) in ticks {
                waves.draw_tick_line(*x, ctx, &stroke);
            }
        }

        if draw_clock_edges {
            draw_clock_edge_marks(clock_edges, ctx, &self.user.config);
        }
        // Only the rows visible in the current scroll position need to be drawn; `top`/`bottom`
        // are derived from `waves.drawing_infos`, which are themselves computed purely from the
        // Surfer config layout constants (`waveforms_line_height`/`waveforms_gap`/etc.), not
        // egui defaults.
        let visible_top = -row_offset;
        let visible_bottom = ctx.cfg.canvas_size.y - row_offset;
        for (item_count, drawing_info) in waves
            .visible_drawing_infos(visible_top, visible_bottom)
            .enumerate()
        {
            let y_offset = drawing_info.top_at(row_offset);

            let displayed_item = waves
                .items_tree
                .get_visible(drawing_info.vidx())
                .and_then(|node| waves.displayed_items.get(&node.item_ref));
            let color = displayed_item
                .and_then(super::displayed_item::DisplayedItem::color)
                .and_then(|color| self.user.config.theme.get_color(color));

            match drawing_info {
                ItemDrawingInfo::Variable(variable_info) => {
                    if let Some(commands) = draw_commands.get(&variable_info.displayed_field_ref) {
                        let height_scaling_factor = displayed_item.map_or(
                            1.0,
                            super::displayed_item::DisplayedItem::height_scaling_factor,
                        );
                        let y_offset = y_offset + self.user.config.layout.waveforms_gap;
                        let focus_highlight = if waves.focused_item == Some(drawing_info.vidx()) {
                            self.focus_highlight()
                        } else {
                            FocusHighlight::Off
                        };
                        let line_width = if matches!(
                            focus_highlight,
                            FocusHighlight::LineWidth | FocusHighlight::LineWidthAndBrightnessShift
                        ) {
                            self.user.config.theme.linewidth
                                * self.user.config.theme.focus_highlight_line_width_multiplier
                        } else {
                            self.user.config.theme.linewidth
                        };

                        let color = color.unwrap_or_else(|| {
                            if let Some(DisplayedItem::Variable(variable)) = displayed_item {
                                waves
                                    .inner
                                    .as_waves()
                                    .and_then(|w| w.variable_meta(&variable.variable_ref).ok())
                                    .and_then(|meta| {
                                        if meta.is_event() {
                                            Some(self.user.config.theme.variable_event)
                                        } else if meta.is_parameter() {
                                            Some(self.user.config.theme.variable_parameter)
                                        } else {
                                            None
                                        }
                                    })
                                    .unwrap_or(self.user.config.theme.variable_default)
                            } else {
                                self.user.config.theme.variable_default
                            }
                        });
                        let brightness_shift = if matches!(
                            focus_highlight,
                            FocusHighlight::BrightnessShift
                                | FocusHighlight::LineWidthAndBrightnessShift
                        ) {
                            Some(self.user.config.theme.focus_highlight_brightness_shift)
                        } else {
                            None
                        };
                        match commands {
                            DrawingCommands::Digital(digital_commands) => {
                                match digital_commands.drawing_type {
                                    DigitalDrawingType::Bool | DigitalDrawingType::Clock => {
                                        let draw_clock = (digital_commands.drawing_type
                                            == DigitalDrawingType::Clock)
                                            && draw_clock_rising_marker;
                                        let draw_background = self.fill_high_values();
                                        for (old, new) in digital_commands
                                            .values
                                            .iter()
                                            .zip(digital_commands.values.iter().skip(1))
                                        {
                                            self.draw_bool_transition(
                                                (old, new),
                                                new.1.force_anti_alias,
                                                color,
                                                y_offset,
                                                height_scaling_factor,
                                                draw_clock,
                                                draw_background,
                                                line_width,
                                                brightness_shift,
                                                ctx,
                                            );
                                        }
                                    }
                                    DigitalDrawingType::Event => {
                                        for event in &digital_commands.values {
                                            self.draw_event(
                                                event,
                                                color,
                                                y_offset,
                                                height_scaling_factor,
                                                line_width,
                                                brightness_shift,
                                                ctx,
                                            );
                                        }
                                    }
                                    DigitalDrawingType::Vector => {
                                        // Get background color and determine best text color
                                        let background_color = self.get_background_color(
                                            waves,
                                            drawing_info.vidx(),
                                            item_count,
                                        );

                                        let text_color = self
                                            .user
                                            .config
                                            .theme
                                            .get_best_text_color(background_color);

                                        for (old, new) in digital_commands
                                            .values
                                            .iter()
                                            .zip(digital_commands.values.iter().skip(1))
                                        {
                                            self.draw_region(
                                                (old, new),
                                                color,
                                                y_offset,
                                                height_scaling_factor,
                                                ctx,
                                                text_color,
                                                line_width,
                                                brightness_shift,
                                            );
                                        }
                                    }
                                }
                            }
                            DrawingCommands::Analog(analog_commands) => {
                                crate::analog_renderer::draw_analog(
                                    analog_commands,
                                    color,
                                    y_offset,
                                    height_scaling_factor,
                                    brightness_shift,
                                    ctx,
                                );
                            }
                            DrawingCommands::Decoder(_) => {}
                        }
                    }
                }
                ItemDrawingInfo::Divider(_) | ItemDrawingInfo::Group(_) => {
                    if !self.show_divider_text() {
                        continue;
                    }

                    let text_color = color.unwrap_or(
                        // Get background color and determine best text color
                        self.user
                            .config
                            .theme
                            .get_best_text_color(self.get_background_color(
                                waves,
                                drawing_info.vidx(),
                                item_count,
                            )),
                    );

                    let wave_y_offset = y_offset + self.user.config.layout.waveforms_gap;
                    waves.draw_divider_text(
                        Some(text_color),
                        &displayed_item
                            .map(super::displayed_item::DisplayedItem::name)
                            .unwrap_or_default(),
                        ticks,
                        ctx,
                        wave_y_offset,
                        &self.user.config,
                    );
                }
                ItemDrawingInfo::Marker(_) => {}
                ItemDrawingInfo::TimeLine(_) => {
                    let text_color = color.unwrap_or(
                        // Get background color and determine best text color
                        self.user
                            .config
                            .theme
                            .get_best_text_color(self.get_background_color(
                                waves,
                                drawing_info.vidx(),
                                item_count,
                            )),
                    );
                    let wave_y_offset = y_offset + self.user.config.layout.waveforms_gap;
                    waves.draw_ticks(text_color, ticks, ctx, wave_y_offset, Align2::CENTER_TOP);
                }
                ItemDrawingInfo::Stream(_) => {}
                ItemDrawingInfo::Decoder(decoder_info) => {
                    let Some(DrawingCommands::Decoder(commands)) =
                        draw_commands.get(&DisplayedFieldRef {
                            item: decoder_info.item,
                            field: vec![],
                        })
                    else {
                        continue;
                    };
                    let row_height = drawing_info.height() / (decoder_info.rows + 1) as f32;
                    if commands.loading {
                        ctx.painter.text(
                            (ctx.to_screen)(
                                ctx.cfg.canvas_size.x * 0.5,
                                y_offset + drawing_info.height() * 0.5,
                            ),
                            Align2::CENTER_CENTER,
                            "Building…",
                            FontId::monospace(ctx.cfg.text_size),
                            self.user.config.theme.foreground.gamma_multiply(0.6),
                        );
                        continue;
                    }
                    if let Some(error) = &commands.error {
                        let color = ctx.theme.accent_error.background;
                        let galley = ctx.painter.layout_no_wrap(
                            format!("Decoder failed: {error}"),
                            FontId::monospace(ctx.cfg.text_size),
                            color,
                        );
                        let pos = (ctx.to_screen)(
                            ctx.cfg.text_size * 0.5,
                            y_offset + (drawing_info.height() - galley.size().y) * 0.5,
                        );
                        ctx.painter.galley(pos, galley, color);
                        continue;
                    }
                    if let Some(rows) = &commands.samples {
                        let background =
                            self.get_background_color(waves, drawing_info.vidx(), item_count);
                        let text_color = self.user.config.theme.get_best_text_color(background);
                        let text_size = ctx.cfg.text_size;
                        for (row, cells) in rows.iter().enumerate() {
                            let row_top = y_offset + (row + 1) as f32 * row_height;
                            for cell in cells {
                                let visible_left = cell.x_start.max(0.0);
                                let visible_right = cell.x_end.min(ctx.cfg.canvas_size.x);
                                if visible_right <= visible_left {
                                    continue;
                                }
                                let min = (ctx.to_screen)(cell.x_start, row_top);
                                let max = (ctx.to_screen)(cell.x_end, row_top + row_height);
                                let rect = Rect::from_min_max(min, max);
                                let cell_color = cell.kind.color(
                                    color.unwrap_or(self.user.config.theme.variable_default),
                                    ctx.theme,
                                );
                                ctx.painter.rect_filled(
                                    rect,
                                    CornerRadius::same(2),
                                    cell_color.gamma_multiply(0.25),
                                );
                                let (text_pos, align) =
                                    if cell.x_start >= 0.0 && cell.x_end <= ctx.cfg.canvas_size.x {
                                        (rect.center(), Align2::CENTER_CENTER)
                                    } else {
                                        (
                                            (ctx.to_screen)(
                                                visible_left + ctx.cfg.text_size * 0.5,
                                                row_top + row_height * 0.5,
                                            ),
                                            Align2::LEFT_CENTER,
                                        )
                                    };
                                ctx.painter.text(
                                    text_pos,
                                    align,
                                    &cell.text,
                                    FontId::monospace(text_size),
                                    text_color,
                                );
                                let time_scale = &waves.inner.metadata().timescale;
                                let start = crate::time::time_string(
                                    &BigInt::from(cell.start.clone()),
                                    time_scale,
                                    &self.user.wanted_timeunit,
                                    &self.get_time_format(),
                                );
                                let end = crate::time::time_string(
                                    &BigInt::from(cell.end.clone()),
                                    time_scale,
                                    &self.user.wanted_timeunit,
                                    &self.get_time_format(),
                                );
                                let _ =
                                    ui.allocate_rect(rect, Sense::hover())
                                        .on_hover_text(format!(
                                            "{}: {} @ [{} .. {}]",
                                            cell.row, cell.text, start, end
                                        ));
                            }
                        }
                    }
                    if let Some(channels) = &commands.analog {
                        let gap = self.user.config.layout.waveforms_gap;
                        let channel_color =
                            color.unwrap_or(self.user.config.theme.variable_default);
                        for (row, channel_commands) in channels.iter().enumerate() {
                            let Some(channel_commands) = channel_commands else {
                                continue;
                            };
                            let row_top = y_offset + (row + 1) as f32 * row_height;
                            crate::analog_renderer::draw_analog(
                                channel_commands,
                                channel_color,
                                row_top + gap,
                                1.0,
                                None,
                                ctx,
                            );
                        }
                    }
                }
                ItemDrawingInfo::Placeholder(_) => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_transaction_data(
        &self,
        waves: &WaveData,
        draw_data: &CachedTransactionDrawData,
        viewport_idx: usize,
        ui: &mut Ui,
        msgs: &mut Vec<Message>,
        row_offset: f32,
        ctx: &mut DrawingContext,
    ) {
        let draw_commands = &draw_data.draw_commands;
        let stream_to_displayed_txs = &draw_data.stream_to_displayed_txs;
        let inc_relation_tx_ids = &draw_data.inc_relation_tx_ids;
        let out_relation_tx_ids = &draw_data.out_relation_tx_ids;

        let mut inc_relation_starts = vec![];
        let mut out_relation_starts = vec![];
        let mut focused_transaction_start: Option<Pos2> = None;

        let ticks = self.get_ticks_for_viewport_idx(waves, viewport_idx, ctx.cfg);

        if !ticks.is_empty() && self.show_ticks() {
            let stroke = Stroke::from(&self.user.config.theme.ticks.style);

            for (_, x, _) in &ticks {
                waves.draw_tick_line(*x, ctx, &stroke);
            }
        }

        // Draws the surrounding border of the stream
        let border_stroke = Stroke::new(
            self.user.config.theme.linewidth,
            self.user.config.theme.foreground,
        );

        let visible_top = -row_offset;
        let visible_bottom = ctx.cfg.canvas_size.y - row_offset;
        // Loop over all items to enable drawing relations to non-visible transactions
        for (item_count, drawing_info) in waves.drawing_infos.iter().enumerate() {
            let is_visible = drawing_info.overlaps(visible_top, visible_bottom);
            let y_offset = drawing_info.top_at(row_offset);

            let displayed_item = waves
                .items_tree
                .get_visible(drawing_info.vidx())
                .and_then(|node| waves.displayed_items.get(&node.item_ref));
            let color = displayed_item
                .and_then(super::displayed_item::DisplayedItem::color)
                .and_then(|color| self.user.config.theme.get_color(color));
            let tx_color = color.unwrap_or(self.user.config.theme.transaction_default);

            match drawing_info {
                ItemDrawingInfo::Stream(stream) => {
                    if let Some(tx_refs) =
                        stream_to_displayed_txs.get(&stream.transaction_stream_ref)
                    {
                        for tx_ref in tx_refs {
                            if let Some(tx_draw_command) = draw_commands.get(tx_ref) {
                                let mut min = tx_draw_command.min;
                                let mut max = tx_draw_command.max;

                                min.x = min.x.max(0.);
                                max.x = max.x.min(ctx.cfg.canvas_size.x - 1.);

                                let min = (ctx.to_screen)(min.x, y_offset + min.y);
                                let max = (ctx.to_screen)(max.x, y_offset + max.y);

                                let start = Pos2::new(min.x, f32::midpoint(min.y, max.y));

                                let is_transaction_focused = waves
                                    .focused_transaction
                                    .0
                                    .as_ref()
                                    .is_some_and(|t| t == tx_ref);

                                if inc_relation_tx_ids.contains(tx_ref) {
                                    inc_relation_starts.push(start);
                                } else if out_relation_tx_ids.contains(tx_ref) {
                                    out_relation_starts.push(start);
                                } else if is_transaction_focused {
                                    focused_transaction_start = Some(start);
                                }

                                // Skip rendering if the transaction is not visible
                                if !is_visible {
                                    continue;
                                }

                                let transaction_rect = Rect { min, max };
                                if (max.x - min.x) > 1.0 {
                                    let mut response =
                                        ui.allocate_rect(transaction_rect, Sense::click());

                                    response = handle_transaction_tooltip(
                                        response,
                                        waves,
                                        &tx_draw_command.gen_ref,
                                        tx_ref,
                                    );

                                    if response.clicked() {
                                        msgs.push(Message::FocusTransaction(
                                            Some(tx_ref.clone()),
                                            None,
                                        ));
                                    }

                                    let tx_fill_color = if is_transaction_focused {
                                        // Complementary color for focused transaction
                                        Color32::from_rgb(
                                            255 - tx_color.r(),
                                            255 - tx_color.g(),
                                            255 - tx_color.b(),
                                        )
                                    } else {
                                        tx_color
                                    };

                                    let stroke =
                                        Stroke::new(1.5, tx_fill_color.gamma_multiply(1.2));
                                    ctx.painter.rect(
                                        transaction_rect,
                                        CornerRadius::same(5),
                                        tx_fill_color,
                                        stroke,
                                        epaint::StrokeKind::Middle,
                                    );
                                } else {
                                    let tx_fill_color = tx_color.gamma_multiply(1.2);

                                    let stroke = Stroke::new(1.5, tx_fill_color);
                                    ctx.painter.rect(
                                        transaction_rect,
                                        CornerRadius::ZERO,
                                        tx_fill_color,
                                        stroke,
                                        epaint::StrokeKind::Middle,
                                    );
                                }
                            }
                        }
                        if is_visible {
                            ctx.painter.hline(
                                0.0..=((ctx.to_screen)(ctx.cfg.canvas_size.x, 0.0).x),
                                (ctx.to_screen)(0.0, drawing_info.bottom_at(row_offset)).y,
                                border_stroke,
                            );
                        }
                    }
                }
                ItemDrawingInfo::TimeLine(_) => {
                    if !is_visible {
                        continue;
                    }
                    let text_color = color.unwrap_or(
                        // Get background color and determine best text color
                        self.user
                            .config
                            .theme
                            .get_best_text_color(self.get_background_color(
                                waves,
                                drawing_info.vidx(),
                                item_count,
                            )),
                    );
                    waves.draw_ticks(text_color, &ticks, ctx, y_offset, Align2::CENTER_TOP);
                }
                ItemDrawingInfo::Variable(_) => {}
                ItemDrawingInfo::Divider(_) => {}
                ItemDrawingInfo::Marker(_) => {}
                ItemDrawingInfo::Decoder(_) => {}
                ItemDrawingInfo::Group(_) => {}
                ItemDrawingInfo::Placeholder(_) => {}
            }
        }

        // Draws the relations of the focused transaction
        if let Some(focused_pos) = focused_transaction_start {
            let path_stroke = PathStroke::from(&ctx.theme.relation_arrow.style);
            // let stroke = PathStroke::from({
            // color = self.user.config.theme.annotation_arrow.color
            // width = self.user.config.theme.annotation_arrow.width
            // });
            for start_pos in inc_relation_starts {
                self.draw_arrow(start_pos, focused_pos, ctx, &path_stroke);
            }

            for end_pos in out_relation_starts {
                self.draw_arrow(focused_pos, end_pos, ctx, &path_stroke);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_region(
        &self,
        ((old_x, prev_region), (new_x, _)): (&(f32, DrawnRegion), &(f32, DrawnRegion)),
        user_color: Color32,
        offset: f32,
        height_scaling_factor: f32,
        ctx: &mut DrawingContext,
        text_color: Color32,
        line_width: f32,
        brightness_shift: Option<f32>,
    ) {
        if let Some(prev_result) = &prev_region.inner {
            let color = apply_brightness_shift(
                prev_result.kind.color(user_color, ctx.theme),
                brightness_shift,
                ctx.theme.canvas_colors.background,
            );
            let transition_width = (new_x - old_x).min(ctx.theme.vector_transition_width);

            let trace_coords =
                |x, y| (ctx.to_screen)(x, y * ctx.cfg.line_height * height_scaling_factor + offset);

            let points = vec![
                trace_coords(*old_x, 0.5),
                trace_coords(old_x + transition_width * 0.5, 0.0),
                trace_coords(new_x - transition_width * 0.5, 0.0),
                trace_coords(*new_x, 0.5),
                trace_coords(new_x - transition_width * 0.5, 1.0),
                trace_coords(old_x + transition_width * 0.5, 1.0),
                trace_coords(*old_x, 0.5),
            ];

            if self.draw_vector_unknowns_as_line()
                && matches!(prev_result.kind, ValueKind::HighImp | ValueKind::Undef)
            {
                let stroke = Stroke {
                    color,
                    width: line_width,
                };
                ctx.painter.add(PathShape::line(
                    vec![trace_coords(*old_x, 0.5), trace_coords(*new_x, 0.5)],
                    stroke,
                ));
                return;
            }

            if self.user.config.theme.wide_opacity != 0.0 {
                // For performance, it might be nice to draw both the background and line with this
                // call, but using convex_polygon on our polygons create artefacts on thin transitions.
                ctx.painter.add(PathShape::convex_polygon(
                    points.clone(),
                    color.gamma_multiply(self.user.config.theme.wide_opacity),
                    PathStroke::NONE,
                ));
            }
            match prev_region.trace_value {
                TraceValue::Normal => {
                    let stroke = Stroke {
                        color,
                        width: line_width,
                    };

                    ctx.painter.add(PathShape::line(points, stroke));
                }
                TraceValue::AllOnes => {
                    let stroke_thick = Stroke {
                        color,
                        width: self.user.config.theme.thick_linewidth,
                    };
                    let stroke = Stroke {
                        color,
                        width: self.user.config.theme.linewidth,
                    };
                    ctx.painter
                        .add(PathShape::line(points[0..4].to_vec(), stroke_thick));
                    ctx.painter
                        .add(PathShape::line(points[3..7].to_vec(), stroke));
                }
                TraceValue::AllZeros => {
                    let stroke_thick = Stroke {
                        color,
                        width: self.user.config.theme.linewidth,
                    };
                    ctx.painter
                        .add(PathShape::line(points[3..7].to_vec(), stroke_thick));
                }
                TraceValue::AllZerosThick => {
                    let stroke_thick = Stroke {
                        color,
                        width: self.user.config.theme.thick_linewidth,
                    };
                    ctx.painter
                        .add(PathShape::line(points[3..7].to_vec(), stroke_thick));
                }
            }

            let text_size = ctx.cfg.text_size;
            let char_width = text_size * (20. / 31.);

            let text_area = (new_x - old_x) - transition_width;
            let num_chars = (text_area / char_width).floor() as usize;
            let fits_text = num_chars >= 1;

            if fits_text {
                let content = if prev_result.value.len() > num_chars {
                    prev_result
                        .value
                        .chars()
                        .take(num_chars - 1)
                        .chain(['…'])
                        .collect::<String>()
                } else {
                    prev_result.value.clone()
                };

                ctx.painter.text(
                    trace_coords(*old_x + transition_width, 0.5),
                    Align2::LEFT_CENTER,
                    content,
                    FontId::monospace(text_size),
                    text_color,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_bool_transition(
        &self,
        ((old_x, prev_region), (new_x, new_region)): (&(f32, DrawnRegion), &(f32, DrawnRegion)),
        force_anti_alias: bool,
        color: Color32,
        offset: f32,
        height_scaling_factor: f32,
        draw_clock_marker: bool,
        draw_background: bool,
        line_width: f32,
        brightness_shift: Option<f32>,
        ctx: &mut DrawingContext,
    ) {
        if let (Some(prev_result), Some(new_result)) = (&prev_region.inner, &new_region.inner) {
            let trace_coords =
                |x, y| (ctx.to_screen)(x, y * ctx.cfg.line_height * height_scaling_factor + offset);

            let bg_color = ctx.theme.canvas_colors.background;
            let (old_height, old_color, old_bg) = {
                let (h, c, bg) = prev_result.value.bool_drawing_spec(
                    color,
                    &self.user.config.theme,
                    prev_result.kind,
                );
                (h, apply_brightness_shift(c, brightness_shift, bg_color), bg)
            };
            let (new_height, _, _) =
                new_result
                    .value
                    .bool_drawing_spec(color, &self.user.config.theme, new_result.kind);

            if let (Some(old_bg), true) = (old_bg, draw_background) {
                ctx.painter.add(RectShape::new(
                    Rect {
                        min: (ctx.to_screen)(*old_x, offset),
                        max: (ctx.to_screen)(
                            *new_x,
                            offset
                                + ctx.cfg.line_height * height_scaling_factor
                                + ctx.theme.linewidth * 0.5,
                        ),
                    },
                    CornerRadius::ZERO,
                    old_bg,
                    Stroke::NONE,
                    epaint::StrokeKind::Middle,
                ));
            }

            let stroke = Stroke {
                color: old_color,
                width: line_width,
            };

            if force_anti_alias {
                ctx.painter.add(PathShape::line(
                    vec![trace_coords(*new_x, 0.0), trace_coords(*new_x, 1.0)],
                    stroke,
                ));
            }

            ctx.painter.add(PathShape::line(
                vec![
                    trace_coords(*old_x, 1. - old_height),
                    trace_coords(*new_x, 1. - old_height),
                    trace_coords(*new_x, 1. - new_height),
                ],
                stroke,
            ));

            if draw_clock_marker && (old_height < new_height) {
                ctx.painter.add(PathShape::convex_polygon(
                    vec![
                        trace_coords(*new_x - 2.5, 0.6),
                        trace_coords(*new_x, 0.4),
                        trace_coords(*new_x + 2.5, 0.6),
                    ],
                    old_color,
                    stroke,
                ));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_event(
        &self,
        (x, prev_region): &(f32, DrawnRegion),
        color: Color32,
        offset: f32,
        height_scaling_factor: f32,
        line_width: f32,
        brightness_shift: Option<f32>,
        ctx: &mut DrawingContext,
    ) {
        let color =
            apply_brightness_shift(color, brightness_shift, ctx.theme.canvas_colors.background);
        if prev_region.inner.is_some() {
            let trace_coords =
                |x, y| (ctx.to_screen)(x, y * ctx.cfg.line_height * height_scaling_factor + offset);

            let stroke = Stroke {
                color,
                width: line_width,
            };

            // Draw both at old_x and new_x lines until the drawing commands are reworked to deal with this as a special case
            // Otherwise, not drawing the old_x (new_x) value will cause the first (last) event to not be drawn
            let top = trace_coords(*x, 0.0);
            ctx.painter
                .add(PathShape::line(vec![top, trace_coords(*x, 1.0)], stroke));

            ctx.painter.add(PathShape::convex_polygon(
                vec![
                    trace_coords(*x - 2.5, 0.2),
                    top,
                    trace_coords(*x + 2.5, 0.2),
                ],
                color,
                stroke,
            ));
        }
    }

    /// Draws a curvy arrow from `start` to `end`.
    fn draw_arrow(&self, start: Pos2, end: Pos2, ctx: &DrawingContext, stroke: &PathStroke) {
        let x_diff = (end.x - start.x).max(100.);
        let scaled_x_diff = 0.4 * x_diff;

        let anchor1 = Pos2 {
            x: start.x + scaled_x_diff,
            y: start.y,
        };
        let anchor2 = Pos2 {
            x: end.x - scaled_x_diff,
            y: end.y,
        };

        ctx.painter.add(Shape::CubicBezier(CubicBezierShape {
            points: [start, anchor1, anchor2, end],
            closed: false,
            fill: Default::default(),
            stroke: stroke.clone(),
        }));

        self.draw_arrowheads(anchor2, end, ctx, stroke);
    }

    /// Draws arrowheads for the vector going from `vec_start` to `vec_tip`.
    /// The `angle` has to be in degrees.
    fn draw_arrowheads(
        &self,
        vec_start: Pos2,
        vec_tip: Pos2,
        ctx: &DrawingContext,
        stroke: &PathStroke,
    ) {
        let head_length = ctx.theme.relation_arrow.head_length;

        let vec_x = vec_tip.x - vec_start.x;
        let vec_y = vec_tip.y - vec_start.y;

        let alpha = (PI / 180.) * ctx.theme.relation_arrow.head_angle;

        // calculate the points of the new vector, which forms an angle of the given degrees with the given vector
        let vec_angled_x = vec_x * alpha.cos() + vec_y * alpha.sin();
        let vec_angled_y = -vec_x * alpha.sin() + vec_y * alpha.cos();

        // scale the new vector to be head_length long
        let vec_angled_x = (1. / (vec_angled_y - vec_angled_x).abs()) * vec_angled_x * head_length;
        let vec_angled_y = (1. / (vec_angled_y - vec_angled_x).abs()) * vec_angled_y * head_length;

        let arrowhead_left_x = vec_tip.x - vec_angled_x;
        let arrowhead_left_y = vec_tip.y - vec_angled_y;

        let arrowhead_right_x = vec_tip.x + vec_angled_y;
        let arrowhead_right_y = vec_tip.y - vec_angled_x;

        ctx.painter.add(PathShape::line(
            vec![
                Pos2::new(arrowhead_right_x, arrowhead_right_y),
                vec_tip,
                Pos2::new(arrowhead_left_x, arrowhead_left_y),
            ],
            stroke.clone(),
        ));
    }

    fn handle_canvas_context_menu(
        &self,
        response: &Response,
        waves: &WaveData,
        to_screen: RectTransform,
        ctx: &mut DrawingContext,
        msgs: &mut Vec<Message>,
        viewport_idx: usize,
    ) {
        let frame_size = response.rect.size();
        response.context_menu(|ui| {
            let offset = f32::from(ui.spacing().menu_margin.left);
            let top_left = to_screen.inverse().transform_rect(ui.min_rect()).left_top()
                - Pos2 {
                    x: offset,
                    y: offset,
                };

            let snap_pos =
                self.snap_to_edge(Some(top_left.to_pos2()), waves, frame_size.x, viewport_idx);

            if let Some(time) = snap_pos {
                draw_vertical_line_at_time(
                    &time,
                    ctx,
                    &self.user.config.theme.cursor,
                    &waves.viewports[viewport_idx],
                    waves.time_range(),
                );
                ui.menu_button("Set marker", |ui| {
                    for id in waves.markers.keys().sorted() {
                        ui.button(format!("{id}")).clicked().then(|| {
                            msgs.push(Message::SetMarker {
                                id: *id,
                                time: time.clone(),
                            });
                        });
                    }
                    // At the moment we only support 255 markers, and the cursor is the 255th
                    if waves.can_add_marker() {
                        ui.button("New").clicked().then(|| {
                            msgs.push(Message::AddMarker {
                                time,
                                name: None,
                                move_focus: true,
                            });
                        });
                    }
                });
            }
        });
    }

    /// Takes a pointer pos in the canvas and returns a position that is snapped to transitions
    /// if the cursor is close enough to any transition. If the cursor is on the canvas and no
    /// transitions are close enough for snapping, the raw point will be returned. If the cursor is
    /// off the canvas, `None` is returned
    pub fn snap_to_edge(
        &self,
        pointer_pos_canvas: Option<Pos2>,
        waves: &WaveData,
        frame_width: f32,
        viewport_idx: usize,
    ) -> Option<BigInt> {
        let pos = pointer_pos_canvas?;
        let viewport = &waves.viewports[viewport_idx];
        let range = waves.time_range();
        let timestamp = viewport.as_time_bigint(pos.x, frame_width, range);
        if let Some(utimestamp) = timestamp.to_biguint()
            && let Some(item_ref) = waves.item_ref_at_canvas_y(pos.y)
            && let Some(DisplayedItem::Variable(variable)) = &waves.displayed_items.get(&item_ref)
            && let Ok(Some(res)) = waves
                .inner
                .as_waves()
                .unwrap()
                .query_variable(&variable.variable_ref, &utimestamp)
        {
            let prev_time = &res
                .current
                .and_then(|v| v.0.to_bigint())
                .unwrap_or(BigInt::ZERO);
            let next_time = &res
                .next
                .unwrap_or_default()
                .to_bigint()
                .unwrap_or(BigInt::ZERO);
            let prev = viewport.pixel_from_time(prev_time, frame_width, range);
            let next = viewport.pixel_from_time(next_time, frame_width, range);
            if (prev - pos.x).abs() < (next - pos.x).abs() {
                if (prev - pos.x).abs() <= self.user.config.snap_distance {
                    return Some(prev_time.clone());
                }
            } else if (next - pos.x).abs() <= self.user.config.snap_distance {
                return Some(next_time.clone());
            }
        }
        Some(timestamp)
    }
}

/// Draw a vertical line at the given time with the specified stroke.
#[inline]
pub(crate) fn draw_vertical_line_at_time(
    time: &BigInt,
    ctx: &mut DrawingContext,
    stroke: impl Into<Stroke>,
    viewport: &Viewport,
    range: &TimeRange,
) {
    let x = viewport.pixel_from_time(time, ctx.cfg.canvas_size.x, range);
    ctx.painter.line_segment(
        [
            (ctx.to_screen)(x, 0.),
            (ctx.to_screen)(x, ctx.cfg.canvas_size.y),
        ],
        stroke,
    );
}

fn shift_brightness(color: Color32, delta: f32, background: Color32) -> Color32 {
    // Lighten the color on dark backgrounds (blend toward white),
    // darken it on light backgrounds (blend toward black).
    let bg_luminance = crate::config::get_luminance(background);
    let rgba = Rgba::from(color);
    let result = if bg_luminance < 0.5 {
        // Dark background: lighten
        rgba * (1.0 - delta) + Rgba::WHITE * delta
    } else {
        // Light background: darken
        rgba * (1.0 - delta) + Rgba::BLACK * delta
    };
    Color32::from(result)
}

pub(crate) fn apply_brightness_shift(
    color: Color32,
    brightness_shift: Option<f32>,
    background: Color32,
) -> Color32 {
    match brightness_shift {
        Some(delta) => shift_brightness(color, delta, background),
        None => color,
    }
}

trait VariableExt {
    fn bool_drawing_spec(
        &self,
        user_color: Color32,
        theme: &SurferTheme,
        value_kind: ValueKind,
    ) -> (f32, Color32, Option<Color32>);
}

impl VariableExt for String {
    /// Return the height and color with which to draw this value if it is a boolean
    fn bool_drawing_spec(
        &self,
        user_color: Color32,
        theme: &SurferTheme,
        value_kind: ValueKind,
    ) -> (f32, Color32, Option<Color32>) {
        let color = value_kind.color(user_color, theme);
        let (height, background) = match (value_kind, self) {
            (
                ValueKind::HighImp
                | ValueKind::Undef
                | ValueKind::DontCare
                | ValueKind::Warn
                | ValueKind::Error
                | ValueKind::Custom(_),
                _,
            ) => (0.5, None),
            (ValueKind::Weak, other) => {
                if other.to_lowercase() == "l" {
                    (0., None)
                } else {
                    (1., Some(color.gamma_multiply(theme.waveform_opacity)))
                }
            }
            (ValueKind::Normal, other) => {
                if other == "0" {
                    (0., None)
                } else {
                    (1., Some(color.gamma_multiply(theme.waveform_opacity)))
                }
            }
            (ValueKind::Event, _) => (1., Some(color.gamma_multiply(theme.waveform_opacity))),
        };
        (height, color, background)
    }
}
