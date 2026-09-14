//! Run decoders on a waveform without opening a window.
//!
//! The GUI requests decoder caches while drawing; this module drives the
//! message loop directly instead, so the same decoders and SUCL commands can
//! be used from the command line or a test.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use eyre::{Result, bail};

use crate::{
    StartupParams, SystemState,
    decoders::{DecodedData, ValueFormat, cache_key, resolve_inputs},
    displayed_item::DisplayedItem,
    message::Message,
    wave_source::WaveSource,
};

/// The decoded output of one decoder item.
#[derive(Debug, Clone)]
pub struct DecodedDecoder {
    pub decoder: String,
    pub display_name: String,
    pub data: Arc<DecodedData>,
    pub value_format: ValueFormat,
}

/// Load `wave`, apply the SUCL `commands`, and decode every decoder item they
/// add.
///
/// Returns an error if the waveform cannot be loaded, no decoder item is
/// added, a decoder input cannot be resolved, or a decoder fails. `timeout`
/// bounds the total time spent loading and decoding.
pub fn decode_file(
    wave: WaveSource,
    commands: Vec<String>,
    timeout: Duration,
) -> Result<Vec<DecodedDecoder>> {
    if let Some(path) = wave.as_file()
        && !path.exists()
    {
        bail!("waveform file does not exist: {path}");
    }

    let mut state = SystemState::new()?.with_params(StartupParams {
        waves: Some(wave),
        startup_commands: commands,
        ..Default::default()
    });

    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| eyre::eyre!("timeout is too large"))?;
    let mut messages = Vec::new();
    let mut caches_requested = false;

    loop {
        state.push_async_messages(&mut messages);
        while let Some(message) = messages.pop() {
            state.update(message);
        }
        state.handle_batch_commands();

        if !caches_requested && state.waves_fully_loaded() && state.batch_messages_completed {
            for (display_id, cache_key) in decoder_cache_keys(&state)? {
                state.update(Message::BuildDecoderCache {
                    display_id,
                    cache_key,
                });
            }
            caches_requested = true;
        }

        if caches_requested && decoder_caches_ready(&state) {
            break;
        }
        if Instant::now() > deadline {
            if caches_requested {
                bail!(
                    "timed out waiting for decoders: {}",
                    pending_decoders(&state).join(", ")
                );
            }
            if state.progress_tracker.is_some() {
                bail!("timed out loading the waveform; the load may have failed (see the log)");
            }
            bail!("timed out loading waveform and applying commands");
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    let waves = state.user.waves.as_ref().expect("waves were loaded");
    let mut outputs = Vec::new();
    for item in waves.displayed_items.values() {
        let DisplayedItem::Decoder(decoder) = item else {
            continue;
        };
        let Some(entry) = decoder.cache.as_ref() else {
            bail!("no cache was built for decoder '{}'", decoder.display_name);
        };
        match entry.get() {
            Some(Ok(data)) => outputs.push(DecodedDecoder {
                decoder: decoder.decoder.clone(),
                display_name: decoder.display_name.clone(),
                data: data.clone(),
                value_format: decoder.value_format,
            }),
            Some(Err(error)) => {
                bail!("decoder '{}' failed: {error}", decoder.display_name);
            }
            None => bail!("decoder '{}' is still building", decoder.display_name),
        }
    }
    Ok(outputs)
}

/// Cache keys for every decoder item in the loaded waveform.
fn decoder_cache_keys(
    state: &SystemState,
) -> Result<
    Vec<(
        crate::displayed_item::DisplayedItemRef,
        crate::decoders::DecoderCacheKey,
    )>,
> {
    let Some(waves) = state.user.waves.as_ref() else {
        return Ok(Vec::new());
    };
    let Some(container) = waves.inner.as_waves() else {
        bail!("decoders require a waveform source that supports them");
    };

    let mut keys = Vec::new();
    for (id, item) in &waves.displayed_items {
        let DisplayedItem::Decoder(decoder) = item else {
            continue;
        };
        resolve_inputs(container, &decoder.inputs).map_err(|error| {
            eyre::eyre!(
                "cannot resolve inputs of '{}': {error}",
                decoder.display_name
            )
        })?;
        keys.push((
            *id,
            cache_key(container, &decoder.inputs, &decoder.settings)?,
        ));
    }
    if keys.is_empty() {
        bail!("no decoder items were added; pass e.g. -C 'decoder_add spdif <signal>'");
    }
    Ok(keys)
}

/// True when every decoder item has a requested cache that has finished
/// building. Unlike [`SystemState::decoder_caches_ready`], a missing cache
/// does not count as ready.
fn decoder_caches_ready(state: &SystemState) -> bool {
    state.user.waves.as_ref().is_none_or(|waves| {
        waves.displayed_items.values().all(|item| match item {
            DisplayedItem::Decoder(decoder) => {
                decoder.cache.as_ref().is_some_and(|entry| entry.is_ready())
            }
            _ => true,
        })
    })
}

/// Names of decoder items whose cache has not finished building.
fn pending_decoders(state: &SystemState) -> Vec<String> {
    let Some(waves) = state.user.waves.as_ref() else {
        return Vec::new();
    };
    waves
        .displayed_items
        .values()
        .filter_map(|item| match item {
            DisplayedItem::Decoder(decoder)
                if !decoder.cache.as_ref().is_some_and(|entry| entry.is_ready()) =>
            {
                Some(decoder.display_name.clone())
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_file(name: &str) -> WaveSource {
        let path = project_root::get_project_root()
            .expect("project root")
            .join("examples")
            .join(name);
        WaveSource::File(path.try_into().expect("utf-8 path"))
    }

    fn install_runtime() -> tokio::runtime::EnterGuard<'static> {
        let runtime: &'static tokio::runtime::Runtime = Box::leak(Box::new(
            tokio::runtime::Builder::new_current_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap(),
        ));
        std::thread::spawn(move || {
            runtime.block_on(async {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
                }
            });
        });
        runtime.enter()
    }

    #[test]
    fn decodes_example_file_without_a_gui() {
        let _guard = install_runtime();

        let outputs = decode_file(
            example_file("spdif_2ch_24bit.vcd"),
            vec!["decoder_add spdif tb.data".to_string()],
            Duration::from_secs(30),
        )
        .expect("headless decode");

        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].decoder, "spdif");
        assert_eq!(outputs[0].data.rows.len(), 5);
        assert_eq!(outputs[0].data.rows[0].items.len(), 192);
        assert_eq!(outputs[0].data.rows[1].items.len(), 192);
        assert_eq!(
            outputs[0].data.rows[2].items[0].value,
            crate::decoders::DecodedValue::text("consumer 48 kHz 24-bit")
        );
    }

    #[test]
    fn decodes_multiple_decoders() {
        let _guard = install_runtime();

        let outputs = decode_file(
            example_file("spdif_2ch_24bit.vcd"),
            vec![
                "decoder_add spdif tb.data".to_string(),
                "decoder_add spdif tb.data word_bits=16".to_string(),
            ],
            Duration::from_secs(30),
        )
        .expect("headless decode");

        assert_eq!(outputs.len(), 2);
        assert!(
            outputs
                .iter()
                .all(|output| output.decoder == "spdif" && output.data.rows.len() == 5)
        );
    }

    #[test]
    fn reports_missing_waveform_file() {
        let error = decode_file(
            WaveSource::File("/definitely/not/a/waveform.vcd".into()),
            vec!["decoder_add spdif tb.data".to_string()],
            Duration::from_secs(1),
        )
        .expect_err("missing file should be rejected");

        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    #[test]
    fn rejects_unrepresentable_timeout() {
        let error = decode_file(
            example_file("spdif_2ch_24bit.vcd"),
            vec!["decoder_add spdif tb.data".to_string()],
            Duration::from_secs(u64::MAX),
        )
        .expect_err("oversized timeout should be rejected");

        assert!(error.to_string().contains("timeout"), "{error}");
    }
}
