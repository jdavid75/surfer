use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use ecolor::Color32;
use egui::{RichText, TextWrapMode};
use egui_extras::{Column, TableBuilder, TableRow};
use eyre::Result;
use tracing::{
    Level,
    field::{Field, Visit},
};
use tracing_subscriber::Layer;

use crate::{SystemState, message::Message};

static RECORD_MUTEX: Mutex<Vec<LogMessage>> = Mutex::new(vec![]);
static SHOW_LOGS_ON_ERROR: AtomicBool = AtomicBool::new(false);
static LOG_FILTER: Mutex<(bool, bool, bool, bool, bool)> =
    Mutex::new((true, true, true, true, true));
#[macro_export]
macro_rules! try_log_error {
    ($expr:expr, $what:expr $(,)?) => {
        if let Err(e) = $expr {
            error!("{}: {}", $what, e)
        }
    };
}

#[derive(Clone)]
pub struct LogMessage {
    pub name: String,
    pub msg: String,
    pub level: Level,
}

struct EguiLogger {}

struct FieldVisitor<'a>(&'a mut BTreeMap<String, String>);

impl Visit for FieldVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

impl<S> Layer<S> for EguiLogger
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut fields = BTreeMap::new();
        event.record(&mut FieldVisitor(&mut fields));

        RECORD_MUTEX
            .lock()
            .expect("Failed to lock logger. Thread poisoned?")
            .push(LogMessage {
                name: event.metadata().module_path().unwrap_or("-").to_string(),
                msg: fields.get("message").cloned().unwrap_or("-".to_string()),
                level: *event.metadata().level(),
            });

        if *event.metadata().level() == Level::ERROR {
            SHOW_LOGS_ON_ERROR.store(true, Ordering::Release);
            if let Some(ctx) = crate::EGUI_CONTEXT.read().unwrap().as_ref() {
                ctx.request_repaint();
            }
        }
    }
}

pub(crate) fn take_error_notification() -> bool {
    SHOW_LOGS_ON_ERROR.swap(false, Ordering::AcqRel)
}

impl SystemState {
    pub fn draw_log_window(&self, ctx: &egui::Context, msgs: &mut Vec<Message>) {
        let mut open = true;
        egui::Window::new("Logs")
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .show(ctx, |ui| {
                {
                    let mut filters = LOG_FILTER.lock().unwrap();
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut filters.0, "Error");
                        ui.checkbox(&mut filters.1, "Warn");
                        ui.checkbox(&mut filters.2, "Info");
                        ui.checkbox(&mut filters.3, "Debug");
                        ui.checkbox(&mut filters.4, "Trace");
                    });
                }

                ui.style_mut().wrap_mode = Some(TextWrapMode::Extend);

                egui::ScrollArea::new([true, false]).show(ui, |ui| {
                    TableBuilder::new(ui)
                        .column(Column::auto().resizable(true))
                        .column(Column::auto().resizable(true))
                        .column(Column::remainder())
                        .vscroll(true)
                        .stick_to_bottom(true)
                        .header(20.0, |mut header| {
                            header.col(|ui| {
                                ui.heading("Level");
                            });
                            header.col(|ui| {
                                ui.heading("Source");
                            });
                            header.col(|ui| {
                                ui.heading("Message");
                            });
                        })
                        .body(|body| {
                            let records = RECORD_MUTEX.lock().unwrap();
                            let filters = LOG_FILTER.lock().unwrap();
                            let filtered: Vec<&LogMessage> = records
                                .iter()
                                .filter(|record| match record.level {
                                    Level::ERROR => filters.0,
                                    Level::WARN => filters.1,
                                    Level::INFO => filters.2,
                                    Level::DEBUG => filters.3,
                                    Level::TRACE => filters.4,
                                })
                                .collect();

                            let heights = filtered
                                .iter()
                                .map(|record| record.msg.lines().count() as f32 * 15.0)
                                .collect::<Vec<_>>();

                            body.heterogeneous_rows(heights.into_iter(), |mut row: TableRow| {
                                let record = filtered[row.index()];
                                row.col(|ui| {
                                    let (color, text) = match record.level {
                                        Level::ERROR => (Color32::RED, "Error"),
                                        Level::WARN => (Color32::YELLOW, "Warn"),
                                        Level::INFO => (Color32::GREEN, "Info"),
                                        Level::DEBUG => (Color32::BLUE, "Debug"),
                                        Level::TRACE => (Color32::GRAY, "Trace"),
                                    };

                                    ui.colored_label(color, text);
                                });
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(record.name.clone())
                                            .color(Color32::GRAY)
                                            .monospace(),
                                    );
                                });
                                row.col(|ui| {
                                    ui.label(RichText::new(record.msg.clone()).monospace());
                                });
                            });
                        });
                })
            });
        if !open {
            msgs.push(Message::SetLogsVisible(false));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn start_logging_with<W>(writer: W) -> Result<()>
where
    W: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    use tracing_subscriber::{Registry, fmt, layer::SubscriberExt};

    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let subscriber = Registry::default()
        .with(
            fmt::layer()
                .without_time()
                .with_writer(writer)
                .with_filter(filter.clone()),
        )
        .with(EguiLogger {}.with_filter(filter));

    tracing::subscriber::set_global_default(subscriber).expect("unable to set global subscriber");

    Ok(())
}

/// Starts the logging and error handling.
///
/// Can be used by unittests to get more insights.
#[cfg(not(target_arch = "wasm32"))]
pub fn start_logging() -> Result<()> {
    start_logging_with(std::io::stdout)
}

/// Like [`start_logging`], but writes the console log to stderr so that
/// command output on stdout stays machine-readable.
#[cfg(not(target_arch = "wasm32"))]
pub fn start_logging_to_stderr() -> Result<()> {
    start_logging_with(std::io::stderr)
}

/// Starts the logging and error handling.
///
/// Can be used by unittests to get more insights.
#[cfg(target_arch = "wasm32")]
pub fn start_logging() -> Result<()> {
    use tracing_subscriber::{Registry, fmt, layer::SubscriberExt};
    use wasm_tracing::WasmLayer;

    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let subscriber = Registry::default()
        .with(fmt::layer().without_time().with_filter(filter.clone()))
        .with(WasmLayer::default())
        .with(EguiLogger {}.with_filter(filter));

    tracing::subscriber::set_global_default(subscriber).expect("unable to set global subscriber");

    Ok(())
}
