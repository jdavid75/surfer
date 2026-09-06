use std::future::Future;

#[cfg(not(target_arch = "wasm32"))]
use camino::Utf8PathBuf;
use rfd::{AsyncFileDialog, FileHandle};
use serde::Deserialize;
#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
use wasm_bindgen::prelude::*;

use crate::SystemState;
use crate::async_util::perform_async_work;
use crate::channels::checked_send_many;
use crate::message::Message;
use crate::transactions::TRANSACTIONS_FILE_EXTENSION;
use crate::wave_source::LoadOptions;

// JS entry points that must be provided by the VS Code extension's webview setup
// (e.g. inside the SURFER_SETUP_HOOKS block or integration.js).
//
// `vscode_show_open_dialog(kind, filters_json)` – asks the extension host to show
//   a native open-file picker.  `kind` is an opaque tag the host echoes back in
//   the inject_message it fires once the user confirms:
//
//   | kind                       | injected Message                               |
//   |----------------------------|------------------------------------------------|
//   | `"waveform_clear"`         | `LoadWaveformFileFromUrl(url, Clear)`          |
//   | `"waveform_keep_available"`| `LoadWaveformFileFromUrl(url, KeepAvailable)`  |
//   | `"waveform_keep_all"`      | `LoadWaveformFileFromUrl(url, KeepAll)`        |
//   | `"command_file"`           | `LoadCommandFileFromUrl(url)`                  |
//   | `"state_file"`             | `LoadStateFromData(bytes)`                     |
//
//   `filters_json` is a JSON array of `{"name":str,"extensions":[str]}` objects.
//
#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
#[wasm_bindgen]
extern "C" {
    fn vscode_show_open_dialog(kind: &str, filters_json: &str);
}

#[derive(Debug, Deserialize)]
pub enum OpenMode {
    Open,
    Switch,
}

pub(crate) struct FileFilter {
    name: &'static str,
    extensions: &'static [&'static str],
}

static WAVEFORM_FILE_FILTER: FileFilter = FileFilter {
    name: "Waveform/Transaction-files (*.vcd, *.fst, *.ghw, *.ftr)",
    extensions: &["vcd", "fst", "ghw", TRANSACTIONS_FILE_EXTENSION],
};

static COMMAND_FILE_FILTER: FileFilter = FileFilter {
    name: "Command-file (*.sucl)",
    extensions: &["sucl"],
};

pub(crate) static STATE_FILE_FILTER: FileFilter = FileFilter {
    name: "Surfer state files (*.surf.ron)",
    extensions: &["surf.ron"],
};

#[cfg(not(target_arch = "wasm32"))]
pub(crate) static FST_EXPORT_FILTER: FileFilter = FileFilter {
    name: "FST files (*.fst)",
    extensions: &["fst"],
};

pub(crate) static GTKWAVE_FILE_FILTER: FileFilter = FileFilter {
    name: "GTKWave files (*.gtkw)",
    extensions: &["gtkw"],
};

#[cfg(any(
    target_os = "macos",
    all(target_arch = "wasm32", not(feature = "vscode"))
))]
// Mac OS file dialogs don't support multi-part extensions like `surf.ron`, so we use a different filter for macOS and possibly on WASM.
pub(crate) static STATE_FILE_FILTER_MACOS: FileFilter = FileFilter {
    name: "Surfer state files (*.ron)",
    extensions: &["ron"],
};

#[cfg(feature = "python")]
static PYTHON_FILE_FILTER: FileFilter = FileFilter {
    name: "Python files (*.py)",
    extensions: &["py"],
};

