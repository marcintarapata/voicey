use crate::audio_feedback::{play_feedback_sound, play_feedback_sound_blocking, SoundType};
use crate::audio_toolkit::VadPolicy;
use crate::managers::audio::{recording_notice_code, AudioRecordingManager};
use crate::managers::history::HistoryManager;
use crate::managers::model::{ModelManager, SCREEN_VLM_ID};
use crate::managers::transcription::policy::{live_decode, LiveDecode, ModelTraits};
use crate::managers::transcription::StreamWorkKind;
use crate::managers::transcription::{Transcription, TranscriptionManager};
use crate::notices::{self, Notice, NoticeCode, NoticeSource};
use crate::screen_capture::{self, ScreenFrame};
use crate::settings::{get_settings, OverlayStyle};
use crate::shortcut;
use crate::tray::{set_tray_state, TrayIconState};
use crate::utils::{
    self, show_grounding_overlay, show_recording_overlay, show_transcribing_overlay,
};
use crate::vlm_supervisor::{VlmLoadSpec, VlmSupervisor};
use crate::TranscriptionCoordinator;
use log::{debug, error, warn};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

/// How a dictation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptionOutcome {
    /// The text was typed.
    Typed,
    /// Nothing was heard, so nothing was typed.
    Empty,
    /// The microphone sent only digital silence (e.g. muted).
    Silent,
    /// Transcribing or pasting failed.
    Failed,
}

/// Emitted when a dictation finishes (not on cancel); onboarding uses it to measure speed.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct TranscriptionCompleteEvent {
    pub outcome: TranscriptionOutcome,
    pub audio_seconds: f64,
    pub wait_seconds: f64,
}

fn emit_transcription_complete(
    app: &AppHandle,
    outcome: TranscriptionOutcome,
    sample_count: usize,
    stop_time: Instant,
) {
    let event = TranscriptionCompleteEvent {
        outcome,
        audio_seconds: sample_count as f64
            / crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE as f64,
        wait_seconds: stop_time.elapsed().as_secs_f64(),
    };
    if let Err(e) = event.emit(app) {
        warn!("Failed to emit transcription complete: {e}");
    }
}

/// Nothing to type: `Silent` if the microphone sent only zeros, else `Empty`.
fn empty_outcome(input_silent: bool) -> TranscriptionOutcome {
    if input_silent {
        TranscriptionOutcome::Silent
    } else {
        TranscriptionOutcome::Empty
    }
}

/// Ends a dictation that has nothing to type. A microphone that sent only
/// digital silence (muted, or the wrong default device) gets a notice, so it
/// doesn't look like the model missed the speech (#1899).
fn finish_without_text(
    app: &AppHandle,
    input_silent: bool,
    sample_count: usize,
    stop_time: Instant,
) {
    if input_silent {
        warn!("The microphone sent only digital silence; nothing was transcribed");
        notices::raise(
            app,
            Notice::new(NoticeCode::MicrophoneSilent, NoticeSource::Dictation),
        );
    } else {
        utils::hide_recording_overlay(app);
    }
    set_tray_state(app, TrayIconState::Idle);
    emit_transcription_complete(app, empty_outcome(input_silent), sample_count, stop_time);
}

const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Bumped per recording session, so a late paste from an earlier session
/// doesn't reset the tray or hide the overlay of a newer one.
static RECORDING_SESSION: AtomicU64 = AtomicU64::new(0);

/// Drop guard that finishes the transcription pipeline, including immediate
/// model unloading on early exits.
struct FinishGuard(AppHandle, Arc<TranscriptionManager>);
impl Drop for FinishGuard {
    fn drop(&mut self) {
        self.1.maybe_unload_immediately("transcription session");
        if let Some(c) = self.0.try_state::<TranscriptionCoordinator>() {
            c.notify_processing_finished();
        }
        // The pipeline just freed its large transient buffers (captured PCM,
        // WAV copy, engine scratch); hand the cached pages back to the OS so
        // they don't sit in malloc arenas until they get swapped out (#1792).
        crate::memory::trim_freed_memory();
    }
}

