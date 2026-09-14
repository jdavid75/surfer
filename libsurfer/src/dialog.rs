use crate::message::Message;
use ecolor::Color32;
use egui::{Layout, RichText};
use emath::Align;

use crate::decoders::{
    DecoderInput, DecoderSettings, SettingKind, SettingValue, SettingsSchema, SignalDecoder,
    ValueFormat, all_decoders, decoder_by_id,
};
use crate::displayed_item::{
    AnalogRenderStyle, AnalogSettings, AnalogYAxisScale, DisplayedDecoder, DisplayedItemRef,
};
use crate::wave_container::{VariableRef, VariableRefExt};
use crate::wave_data::WaveData;

#[derive(Debug, Default, Copy, Clone)]
pub struct ReloadWaveformDialog {
    /// `true` to persist the setting returned by the dialog.
    do_not_show_again: bool,
}

#[derive(Debug, Default, Copy, Clone)]
pub struct OpenSiblingStateFileDialog {
    do_not_show_again: bool,
}

/// Draw a dialog that asks the user if it wants to load a state file situated in the same directory as the waveform file.
pub(crate) fn draw_open_sibling_state_file_dialog(
    ctx: &egui::Context,
    dialog: OpenSiblingStateFileDialog,
    msgs: &mut Vec<Message>,
) {
    let mut do_not_show_again = dialog.do_not_show_again;
    egui::Window::new("State file detected")
            .auto_sized()
            .collapsible(false)
            .fixed_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                let label = ui.label(RichText::new("A state file was detected in the same directory as the loaded file.\nLoad state?").heading());
                ui.set_width(label.rect.width());
                ui.add_space(5.0);
                ui.checkbox(
                    &mut do_not_show_again,
                    "Remember my decision for this session",
                );
                ui.add_space(14.0);
                ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                    // Sets the style when focused
                    ui.style_mut().visuals.widgets.active.weak_bg_fill = Color32::BLUE;
                    let load_button = ui.button("Load");
                    let dont_load_button = ui.button("Don't load");
                    ctx.memory_mut(|mem| {
                        if !matches!(mem.focused(), Some(id) if id == load_button.id || id == dont_load_button.id)
                        {
                            mem.request_focus(load_button.id);
                        }
                    });

                    if load_button.clicked() {
                        msgs.push(Message::CloseOpenSiblingStateFileDialog {
                            load_state: true,
                            do_not_show_again,
                        });
                    } else if dont_load_button.clicked() {
                        msgs.push(Message::CloseOpenSiblingStateFileDialog {
                            load_state: false,
                            do_not_show_again,
                        });
                    } else if do_not_show_again != dialog.do_not_show_again {
                        msgs.push(Message::UpdateOpenSiblingStateFileDialog(OpenSiblingStateFileDialog {
                            do_not_show_again,
                        }));
                    }
                });
            });
}

/// Draw a dialog that asks for user confirmation before re-loading a file.
/// This is triggered by a file loading event from disk.
pub(crate) fn draw_reload_waveform_dialog(
    ctx: &egui::Context,
    dialog: ReloadWaveformDialog,
    msgs: &mut Vec<Message>,
) {
    let mut do_not_show_again = dialog.do_not_show_again;
    egui::Window::new("File Change")
        .auto_sized()
        .collapsible(false)
        .fixed_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            let label = ui.label(RichText::new("File on disk has changed. Reload?").heading());
            ui.set_width(label.rect.width());
            ui.add_space(5.0);
            ui.checkbox(
                &mut do_not_show_again,
                "Remember my decision for this session",
            );
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                // Sets the style when focused
                ui.style_mut().visuals.widgets.active.weak_bg_fill = Color32::BLUE;
                let reload_button = ui.button("Reload");
                let leave_button = ui.button("Leave");
                ctx.memory_mut(|mem| {
                    if !matches!(mem.focused(), Some(id) if id == reload_button.id || id == leave_button.id)
                    {
                        mem.request_focus(reload_button.id);
                    }
                });

                if reload_button.clicked() {
                    msgs.push(Message::CloseReloadWaveformDialog {
                        reload_file: true,
                        do_not_show_again,
                    });
                } else if leave_button.clicked() {
                    msgs.push(Message::CloseReloadWaveformDialog {
                        reload_file: false,
                        do_not_show_again,
                    });
                } else if do_not_show_again != dialog.do_not_show_again {
                    msgs.push(Message::UpdateReloadWaveformDialog(ReloadWaveformDialog {
                        do_not_show_again,
                    }));
                }
            });
        });
}