impl SystemState {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn file_dialog_open<F>(
        &mut self,
        title: &'static str,
        filter: &'static FileFilter,
        messages: F,
    ) where
        F: FnOnce(Utf8PathBuf) -> Vec<Message> + Send + 'static,
    {
        self.file_dialog_open_async(title, filter, |path| async { messages(path) });
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn file_dialog_open_async<F, Fut>(
        &mut self,
        title: &'static str,
        filter: &'static FileFilter,
        messages: F,
    ) where
        F: FnOnce(Utf8PathBuf) -> Fut + Send + 'static,
        Fut: Future<Output = Vec<Message>> + Send + 'static,
    {
        let sender = self.channels.msg_sender.clone();

        perform_async_work(async move {
            if let Some(file) = create_file_dialog(filter, title).pick_file().await {
                let path = file.path().to_path_buf();
                let result = match Utf8PathBuf::from_path_buf(path.clone()) {
                    Ok(utf8_path) => messages(utf8_path).await,
                    Err(_) => vec![Message::Error(eyre::eyre!(
                        "File path '{}' contains invalid UTF-8",
                        path.display()
                    ))],
                };
                checked_send_many(&sender, result);
            }
        });
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "vscode")))]
    pub(crate) fn file_dialog_open<F>(
        &mut self,
        title: &'static str,
        filter: &'static FileFilter,
        messages: F,
    ) where
        F: FnOnce(Vec<u8>) -> Vec<Message> + 'static,
    {
        let sender = self.channels.msg_sender.clone();

        perform_async_work(async move {
            if let Some(file) = create_file_dialog(filter, title).pick_file().await {
                checked_send_many(&sender, messages(file.read().await));
            }
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn file_dialog_save<F, Fut>(
        &mut self,
        title: &'static str,
        filter: &'static FileFilter,
        default_file_name: Option<String>,
        messages: F,
    ) where
        F: FnOnce(FileHandle) -> Fut + Send + 'static,
        Fut: Future<Output = Vec<Message>> + Send + 'static,
    {
        let sender = self.channels.msg_sender.clone();

        perform_async_work(async move {
            let mut dialog = create_file_dialog(filter, title);
            if let Some(file_name) = default_file_name {
                dialog = dialog.set_file_name(&file_name);
            }
            if let Some(file) = dialog.save_file().await {
                checked_send_many(&sender, messages(file).await);
            }
        });
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "vscode")))]
    pub(crate) fn file_dialog_save<F, Fut>(
        &mut self,
        title: &'static str,
        filter: &'static FileFilter,
        default_file_name: Option<String>,
        messages: F,
    ) where
        F: FnOnce(FileHandle) -> Fut + 'static,
        Fut: Future<Output = Vec<Message>> + 'static,
    {
        let sender = self.channels.msg_sender.clone();

        perform_async_work(async move {
            let mut dialog = create_file_dialog(filter, title);
            if let Some(file_name) = default_file_name {
                dialog = dialog.set_file_name(&file_name);
            }
            if let Some(file) = dialog.save_file().await {
                checked_send_many(&sender, messages(file).await);
            }
        });
    }

    #[cfg(all(target_arch = "wasm32", feature = "vscode"))]
    pub(crate) fn open_file_dialog(&mut self, mode: OpenMode) {
        let load_options: LoadOptions = (mode, self.user.config.behavior.keep_during_reload).into();

        let kind = match load_options {
            LoadOptions::Clear => "waveform_clear",
            LoadOptions::KeepAvailable => "waveform_keep_available",
            LoadOptions::KeepAll => "waveform_keep_all",
        };
        vscode_open_dialog_with_filter(kind, &WAVEFORM_FILE_FILTER);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_file_dialog(&mut self, mode: OpenMode) {
        let load_options: LoadOptions = (mode, self.user.config.behavior.keep_during_reload).into();

        let message = move |file: Utf8PathBuf| vec![Message::LoadFile(file, load_options)];

        self.file_dialog_open("Open waveform file", &WAVEFORM_FILE_FILTER, message);
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "vscode")))]
    pub(crate) fn open_file_dialog(&mut self, mode: OpenMode) {
        let load_options: LoadOptions = (mode, self.user.config.behavior.keep_during_reload).into();

        let message = move |file: Vec<u8>| vec![Message::LoadFromData(file, load_options)];

        self.file_dialog_open("Open waveform file", &WAVEFORM_FILE_FILTER, message);
    }

    #[cfg(all(target_arch = "wasm32", feature = "vscode"))]
    pub(crate) fn open_command_file_dialog(&mut self) {
        vscode_open_dialog_with_filter("command_file", &COMMAND_FILE_FILTER);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_command_file_dialog(&mut self) {
        let message = move |file: Utf8PathBuf| vec![Message::LoadCommandFile(file)];

        self.file_dialog_open("Open command file", &COMMAND_FILE_FILTER, message);
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "vscode")))]
    pub(crate) fn open_command_file_dialog(&mut self) {
        self.file_dialog_open(
            "Open command file",
            &COMMAND_FILE_FILTER,
            |file: Vec<u8>| vec![Message::LoadCommandFromData(file)],
        );
    }

    #[cfg(feature = "python")]
    pub(crate) fn open_python_file_dialog(&mut self) {
        self.file_dialog_open("Open Python translator file", &PYTHON_FILE_FILTER, |file| {
            vec![Message::LoadPythonTranslator(file)]
        });
    }
}

#[cfg(not(all(target_arch = "wasm32", feature = "vscode")))]
#[cfg(not(target_os = "macos"))]
fn create_file_dialog(filter: &'static FileFilter, title: &'static str) -> AsyncFileDialog {
    AsyncFileDialog::new()
        .set_title(title)
        .add_filter(filter.name, filter.extensions)
        .add_filter("All files", &["*"])
}

#[cfg(not(all(target_arch = "wasm32", feature = "vscode")))]
#[cfg(target_os = "macos")]
fn create_file_dialog(filter: &'static FileFilter, title: &'static str) -> AsyncFileDialog {
    AsyncFileDialog::new()
        .set_title(title)
        .add_filter(filter.name, &filter.extensions)
}

/// Serialise a `(name, extensions)` filter pair into the JSON array expected by
/// `vscode_show_open_dialog`.
///
/// Example output: `[{"name":"Waveform files","extensions":["vcd","fst"]}]`
#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
pub(crate) fn vscode_open_dialog_with_filter(kind: &str, filter: &'static FileFilter) {
    let filters_json = filters_to_json(filter);
    vscode_show_open_dialog(kind, &filters_json);
}

#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
fn filters_to_json(filter: &'static FileFilter) -> String {
    let exts = filter
        .extensions
        .iter()
        .map(|e| format!("{e:?}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{{\"name\":{:?},\"extensions\":[{exts}]}}]", filter.name)
}
