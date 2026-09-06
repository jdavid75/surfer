use camino::Utf8PathBuf;
use surfer_gtkwave_parser::{Directive, Error, Parser};
use tracing::{error, warn};

use crate::{
    SystemState,
    async_util::perform_async_work,
    channels::checked_send_many,
    file_dialog::GTKWAVE_FILE_FILTER,
    message::Message,
    wave_container::{VariableRef, VariableRefExt},
    wave_source::LoadOptions,
};

fn directive_to_messages(directive: Directive) -> Vec<Message> {
    match directive {
        Directive::Dumpfile(path) => {
            vec![Message::LoadFile(
                Utf8PathBuf::from(path),
                LoadOptions::Clear,
            )]
        }
        Directive::Markers => vec![],
        Directive::Comment { text, flags } => vec![],
        Directive::Trace {
            path: (word, _bits), // TODO: bits
            color,
            flags,
        } => {
            vec![Message::AddVariables(vec![
                VariableRef::from_hierarchy_string(&word),
            ])]
        }
        Directive::TraceMany {
            path,
            rest,
            color,
            flags,
        } => vec![],
        Directive::OpenGroup { name, flags } => vec![],
        Directive::CloseGroup { name, flags } => vec![],
    }
}

fn log_error(error: Error) {
    match error {
        Error::Eof => warn!("GTKWave unexpected EOF"),
        Error::UnknownLine(line) => {
            warn!("GTKWave unknown line '{line}'");
        }
        Error::UnknownDirective(directive) => {
            warn!("GTKWave unknown directive: '{directive}'")
        }
        Error::Other(err) => error!("GTKWave: {err}"),
    }
}

impl SystemState {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn load_gtk_wave_dump(&mut self, path: Option<Utf8PathBuf>) {
        let messages = |path: Utf8PathBuf| async move {
            use tracing::info;

            let Ok(file_contents) = std::fs::read_to_string(&path) else {
                return vec![Message::Error(eyre::eyre!("Failed to read {path}"))];
            };
            let (directives, errors) = Parser::new(&file_contents).parse();
            info!("GTKWave directives: {:#?}", directives);
            for error in errors {
                log_error(error);
            }
            let messages = directives
                .into_iter()
                .flat_map(directive_to_messages)
                .collect();
            info!("GTKWave messages: {:#?}", messages);
            vec![Message::Batch(messages)]
        };

        if let Some(path) = path {
            let sender = self.channels.msg_sender.clone();
            perform_async_work(async move { checked_send_many(&sender, messages(path).await) });
        } else {
            self.file_dialog_open_async("Load GTKWave dumpfile", &GTKWAVE_FILE_FILTER, messages);
        }
    }

    // TODO: #[cfg(target_arch = "wasm32")]
    // Need to load two files, first the gtk wave dump, and then the mentioned trace file
}
