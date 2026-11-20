//! Long-form chunked decoding, shared by a live recording and a batch clip:
//! both cut the same audio at the same pauses and decode it the same way.

use std::time::Instant;

use log::{info, warn};
use transcribe_cpp::Transcript;

use super::{emit_stream_text, RunSetup, TranscriptionManager};
use crate::audio_toolkit::chunking::{join_transcripts, Chunker};
use crate::engine_supervisor::{CancelMark, EngineError, LoadedInfo};
use crate::language_detection::Plan;
use crate::managers::audio::create_detector;
use crate::settings::{AppSettings, VadBackend};

const RATE: usize = 16_000;

/// How a chunked decode's runs are set up: known up front (a whole clip), or
/// once the first chunk's audio can pick the language (a live recording).
pub(super) enum ChunkSetup {
    Ready(Box<RunSetup>),
    Pending(Box<PendingSetup>),
}

/// When the live preview re-decodes the tail it is showing. The pinned
/// dictation model decodes offline (no engine streaming), so the preview is
/// refreshed by re-running it over the audio that is not yet committed to a
/// chunk; the pacer keeps that to a natural cadence instead of once per
/// microphone frame.
#[derive(Debug)]
pub(crate) struct PreviewPacer {
    /// New audio that must arrive between two preview re-decodes, in samples.
    interval_samples: usize,
    /// The shortest tail worth re-decoding: below it a run costs more than
    /// the text it could add. In samples.
    floor_samples: usize,
    /// Total samples at the last preview (or the recording's start).
    last_preview: usize,
}

impl PreviewPacer {
    /// `interval_ms` is [`AppSettings::live_preview_interval_ms`] (the
    /// config file's `[overlay] preview_interval_ms`).
    pub(crate) fn new(interval_ms: u64) -> Self {
        Self {
            interval_samples: interval_ms as usize * RATE / 1000,
            floor_samples: RATE,
            last_preview: 0,
        }
    }

    /// Whether the tail, of `tail_samples`, should be re-decoded now that the
    /// recording holds `samples` in total.
    pub(crate) fn is_due(&self, samples: usize, tail_samples: usize) -> bool {
        tail_samples >= self.floor_samples && samples - self.last_preview >= self.interval_samples
    }

    /// Records that a preview ran at `samples`; the next one is due an
    /// interval later.
    pub(crate) fn mark(&mut self, samples: usize) {
        self.last_preview = samples;
    }
}

/// What a chunked decode's setup is resolved from.
pub(super) struct PendingSetup {
    pub plan: Plan,
    pub info: LoadedInfo,
    pub settings: AppSettings,
}

impl ChunkSetup {
    /// The setup, resolving a pending one with `audio`.
    fn resolve(&mut self, tm: &TranscriptionManager, audio: &[f32]) -> &RunSetup {
        if let Self::Pending(pending) = self {
            let setup = tm.setup_for(&pending.info, &pending.settings, &pending.plan, audio);
            *self = Self::Ready(Box::new(setup));
        }
        match self {
            Self::Ready(setup) => setup,
            Self::Pending(_) => unreachable!("resolved above"),
        }
    }
}

/// Cuts audio at pauses as it arrives and decodes each chunk once its cut is
/// certain, so a recording has only its last chunk left when it stops.
pub(super) struct ChunkedDecode<'a> {
    tm: &'a TranscriptionManager,
    /// Every chunk's run stops with one cancel.
    mark: CancelMark,
    setup: ChunkSetup,
    model_id: String,
    chunker: Chunker,
    transcripts: Vec<Transcript>,
    /// The first run that failed or was cancelled; nothing more is decoded
    /// after it.
    failure: Option<EngineError>,
    /// Live recordings only: emit the decoded-so-far text to the overlay as
    /// each chunk lands, so the preview panel can show the transcript while
    /// the recording continues. Batch clip decodes stay quiet — there is no
    /// overlay to receive them.
    emit_live_text: bool,
    /// Live recordings only: when the not-yet-committed tail is due a rolling
    /// preview re-decode.
    pacer: PreviewPacer,
    /// A preview re-decode failed with something other than a cancel: no more
    /// are attempted (the real decode reports the failure if it persists).
    preview_broken: bool,
}

/// The decoded chunks' text joined the way the final result joins them, so
/// the live preview matches what the dictation will paste.
fn joined_text(transcripts: &[Transcript]) -> String {
    if transcripts.len() == 1 {
        return transcripts[0].text.clone();
    }
    join_transcripts(
        &transcripts
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>(),
    )
}