// Shortcut Action Trait
pub trait ShortcutAction: Send + Sync {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
    fn stop(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
}

// Transcribe Action
struct TranscribeAction {
    /// The grounded binding: also capture one frame of the screen at stop and
    /// resolve what the transcript points at through the local vision model
    /// (D6). The verbatim binding stays zero-latency and untouched.
    grounded: bool,
}

/// Returns `true` when a transcription has no meaningful content to ground
/// (empty or whitespace-only): nothing was said, so there is nothing for the
/// screen pass to resolve and the dictation ends as a verbatim one would.
fn is_blank_transcription(transcription: &str) -> bool {
    transcription.trim().is_empty()
}

async fn complete_unless_cancelled<F, C>(operation: F, is_cancelled: C) -> Option<F::Output>
where
    F: Future,
    C: Fn() -> bool,
{
    tokio::pin!(operation);

    loop {
        if is_cancelled() {
            return None;
        }

        if let Ok(result) =
            tokio::time::timeout(CANCELLATION_POLL_INTERVAL, operation.as_mut()).await
        {
            return Some(result);
        }
    }
}

fn should_use_streaming_overlay(style: OverlayStyle, is_streaming: bool) -> bool {
    style == OverlayStyle::Live && is_streaming
}

/// A reply that's blank once trimmed is no better than none: the verbatim
/// transcript goes out instead.
fn non_blank_grounded_text(text: String) -> Option<String> {
    if text.trim().is_empty() {
        warn!("Grounding returned an empty response; pasting the original transcript");
        None
    } else {
        Some(text)
    }
}

/// What became of the grounded pass for one dictation.
#[derive(Debug, PartialEq, Eq)]
enum GroundingOutcome {
    /// The pass ran and produced a rewrite.
    Done(String),
    /// The pass was asked for but couldn't run or produced nothing, so the
    /// verbatim transcript goes out and a warning notice explains.
    Failed,
}

/// Resolve a transcript's on-screen references against the frame with the
/// local vision model (D6). In memory only; the frame is dropped by the time
/// this returns and nothing about it is ever stored.
///
/// `None` only when cancelled. Runs the model files' download (rare: first
/// run fetches them up front, so this only fires when files were deleted),
/// the model load and the grounding itself, each cancellable like the rest
/// of the pipeline. The blocking supervisor calls run on the blocking pool
/// so they never stall the async runtime.
async fn ground_transcription(
    app: &AppHandle,
    frame: &ScreenFrame,
    transcription: &str,
    is_cancelled: impl Fn() -> bool + Send + Sync + 'static,
) -> Option<GroundingOutcome> {
    if is_blank_transcription(transcription) {
        // Nothing was said: behave exactly like the verbatim binding.
        return Some(GroundingOutcome::Done(transcription.to_string()));
    }

    // The pinned files, downloading them if they are missing (a deleted
    // file, an interrupted first-run download). The overlay keeps its
    // working state; the settings window, if open, tracks the same
    // `model-download-*` events under `SCREEN_VLM_ID`.
    let (model_path, mmproj_path) = {
        let model_manager = app.state::<Arc<ModelManager>>();
        match model_manager.screen_vlm_paths() {
            Some(paths) => paths,
            None => {
                debug!("The screen vision model is missing; downloading it");
                let manager = model_manager.inner().clone();
                match complete_unless_cancelled(
                    async move { manager.download_screen_vlm().await },
                    &is_cancelled,
                )
                .await
                {
                    None => return None,
                    Some(Err(e)) => {
                        error!("Could not download '{}': {:#}", SCREEN_VLM_ID, e);
                        return Some(GroundingOutcome::Failed);
                    }
                    Some(Ok(())) => {}
                }
                match app.state::<Arc<ModelManager>>().screen_vlm_paths() {
                    Some(paths) => paths,
                    None => {
                        error!(
                            "'{}' finished downloading but its files are missing",
                            SCREEN_VLM_ID
                        );
                        return Some(GroundingOutcome::Failed);
                    }
                }
            }
        }
    };

    // The load and the grounding block on the worker's answer; the
    // supervisor's own deadlines bound both.
    let vlm = app.state::<VlmSupervisor>().inner().clone();
    let mark = vlm.cancel_mark();
    let spec = VlmLoadSpec {
        model_path,
        mmproj_path,
        gpu: true,
    };
    let transcript = transcription.to_string();
    let frame = frame.clone();
    let ground = tokio::task::spawn_blocking(move || {
        vlm.load(spec)?;
        vlm.ground(mark, &frame, transcript)
    });
    let grounded = match complete_unless_cancelled(ground, &is_cancelled).await {
        None => return None,
        Some(Ok(Ok(text))) => text,
        Some(Ok(Err(e))) => {
            error!("Screen grounding failed: {e}");
            return Some(GroundingOutcome::Failed);
        }
        Some(Err(e)) => {
            error!("Screen grounding task failed: {e}");
            return Some(GroundingOutcome::Failed);
        }
    };
    Some(non_blank_grounded_text(grounded).map_or(GroundingOutcome::Failed, GroundingOutcome::Done))
}

impl ShortcutAction for TranscribeAction {
    fn start(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        let start_time = Instant::now();
        debug!("TranscribeAction::start called for binding: {}", binding_id);

        // Load model in the background
        let tm = app.state::<Arc<TranscriptionManager>>();
        let rm = app.state::<Arc<AudioRecordingManager>>();

        // Load ASR model and VAD model in parallel
        let kickoff_started = Instant::now();
        // An empty selection has nothing to load; the check below says so.
        let selected_model = get_settings(app).selected_model;
        let load_started = !selected_model.is_empty() && tm.initiate_model_load(true);
        let rm_clone = Arc::clone(&rm);
        std::thread::spawn(move || {
            if let Err(e) = rm_clone.preload_vad() {
                debug!("VAD pre-load failed: {}", e);
            }
        });
        let kickoff_elapsed = kickoff_started.elapsed();

        // Don't open the mic if nothing can transcribe the recording. A load
        // started above reports its own failure; one already running doesn't.
        if !tm.is_model_loaded() {
            if selected_model.is_empty() {
                warn!("Not starting recording: no model is selected");
                tm.raise_no_model_selected();
                return;
            }
            if let Err(e) = app
                .state::<Arc<ModelManager>>()
                .get_model_path(&selected_model)
            {
                warn!("Not starting recording: no model can transcribe it ({})", e);
                if !load_started {
                    tm.raise_model_load_failed(&selected_model, &e);
                }
                return;
            }
        }

        let binding_id = binding_id.to_string();
        RECORDING_SESSION.fetch_add(1, Ordering::SeqCst);
        let tray_started = Instant::now();
        set_tray_state(app, TrayIconState::Recording);
        let tray_elapsed = tray_started.elapsed();

        // Get the microphone mode to determine audio feedback timing
        let plan_started = Instant::now();
        let settings = get_settings(app);
        let is_always_on = settings.always_on_microphone;

        let selected_model_info = app
            .state::<Arc<ModelManager>>()
            .get_model_info(&settings.selected_model);

        // Use the app-facing model capability as the single pre-recording source
        // for live streaming decisions. Unknown support is represented as false
        // until the model registry is updated by discovery or runtime load.
        let model_supports_streaming = selected_model_info
            .as_ref()
            .map(|m| m.supports_streaming)
            .unwrap_or(false);
        let vad_policy = if !settings.vad_enabled {
            VadPolicy::Disabled
        } else if model_supports_streaming {
            VadPolicy::Streaming
        } else {
            VadPolicy::Offline
        };
        // Before the model loads, whether it keeps its accuracy on long audio
        // is unknown, so assume not: the worker decides again with the loaded
        // model, and a wrong guess costs only an idle worker.
        let expected_model = ModelTraits {
            streaming: model_supports_streaming,
            long_form: false,
        };
        if live_decode(expected_model, settings.long_form_chunking) != LiveDecode::None {
            tm.start_stream();
        }
        let plan_elapsed = plan_started.elapsed();

        // Sizing the overlay follows the same advertised capability. A model that
        // doesn't stream (or whose capability is not known yet) gets the compact
        // pill instead of an oversized transparent live window.
        let overlay_started = Instant::now();
        match settings.overlay_style {
            OverlayStyle::Live if model_supports_streaming => utils::show_streaming_overlay(app),
            OverlayStyle::Live | OverlayStyle::Minimal => show_recording_overlay(app),
            OverlayStyle::None => {} // show_overlay_state no-ops on None anyway
        }
        // Everything above runs before capture can begin, so each span here is
        // added keypress->capture latency.
        debug!(
            "start-path pre-recording steps: model_kickoff={:?} tray={:?} settings+stream_plan={:?} overlay={:?}",
            kickoff_elapsed,
            tray_elapsed,
            plan_elapsed,
            overlay_started.elapsed()
        );
        debug!("Microphone mode - always_on: {}", is_always_on);

        let mut recording_error: Option<anyhow::Error> = None;
        let recording_start_time = Instant::now();
        match rm.try_start_recording(&binding_id, vad_policy) {
            Ok(readiness) => {
                debug!(
                    "Recording request accepted in {:?}; waiting for first microphone samples",
                    recording_start_time.elapsed()
                );
                let generation = readiness.generation();
                let app_clone = app.clone();
                let rm_clone = Arc::clone(&rm);
                std::thread::spawn(move || {
                    if !readiness.wait() {
                        debug!("Microphone readiness wait ended without receiving samples");
                        return;
                    }

                    // Development-only preview hook for evaluating the brief
                    // arming animation on hardware that normally starts too fast
                    // to make it visible.
                    #[cfg(debug_assertions)]
                    if let Ok(delay_ms) = std::env::var("HANDY_DEBUG_MIC_READY_DELAY_MS")
                        .unwrap_or_default()
                        .parse::<u64>()
                    {
                        let delay_ms = delay_ms.min(10_000);
                        if delay_ms > 0 {
                            debug!("Delaying microphone-ready cue by {delay_ms}ms for UI preview");
                            std::thread::sleep(Duration::from_millis(delay_ms));
                        }
                    }

                    if !rm_clone.is_recording_readiness_current(generation) {
                        debug!("Microphone became ready for an inactive recording");
                        return;
                    }

                    debug!("Microphone is receiving samples; recording is ready");
                    utils::emit_recording_ready(&app_clone);

                    // The start chime is a readiness cue, so it must follow the
                    // first real input callback rather than Stream::play() or a
                    // fixed delay. The helper returns immediately when feedback
                    // is disabled; mute still follows the same readiness point.
                    if rm_clone.is_recording_readiness_current(generation) {
                        play_feedback_sound_blocking(&app_clone, SoundType::Start);
                    }
                    if rm_clone.is_recording_readiness_current(generation) {
                        rm_clone.apply_mute();
                    }
                });
            }
            Err(e) => {
                debug!("Failed to start recording: {}", e);
                recording_error = Some(e);
            }
        }

        if recording_error.is_none() {
            // Dynamically register the cancel shortcut in a separate task to avoid deadlock
            shortcut::register_cancel_shortcut(app);
        } else {
            // Starting failed (for example due to blocked microphone permissions).
            // Revert UI state so we don't stay stuck in the recording overlay.
            tm.cancel_stream();
            set_tray_state(app, TrayIconState::Idle);
            if let Some(err) = recording_error {
                notices::raise(
                    app,
                    Notice::new(recording_notice_code(&err), NoticeSource::Dictation)
                        .with_detail(format!("{err:#}")),
                );
            }
        }

        debug!(
            "TranscribeAction::start completed in {:?}",
            start_time.elapsed()
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        // Prevent a slow microphone from emitting a ready event or start chime
        // after the user has already requested stop.
        app.state::<Arc<AudioRecordingManager>>()
            .invalidate_recording_readiness();

        // Unregister the cancel shortcut when transcription stops
        shortcut::unregister_cancel_shortcut(app);

        let stop_time = Instant::now();
        debug!("TranscribeAction::stop called for binding: {}", binding_id);

        let ah = app.clone();
        let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
        let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
        let hm = Arc::clone(&app.state::<Arc<HistoryManager>>());

        set_tray_state(app, TrayIconState::Transcribing);
        // Stop should give immediate visual feedback. Live streaming can keep
        // the larger panel, but it still switches from listening to a working
        // spinner while the stream finalizes. Non-streaming paths use the
        // compact transcribing pill (None no-ops in show_*).
        let style = get_settings(app).overlay_style;
        // Capture this before finalizing the stream so every later working state
        // targets the same overlay that was shown for this transcription.
        let use_streaming_overlay = should_use_streaming_overlay(style, tm.is_streaming());
        if use_streaming_overlay {
            tm.emit_stream_working(StreamWorkKind::Transcribing);
        } else {
            show_transcribing_overlay(app);
        }

        // Unmute before playing audio feedback so the stop sound is audible
        rm.remove_mute();

        // Play audio feedback for recording stop
        play_feedback_sound(app, SoundType::Stop);

        let binding_id = binding_id.to_string(); // Clone binding_id for the async task
        let grounded = self.grounded;
        let cancel_generation = rm.cancel_generation();

        tauri::async_runtime::spawn(async move {
            let _guard = FinishGuard(ah.clone(), Arc::clone(&tm));
            debug!(
                "Starting async transcription task for binding: {}",
                binding_id
            );

            // D4: the grounded binding grabs exactly one frame of the screen
            // at release — the screen the words referred to — before anything
            // else happens. In memory only; a capture failure degrades the
            // dictation to verbatim with a notice, never blocks it.
            let frame = if grounded {
                match screen_capture::capture_primary_display() {
                    Ok(frame) => Some(frame),
                    Err(e) => {
                        warn!("Screen capture failed: {e}");
                        None
                    }
                }
            } else {
                None
            };

            let stop_recording_time = Instant::now();
            if let Some(samples) = rm.stop_recording(&binding_id, cancel_generation) {
                // Read now: by the time transcribing ends, another take may
                // have replaced it.
                let input_silent = rm.last_input_silent();
                debug!(
                    "Recording stopped and samples retrieved in {:?}, sample count: {}",
                    stop_recording_time.elapsed(),
                    samples.len()
                );

                if rm.was_cancelled_since(cancel_generation) {
                    debug!("Transcription operation cancelled after recording stop");
                    tm.cancel_stream();
                    utils::hide_recording_overlay(&ah);
                    set_tray_state(&ah, TrayIconState::Idle);
                    return;
                }

                if samples.is_empty() {
                    debug!("Recording produced no audio samples; skipping persistence");
                    // Tear down any streaming worker so its channel doesn't leak
                    // and block the next start_stream.
                    tm.cancel_stream();
                    finish_without_text(&ah, input_silent, 0, stop_time);
                } else {
                    // Voicey never writes dictation audio to disk: the
                    // samples stay in memory and only the text is kept.
                    let sample_count = samples.len();
                    let transcription_time = Instant::now();
                    let transcription_result = tm.transcribe_recording(samples);

                    if rm.was_cancelled_since(cancel_generation) {
                        debug!("Transcription operation cancelled before output handling");
                        utils::hide_recording_overlay(&ah);
                        set_tray_state(&ah, TrayIconState::Idle);
                        return;
                    }

                    match transcription_result {
                        Ok(Transcription {
                            text: transcription,
                            language,
                        }) => {
                            debug!(
                                "Transcription completed in {:?}: '{}'",
                                transcription_time.elapsed(),
                                utils::redact_text(&transcription)
                            );

                            // The grounded pass: one frame plus the transcript
                            // through the local vision model (D6). The verbatim
                            // binding skips straight past it.
                            let grounding = if grounded {
                                if use_streaming_overlay {
                                    tm.emit_stream_working(StreamWorkKind::Grounding);
                                } else {
                                    show_grounding_overlay(&ah);
                                }
                                let outcome = match frame.as_ref() {
                                    // Capture already failed at release; say
                                    // so now, once there is something to paste.
                                    None => GroundingOutcome::Failed,
                                    Some(frame) => {
                                        let rm_for_ground = Arc::clone(&rm);
                                        match ground_transcription(
                                            &ah,
                                            frame,
                                            &transcription,
                                            move || {
                                                rm_for_ground.was_cancelled_since(cancel_generation)
                                            },
                                        )
                                        .await
                                        {
                                            Some(outcome) => outcome,
                                            // Cancelled: the whole pipeline is
                                            // unwinding.
                                            None => {
                                                debug!(
                                                    "Transcription operation cancelled during grounding"
                                                );
                                                utils::hide_recording_overlay(&ah);
                                                set_tray_state(&ah, TrayIconState::Idle);
                                                return;
                                            }
                                        }
                                    }
                                };
                                Some(outcome)
                            } else {
                                None
                            };

                            if rm.was_cancelled_since(cancel_generation) {
                                debug!("Transcription operation cancelled before paste");
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
                                return;
                            }

                            // What the dictation produced: the grounded rewrite
                            // when the pass succeeded, the verbatim transcript
                            // otherwise. `grounded_text` is what the pass
                            // extracted (kept by history, D5: text, never
                            // pixels); `grounding_failed` drives the warning
                            // notice that replaces the overlay after the paste.
                            let (final_text, grounded_text, grounding_failed) = match &grounding {
                                Some(GroundingOutcome::Done(grounded)) => {
                                    (grounded.clone(), Some(grounded.clone()), false)
                                }
                                Some(GroundingOutcome::Failed) => {
                                    (transcription.clone(), None, true)
                                }
                                None => (transcription.clone(), None, false),
                            };

                            // Save the transcript to history. Voicey keeps no
                            // audio, so the entry has no recording to point at;
                            // the grounded text is kept alongside the verbatim
                            // one (D5: history keeps text, never pixels).
                            if let Err(err) = hm.save_entry(
                                String::new(),
                                transcription,
                                grounded,
                                grounded_text,
                                None,
                                language,
                            ) {
                                error!("Failed to save history entry: {}", err);
                            }

                            // The user's hook only changes what gets pasted;
                            // history above keeps the text as produced.
                            let Some(final_text) = complete_unless_cancelled(
                                crate::transcription_hook::apply(&ah, final_text),
                                || rm.was_cancelled_since(cancel_generation),
                            )
                            .await
                            else {
                                debug!("Transcription operation cancelled during hook");
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
                                return;
                            };

                            if final_text.is_empty() {
                                finish_without_text(&ah, input_silent, sample_count, stop_time);
                            } else {
                                let ah_clone = ah.clone();
                                let paste_time = Instant::now();
                                let rm_for_paste = Arc::clone(&rm);
                                // Taken before the paste thread exists, and so
                                // before the next session can start.
                                let session = RECORDING_SESSION.load(Ordering::SeqCst);
                                // Off the main thread: paste sleeps between steps, and a
                                // paste into Handy's own windows needs the main thread free.
                                std::thread::Builder::new()
                                    .name("paste".into())
                                    .spawn(move || {
                                        if rm_for_paste.was_cancelled_since(cancel_generation) {
                                            debug!(
                                                "Transcription operation cancelled before paste"
                                            );
                                            utils::hide_recording_overlay(&ah_clone);
                                            set_tray_state(&ah_clone, TrayIconState::Idle);
                                            return;
                                        }

                                        // A new session may have taken over the
                                        // UI while this paste ran.
                                        let owns_ui =
                                            || RECORDING_SESSION.load(Ordering::SeqCst) == session;
                                        let (outcome, notice) =
                                            match utils::paste(final_text, ah_clone.clone()) {
                                                Ok(utils::PasteOutcome::Pasted) => {
                                                    debug!(
                                                        "Text pasted successfully in {:?}",
                                                        paste_time.elapsed()
                                                    );
                                                    // Leave a newer session's notice alone.
                                                    if owns_ui() {
                                                        notices::clear(
                                                            &ah_clone,
                                                            NoticeSource::Dictation,
                                                        );
                                                    }
                                                    let notice = grounding_failed.then(|| {
                                                        Notice::new(
                                                            NoticeCode::GroundingFailed,
                                                            NoticeSource::Dictation,
                                                        )
                                                    });
                                                    (TranscriptionOutcome::Typed, notice)
                                                }
                                                Ok(utils::PasteOutcome::Blocked) => (
                                                    TranscriptionOutcome::Failed,
                                                    Some(Notice::new(
                                                        NoticeCode::PasteBlockedElevated,
                                                        NoticeSource::Dictation,
                                                    )),
                                                ),
                                                Err(e) => {
                                                    error!("Failed to paste transcription: {}", e);
                                                    (
                                                        TranscriptionOutcome::Failed,
                                                        Some(Notice::new(
                                                            NoticeCode::PasteFailed,
                                                            NoticeSource::Dictation,
                                                        )),
                                                    )
                                                }
                                            };
                                        match (owns_ui(), notice) {
                                            (true, Some(notice)) => {
                                                notices::raise(&ah_clone, notice)
                                            }
                                            (true, None) => {
                                                utils::hide_recording_overlay(&ah_clone)
                                            }
                                            (false, Some(notice)) => {
                                                notices::raise_without_overlay(&ah_clone, notice)
                                            }
                                            (false, None) => {}
                                        }
                                        if owns_ui() {
                                            set_tray_state(&ah_clone, TrayIconState::Idle);
                                        }
                                        emit_transcription_complete(
                                            &ah_clone,
                                            outcome,
                                            sample_count,
                                            stop_time,
                                        );
                                    })
                                    .map(|_| ())
                                    .unwrap_or_else(|e| {
                                        error!("Failed to start the paste thread: {:?}", e);
                                        utils::hide_recording_overlay(&ah);
                                        set_tray_state(&ah, TrayIconState::Idle);
                                        emit_transcription_complete(
                                            &ah,
                                            TranscriptionOutcome::Failed,
                                            sample_count,
                                            stop_time,
                                        );
                                    });
                            }
                        }
                        Err(err) => {
                            if rm.was_cancelled_since(cancel_generation) {
                                debug!(
                                    "Transcription operation cancelled after transcription error"
                                );
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
                                return;
                            }

                            error!("Transcription failed: {}", err);
                            notices::raise(
                                &ah,
                                Notice::new(
                                    NoticeCode::TranscriptionFailed,
                                    NoticeSource::Dictation,
                                )
                                .with_detail(err.to_string()),
                            );
                            // Save entry with empty text so user can retry
                            // (text-only; Voicey keeps no dictation audio).
                            if let Err(save_err) = hm.save_entry(
                                String::new(),
                                String::new(),
                                grounded,
                                None,
                                None,
                                None,
                            ) {
                                error!("Failed to save failed history entry: {}", save_err);
                            }
                            set_tray_state(&ah, TrayIconState::Idle);
                            emit_transcription_complete(
                                &ah,
                                TranscriptionOutcome::Failed,
                                sample_count,
                                stop_time,
                            );
                        }
                    }
                }
            } else {
                debug!("No samples retrieved from recording stop");
                // Tear down any streaming worker so its channel doesn't leak.
                tm.cancel_stream();
                utils::hide_recording_overlay(&ah);
                set_tray_state(&ah, TrayIconState::Idle);
                if !rm.was_cancelled_since(cancel_generation) {
                    emit_transcription_complete(&ah, TranscriptionOutcome::Empty, 0, stop_time);
                }
            }
        });

        debug!(
            "TranscribeAction::stop completed in {:?}",
            stop_time.elapsed()
        );
    }
}

// Cancel Action
struct CancelAction;

impl ShortcutAction for CancelAction {
    fn start(&self, app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        utils::cancel_current_operation(app);
    }