/// Transient state for the add/edit decoder dialog.
#[derive(Debug, Clone)]
pub struct DecoderDialog {
    /// `None` when adding a new decoder, `Some` when editing an existing item.
    pub item: Option<DisplayedItemRef>,
    pub decoder: String,
    pub inputs: Vec<DecoderDialogInput>,
    pub settings: DecoderSettings,
    pub show_samples: bool,
    pub value_format: ValueFormat,
    pub analog: Option<AnalogSettings>,
}

/// One input role in the decoder dialog.
#[derive(Debug, Clone)]
pub struct DecoderDialogInput {
    pub role: String,
    pub description: String,
    pub required: bool,
    pub variable: Option<VariableRef>,
    pub filter: String,
}

impl DecoderDialog {
    #[must_use]
    pub fn new(decoder: &dyn SignalDecoder) -> Self {
        Self {
            item: None,
            decoder: decoder.id().to_string(),
            // Optional inputs are listed too, so they can be assigned while
            // adding; `decoder_inputs` drops the ones left unassigned.
            inputs: decoder
                .inputs()
                .iter()
                .map(|spec| DecoderDialogInput {
                    role: spec.role.to_string(),
                    description: spec.description.to_string(),
                    required: spec.required,
                    variable: None,
                    filter: String::new(),
                })
                .collect(),
            settings: decoder.default_settings(),
            show_samples: true,
            value_format: ValueFormat::default(),
            analog: None,
        }
    }

    #[must_use]
    pub fn from_displayed(
        item: DisplayedItemRef,
        displayed: &DisplayedDecoder,
        decoder: &dyn SignalDecoder,
    ) -> Self {
        Self {
            item: Some(item),
            decoder: displayed.decoder.clone(),
            inputs: decoder
                .inputs()
                .iter()
                .filter(|spec| {
                    spec.required || displayed.inputs.iter().any(|input| input.role == spec.role)
                })
                .map(|spec| DecoderDialogInput {
                    role: spec.role.to_string(),
                    description: spec.description.to_string(),
                    required: spec.required,
                    variable: displayed
                        .inputs
                        .iter()
                        .find(|input| input.role == spec.role)
                        .map(|input| input.variable_ref.clone()),
                    filter: String::new(),
                })
                .collect(),
            settings: displayed.settings.clone(),
            show_samples: displayed.show_samples,
            value_format: displayed.value_format,
            analog: displayed.analog,
        }
    }

    fn decoder_inputs(&self) -> Option<Vec<DecoderInput>> {
        if self
            .inputs
            .iter()
            .any(|input| input.required && input.variable.is_none())
        {
            return None;
        }
        Some(
            self.inputs
                .iter()
                .filter_map(|input| {
                    Some(DecoderInput {
                        role: input.role.clone(),
                        variable_ref: input.variable.clone()?,
                    })
                })
                .collect(),
        )
    }
}