impl<'a> ChunkedDecode<'a> {
    pub(super) fn new(
        tm: &'a TranscriptionManager,
        mark: CancelMark,
        setup: ChunkSetup,
        model_id: &str,
        backend: VadBackend,
    ) -> Self {
        // A batch clip emits nothing live, so its pacer never fires.
        Self::new_with(
            tm,
            mark,
            setup,
            model_id,
            backend,
            false,
            PreviewPacer::new(0),
        )
    }

    /// A live-recording chunked decode: like [`Self::new`], but every decoded
    /// chunk's accumulated text is emitted to the overlay as it lands — and
    /// the growing tail is re-decoded on `preview_interval_ms` so the preview
    /// updates mid-speech even when no chunk is cut yet.
    pub(super) fn new_live(
        tm: &'a TranscriptionManager,
        mark: CancelMark,
        setup: ChunkSetup,
        model_id: &str,
        backend: VadBackend,
        preview_interval_ms: u64,
    ) -> Self {
        Self::new_with(
            tm,
            mark,
            setup,
            model_id,
            backend,
            true,
            PreviewPacer::new(preview_interval_ms),
        )
    }

    fn new_with(
        tm: &'a TranscriptionManager,
        mark: CancelMark,
        setup: ChunkSetup,
        model_id: &str,
        backend: VadBackend,
        emit_live_text: bool,
        pacer: PreviewPacer,
    ) -> Self {
        // The VAD is only loaded if the audio turns out long enough to cut.
        let app_handle = tm.app_handle.clone();
        Self {
            tm,
            mark,
            setup,
            model_id: model_id.to_string(),
            chunker: Chunker::new(Box::new(move || create_detector(backend, &app_handle))),
            transcripts: Vec::new(),
            failure: None,
            emit_live_text,
            pacer,
            preview_broken: false,
        }
    }

    /// A whole clip's chunks, decoded in order, and the setup they ran with.
    pub(super) fn decode_clip(
        tm: &'a TranscriptionManager,
        mark: CancelMark,
        setup: ChunkSetup,
        audio: Vec<f32>,
        model_id: &str,
        backend: VadBackend,
    ) -> Result<(Vec<Transcript>, RunSetup), EngineError> {
        let mut decode = Self::new(tm, mark, setup, model_id, backend);
        decode.push(audio);
        decode.finish()
    }

    /// Samples pushed so far.
    pub(super) fn samples(&self) -> usize {
        self.chunker.samples()
    }

    /// Chunks decoded so far.
    pub(super) fn decoded(&self) -> usize {
        self.transcripts.len()
    }

    /// Whether a failure already ended the decode; [`finish`](Self::finish)
    /// reports it.
    pub(super) fn failed(&self) -> bool {
        self.failure.is_some()
    }

    /// Adds audio, decoding each chunk it makes certain.
    pub(super) fn push(&mut self, pcm: Vec<f32>) {
        if self.failure.is_some() {
            return;
        }
        let chunks = self.chunker.push(pcm);
        self.decode(chunks);
        self.preview_tail();
    }

    /// Decodes what is left. Every chunk's transcript in order and the setup
    /// they ran with, or the run that failed or was cancelled.
    pub(super) fn finish(mut self) -> Result<(Vec<Transcript>, RunSetup), EngineError> {
        if self.failure.is_none() {
            let chunks = self.chunker.finish();
            self.decode(chunks);
        }
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        if self.transcripts.len() > 1 {
            info!(
                "Decoded {:.1}s of audio in {} chunks",
                self.samples() as f64 / 16_000.0,
                self.transcripts.len()
            );
        }
        // No chunk at all (no audio) still needs a setup to finish with.
        self.setup.resolve(self.tm, &[]);
        let ChunkSetup::Ready(setup) = self.setup else {
            unreachable!("resolved above")
        };
        Ok((self.transcripts, *setup))
    }