    fn stop(&self, _app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        // Nothing to do on stop for cancel
    }
}

// Test Action
struct TestAction;

impl ShortcutAction for TestAction {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str) {
        log::info!(
            "Shortcut ID '{}': Started - {} (App: {})", // Changed "Pressed" to "Started" for consistency
            binding_id,
            shortcut_str,
            app.package_info().name
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str) {
        log::info!(
            "Shortcut ID '{}': Stopped - {} (App: {})", // Changed "Released" to "Stopped" for consistency
            binding_id,
            shortcut_str,
            app.package_info().name
        );
    }
}

// Static Action Map
pub static ACTION_MAP: Lazy<HashMap<String, Arc<dyn ShortcutAction>>> = Lazy::new(|| {
    let mut map = HashMap::new();
    map.insert(
        "transcribe".to_string(),
        Arc::new(TranscribeAction { grounded: false }) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "transcribe_grounded".to_string(),
        Arc::new(TranscribeAction { grounded: true }) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "cancel".to_string(),
        Arc::new(CancelAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "test".to_string(),
        Arc::new(TestAction) as Arc<dyn ShortcutAction>,
    );
    map
});

#[cfg(test)]
mod tests {
    use super::{
        complete_unless_cancelled, is_blank_transcription, non_blank_grounded_text,
        should_use_streaming_overlay,
    };
    use crate::settings::OverlayStyle;
    use std::future;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn blank_transcription_is_detected() {
        assert!(is_blank_transcription(""));
        assert!(is_blank_transcription("   "));
        assert!(is_blank_transcription("\t\n  \r\n"));
    }

    #[test]
    fn non_blank_transcription_is_kept() {
        assert!(!is_blank_transcription("hello"));
        assert!(!is_blank_transcription("  hello  "));
    }

    #[test]
    fn blank_grounded_text_is_rejected() {
        for text in ["", "   ", "\t\n\r\n", "\u{00A0}\u{2003}"] {
            assert_eq!(non_blank_grounded_text(text.to_string()), None);
        }
    }

    #[test]
    fn non_blank_grounded_text_is_preserved() {
        for text in ["Click Save.", "  정리된 문장입니다.\n", "0"] {
            assert_eq!(
                non_blank_grounded_text(text.to_string()),
                Some(text.to_string())
            );
        }
    }

    #[test]
    fn completed_operation_returns_its_output() {
        let result = tauri::async_runtime::block_on(complete_unless_cancelled(
            future::ready("done"),
            || false,
        ));

        assert_eq!(result, Some("done"));
    }

    #[test]
    fn pending_operation_stops_after_cancellation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_for_thread = Arc::clone(&cancelled);
        let cancel_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            cancelled_for_thread.store(true, Ordering::Release);
        });

        let result = tauri::async_runtime::block_on(complete_unless_cancelled(
            future::pending::<()>(),
            || cancelled.load(Ordering::Acquire),
        ));

        cancel_thread.join().unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn live_overlay_uses_streaming_states_only_for_streaming_models() {
        assert!(should_use_streaming_overlay(OverlayStyle::Live, true));
        assert!(!should_use_streaming_overlay(OverlayStyle::Live, false));
        assert!(!should_use_streaming_overlay(OverlayStyle::Minimal, true));
        assert!(!should_use_streaming_overlay(OverlayStyle::None, true));
    }
}