/// Draw the add/edit decoder dialog.
pub(crate) fn draw_decoder_dialog(
    ctx: &egui::Context,
    dialog: &mut DecoderDialog,
    waves: &WaveData,
    msgs: &mut Vec<Message>,
) {
    let title = if dialog.item.is_some() {
        "Decoder settings"
    } else {
        "Add decoder"
    };
    let variables = waves
        .inner
        .as_waves()
        .map(super::wave_container::WaveContainer::variables)
        .unwrap_or_default();
    let mut open = true;

    egui::Window::new(title)
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(480.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Type");
                let current = decoder_by_id(&dialog.decoder)
                    .map_or_else(|| dialog.decoder.clone(), |d| d.display_name().to_string());
                egui::ComboBox::from_id_salt("decoder_type")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for decoder in all_decoders() {
                            if ui
                                .selectable_label(
                                    dialog.decoder == decoder.id(),
                                    decoder.display_name(),
                                )
                                .clicked()
                                && dialog.decoder != decoder.id()
                            {
                                dialog.decoder = decoder.id().to_string();
                                dialog.settings = decoder.default_settings();
                                dialog.inputs = decoder
                                    .inputs()
                                    .iter()
                                    .filter(|spec| spec.required)
                                    .map(|spec| DecoderDialogInput {
                                        role: spec.role.to_string(),
                                        description: spec.description.to_string(),
                                        required: spec.required,
                                        variable: None,
                                        filter: String::new(),
                                    })
                                    .collect();
                            }
                        }
                    });
            });

            ui.separator();
            ui.label(RichText::new("Inputs").strong());
            for input in &mut dialog.inputs {
                ui.horizontal(|ui| {
                    let suffix = if input.required { "" } else { " (optional)" };
                    ui.label(format!("{}{suffix}", input.description));
                    let role = input.role.clone();
                    variable_picker(ui, &role, input, &variables);
                });
            }

            ui.separator();
            ui.label(RichText::new("Settings").strong());
            if let Some(decoder) = decoder_by_id(&dialog.decoder) {
                draw_settings_schema(ui, decoder.settings_schema(), &mut dialog.settings);
            }

            ui.separator();
            ui.label(RichText::new("Display").strong());
            ui.checkbox(&mut dialog.show_samples, "Show sample values");
            ui.horizontal(|ui| {
                ui.label("Sample format");
                ui.radio_value(&mut dialog.value_format, ValueFormat::Decimal, "Decimal");
                ui.radio_value(
                    &mut dialog.value_format,
                    ValueFormat::Hexadecimal,
                    "Hexadecimal",
                );
            });
            ui.horizontal(|ui| {
                ui.label("Analog");
                if ui.radio(dialog.analog.is_none(), "Off").clicked() {
                    dialog.analog = None;
                }
                for style in [AnalogRenderStyle::Step, AnalogRenderStyle::Interpolated] {
                    if ui
                        .radio(
                            dialog.analog.map(|analog| analog.render_style) == Some(style),
                            style.label(),
                        )
                        .clicked()
                    {
                        dialog.analog = Some(AnalogSettings {
                            render_style: style,
                            ..dialog
                                .analog
                                .unwrap_or_else(AnalogSettings::decoder_default)
                        });
                    }
                }
            });
            if let Some(analog) = &mut dialog.analog {
                ui.horizontal(|ui| {
                    ui.label("Y-axis scale");
                    for scale in [
                        AnalogYAxisScale::Viewport,
                        AnalogYAxisScale::Global,
                        AnalogYAxisScale::TypeLimits,
                    ] {
                        if ui
                            .radio(analog.y_axis_scale == scale, scale.label())
                            .clicked()
                        {
                            analog.y_axis_scale = scale;
                        }
                    }
                });
            }

            let validation_error = decoder_by_id(&dialog.decoder)
                .and_then(|decoder| decoder.validate_settings(&dialog.settings).err())
                .map(|error| format!("{error:#}"));
            let inputs_assigned = dialog
                .inputs
                .iter()
                .all(|input| !input.required || input.variable.is_some());
            if let Some(error) = &validation_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            } else if !inputs_assigned {
                ui.colored_label(ui.visuals().warn_fg_color, "Assign a signal to every input");
            }

            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                let apply_enabled = inputs_assigned && validation_error.is_none();
                let label = if dialog.item.is_some() {
                    "Apply"
                } else {
                    "Add"
                };
                if ui
                    .add_enabled(apply_enabled, egui::Button::new(label))
                    .clicked()
                    && let Some(inputs) = dialog.decoder_inputs()
                {
                    match dialog.item {
                        Some(item) => msgs.push(Message::UpdateDecoder {
                            item,
                            decoder: dialog.decoder.clone(),
                            inputs,
                            settings: dialog.settings.clone(),
                            show_samples: dialog.show_samples,
                            value_format: dialog.value_format,
                            analog: dialog.analog,
                        }),
                        None => msgs.push(Message::AddDecoder {
                            decoder: dialog.decoder.clone(),
                            inputs,
                            settings: dialog.settings.clone(),
                            show_samples: dialog.show_samples,
                            value_format: dialog.value_format,
                            analog: dialog.analog,
                        }),
                    }
                    msgs.push(Message::HideDecoderDialog);
                }
                if ui.button("Cancel").clicked() {
                    msgs.push(Message::HideDecoderDialog);
                }
            });
        });

    if !open {
        msgs.push(Message::HideDecoderDialog);
    }
}

fn variable_picker(
    ui: &mut egui::Ui,
    id_salt: &str,
    input: &mut DecoderDialogInput,
    variables: &[VariableRef],
) {
    let selected_text = input.variable.as_ref().map_or_else(
        || "Select a signal…".to_string(),
        VariableRef::full_path_string,
    );
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(selected_text)
        .width(320.0)
        .show_ui(ui, |ui| {
            ui.add(egui::TextEdit::singleline(&mut input.filter).hint_text("Filter"));
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .show(ui, |ui| {
                    let filter = input.filter.to_lowercase();
                    for variable in variables {
                        let name = variable.full_path_string();
                        if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
                            continue;
                        }
                        if ui
                            .selectable_label(input.variable.as_ref() == Some(variable), &name)
                            .clicked()
                        {
                            input.variable = Some(variable.clone());
                            ui.close();
                        }
                    }
                });
        });
}