    fn decode(&mut self, chunks: Vec<Vec<f32>>) {
        for chunk in chunks {
            if self.failure.is_some() {
                return;
            }
            // The first chunk picks the language for every chunk.
            let run_options = self.setup.resolve(self.tm, &chunk).run_options.clone();
            let audio_secs = chunk.len() as f64 / 16_000.0;
            let started = Instant::now();
            match self
                .tm
                .run_batch(self.mark, chunk, &run_options, &self.model_id)
            {
                Ok(transcript) => {
                    self.transcripts.push(transcript);
                    // The live preview: everything decoded so far, committed.
                    // A chunk's text never changes after this, so this is
                    // exactly the `committed` half of a stream's preview.
                    if self.emit_live_text {
                        let text = joined_text(&self.transcripts);
                        emit_stream_text(&self.tm.app_handle, &text, "");
                    }
                    // Real-time factor: audio seconds decoded per second of
                    // wall clock. Floored so a near-instant decode reports a
                    // large factor instead of infinity.
                    let elapsed_secs = started.elapsed().as_secs_f64().max(1e-6);
                    info!(
                        "Decoded long-form chunk {} ({:.1}s of audio) in {:.2}s ({:.0}x real time)",
                        self.transcripts.len(),
                        audio_secs,
                        elapsed_secs,
                        audio_secs / elapsed_secs
                    );
                }
                Err(e) => {
                    info!("Chunk decoding stopped: {}", e);
                    self.failure = Some(e);
                }
            }
        }
    }

    /// The live preview while speech is still going: re-decode the audio that
    /// is not yet committed to a chunk and emit it as tentative text on top
    /// of the committed chunks. Best-effort — the pasted result comes only
    /// from the chunks (and the batch decode at stop), never from here.
    fn preview_tail(&mut self) {
        if !self.emit_live_text || self.preview_broken || self.failure.is_some() {
            return;
        }
        let samples = self.chunker.samples();
        let tail = self.chunker.tail();
        if !self.pacer.is_due(samples, tail.len()) {
            return;
        }
        // The same setup the eventual chunk decode runs with: the first
        // resolution (it may hear the language from this audio) sticks.
        let run_options = self.setup.resolve(self.tm, tail).run_options.clone();
        match self
            .tm
            .run_batch(self.mark, tail.to_vec(), &run_options, &self.model_id)
        {
            Ok(transcript) => {
                self.pacer.mark(self.chunker.samples());
                let committed = joined_text(&self.transcripts);
                emit_stream_text(&self.tm.app_handle, &committed, &transcript.text);
            }
            // The session is being torn down; the cancel path reports it.
            Err(EngineError::Cancelled) => {}
            Err(e) => {
                warn!("Live preview re-decode failed ({}); pausing previews", e);
                self.preview_broken = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PreviewPacer;

    const RATE: usize = 16_000;

    fn samples(secs: usize) -> usize {
        secs * RATE
    }

    /// The regression pin for the live preview: the pinned dictation model
    /// decodes offline, so with no chunk cut (nothing is cut under 45 s of
    /// audio — the whole tail stays pending) the preview must refresh by
    /// re-decoding the growing tail on the configured cadence. Before the
    /// rolling re-decode, 10 s of continuous speech produced no preview at
    /// all until the recording stopped.
    #[test]
    fn ongoing_speech_re_decodes_the_tail_on_the_configured_cadence() {
        let mut pacer = PreviewPacer::new(2000);
        let mut previews = Vec::new();
        // 10 s of ongoing speech arriving in 100 ms recorder frames, none
        // of it ever committed to a chunk: the tail is the whole recording.
        for frame in 1..=100 {
            let at = frame * RATE / 10;
            if pacer.is_due(at, at) {
                previews.push(at);
                pacer.mark(at);
            }
        }
        // A preview every 2 s of speech, the first at the 2 s mark.
        let timeline = previews
            .iter()
            .map(|at| format!("{:.1}s", *at as f64 / RATE as f64))
            .collect::<Vec<_>>()
            .join(", ");
        println!("live preview timeline over 10 s of ongoing speech: {timeline}");
        assert_eq!(
            previews,
            vec![samples(2), samples(4), samples(6), samples(8), samples(10)]
        );
    }

    /// A tail that just reached the floor decodes at the next cadence tick;
    /// a shorter one waits for more speech rather than spending a run on it.
    #[test]
    fn a_tail_shorter_than_the_floor_waits_for_more_speech() {
        let pacer = PreviewPacer::new(2000);
        assert!(!pacer.is_due(samples(5), RATE / 2));
        assert!(pacer.is_due(samples(5), RATE));
    }

    /// A cadence from the config file reaches the pacer: a shorter interval
    /// means more frequent previews, none before its first tick.
    #[test]
    fn the_interval_comes_from_the_config() {
        let pacer = PreviewPacer::new(500);
        let tick = RATE / 2; // 500 ms of audio
        let tail = RATE; // a full second of speech: past the floor
        assert!(!pacer.is_due(tick - 1, tail));
        assert!(pacer.is_due(tick, tail));
    }
}