fn draw_settings_schema(
    ui: &mut egui::Ui,
    schema: &SettingsSchema,
    settings: &mut DecoderSettings,
) {
    egui::Grid::new("decoder_settings")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for field in &schema.fields {
                ui.label(&field.label);
                match &field.kind {
                    SettingKind::Bool => {
                        let mut value = settings.get_bool(&field.key, false);
                        if ui.checkbox(&mut value, "").changed() {
                            settings.set(field.key.clone(), SettingValue::Bool(value));
                        }
                    }
                    SettingKind::Integer { min, max } => {
                        let mut value = settings.get_integer(&field.key, *min);
                        if ui
                            .add(egui::DragValue::new(&mut value).range(*min..=*max))
                            .changed()
                        {
                            settings.set(field.key.clone(), SettingValue::Integer(value));
                        }
                    }
                    SettingKind::Enum { options } => {
                        let current = settings.get_enum(
                            &field.key,
                            options.first().map_or("", |option| &option.value),
                        );
                        let current_label = options
                            .iter()
                            .find(|option| option.value == current)
                            .map_or(current.as_str(), |option| option.label.as_str());
                        egui::ComboBox::from_id_salt(&field.key)
                            .selected_text(current_label)
                            .show_ui(ui, |ui| {
                                for option in options {
                                    if ui
                                        .selectable_label(current == option.value, &option.label)
                                        .clicked()
                                    {
                                        settings.set(
                                            field.key.clone(),
                                            SettingValue::Enum(option.value.clone()),
                                        );
                                    }
                                }
                            });
                    }
                }
                ui.end_row();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn displayed_with(inputs: Vec<DecoderInput>) -> DisplayedDecoder {
        let decoder = decoder_by_id("spdif").expect("bundled spdif decoder");
        DisplayedDecoder {
            decoder: "spdif".to_string(),
            inputs,
            settings: decoder.default_settings(),
            display_name: decoder.display_name().to_string(),
            manual_name: None,
            color: None,
            background_color: None,
            rows: 4,
            show_samples: true,
            value_format: ValueFormat::default(),
            analog: None,
            height_scaling_factor: None,
            row_names: Vec::new(),
            cache: None,
        }
    }

    #[test]
    fn add_dialog_shows_optional_inputs() {
        let decoder = decoder_by_id("spdif").expect("bundled spdif decoder");
        let mut dialog = DecoderDialog::new(&*decoder);

        assert_eq!(dialog.inputs.len(), 2);
        assert_eq!(dialog.inputs[0].role, "data");
        assert!(dialog.inputs[0].required);
        assert_eq!(dialog.inputs[1].role, "bitclk");
        assert!(!dialog.inputs[1].required);

        // An unassigned optional input is simply left out.
        dialog.inputs[0].variable = Some(VariableRef::from_hierarchy_string("tb.data"));
        let inputs = dialog.decoder_inputs().expect("required input assigned");
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].role, "data");
    }

    #[test]
    fn edit_dialog_shows_assigned_optional_inputs() {
        let decoder = decoder_by_id("spdif").expect("bundled spdif decoder");
        let displayed = displayed_with(vec![
            DecoderInput {
                role: "data".to_string(),
                variable_ref: VariableRef::from_hierarchy_string("tb.data"),
            },
            DecoderInput {
                role: "bitclk".to_string(),
                variable_ref: VariableRef::from_hierarchy_string("tb.bitclk"),
            },
        ]);
        let dialog = DecoderDialog::from_displayed(DisplayedItemRef(1), &displayed, &*decoder);

        assert_eq!(dialog.inputs.len(), 2);
        assert_eq!(dialog.inputs[1].role, "bitclk");
        assert!(!dialog.inputs[1].required);
    }

    #[test]
    fn edit_dialog_hides_unassigned_optional_inputs() {
        let decoder = decoder_by_id("spdif").expect("bundled spdif decoder");
        let displayed = displayed_with(vec![DecoderInput {
            role: "data".to_string(),
            variable_ref: VariableRef::from_hierarchy_string("tb.data"),
        }]);
        let dialog = DecoderDialog::from_displayed(DisplayedItemRef(1), &displayed, &*decoder);

        assert_eq!(dialog.inputs.len(), 1);
        assert_eq!(dialog.inputs[0].role, "data");
    }
}
