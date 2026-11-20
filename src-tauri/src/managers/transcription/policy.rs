//! How audio is decoded: the one place the model's traits and the settings
//! choose between streaming, long-form chunking and a single run.

use crate::engine_supervisor::LoadedInfo;
use crate::settings::PINNED_MODEL_ID;

/// What a model can do that decides how its audio is decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelTraits {
    /// Decodes audio incrementally as it is recorded.
    pub streaming: bool,
    /// Keeps its accuracy on audio longer than its native window in one run
    /// (whisper today).
    pub long_form: bool,
}

impl ModelTraits {
    /// The loaded model's own traits: the authority once a model is loaded.
    pub fn of(info: &LoadedInfo) -> Self {
        Self {
            streaming: info.capabilities.supports_streaming,
            long_form: info.supports_long_form,
        }
    }
}

/// The traits the live-decode policy should see for `model_id`.
///
/// Voicey's one dictation model (Parakeet TDT 0.6B v3) advertises long-form
/// support — its VAD head segments long audio inside a single run — so the
/// generic policy would leave a recording undecoded until stop and only
/// then run it whole: no live text for the preview overlay. The live
/// preview is the point, so for the pinned model the app-side chunker
/// always wins: chunks are cut at pauses and decoded while the recording
/// continues, and each one feeds the overlay as it lands.
pub fn voicey_model_traits(model_id: &str, info: &LoadedInfo) -> ModelTraits {
    let mut traits = ModelTraits::of(info);
    if model_id == PINNED_MODEL_ID {
        traits.long_form = false;
    }
    traits
}

/// How a recording is decoded while it is still going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveDecode {
    /// The model decodes the audio as it arrives.
    Stream,
    /// Long audio is cut at pauses and each chunk decoded once its cut is
    /// certain.
    Chunked,
    /// Nothing until the recording stops; then it is decoded in batch.
    None,
}

/// How a whole clip is decoded: a recording without a live result, a
/// re-transcribed history entry, or a file from the CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchDecode {
    /// Cut at pauses into chunks of about 30 s, each decoded in turn; a clip
    /// of 40 s or less is one chunk.
    Chunked,
    /// One run.
    Whole,
}

/// Streaming wins: a streaming model handles long audio itself. Otherwise
/// long audio is chunked unless the model keeps its accuracy on it.
pub fn live_decode(model: ModelTraits, long_form_chunking: bool) -> LiveDecode {
    if model.streaming {
        LiveDecode::Stream
    } else if long_form_chunking && !model.long_form {
        LiveDecode::Chunked
    } else {
        LiveDecode::None
    }
}

/// As live, except that a streaming model takes a whole clip in one run, as
/// it did before long-form chunking existed.
pub fn batch_decode(model: ModelTraits, long_form_chunking: bool) -> BatchDecode {
    if long_form_chunking && !model.long_form && !model.streaming {
        BatchDecode::Chunked
    } else {
        BatchDecode::Whole
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_supervisor::LoadedInfo;
    use transcribe_cpp::{Capabilities, TimestampKind};

    /// A LoadedInfo with just the traits the policy reads.
    fn info(streaming: bool, long_form: bool) -> LoadedInfo {
        LoadedInfo {
            arch: "parakeet".into(),
            variant: "tdt-0.6b-v3".into(),
            backend: "test".into(),
            device: "test".into(),
            on_gpu: false,
            capabilities: Capabilities {
                native_sample_rate: 16_000,
                languages: Vec::new(),
                translate_target_languages: Vec::new(),
                max_timestamp_kind: TimestampKind::None,
                supports_language_detect: true,
                supports_translate: false,
                supports_streaming: streaming,
                supports_spec_decode: false,
                max_audio_ms: 0,
            },
            supports_initial_prompt: false,
            supports_long_form: long_form,
            supports_vocabulary: false,
        }
    }

    /// The pinned model's own long-form claim (its VAD head) would suppress
    /// the app-side chunker and with it the live preview; Voicey's override
    /// routes the pinned model through chunked decoding when chunking is on.
    #[test]
    fn the_pinned_model_is_always_decoded_in_chunks() {
        // What the engine reports for Parakeet TDT 0.6B v3: non-streaming,
        // long-form capable.
        let traits = voicey_model_traits(crate::settings::PINNED_MODEL_ID, &info(false, true));
        assert!(!traits.streaming);
        assert!(!traits.long_form, "the override wins for the pinned model");
        assert_eq!(
            live_decode(traits, true),
            LiveDecode::Chunked,
            "chunked live decode: the live preview gets text mid-speech"
        );
    }

    /// Any other model keeps the engine's own claim untouched.
    #[test]
    fn other_models_keep_their_own_traits() {
        let traits =
            voicey_model_traits("handy-computer/other-gguf/other.gguf", &info(false, true));
        assert!(traits.long_form);
        assert_eq!(live_decode(traits, true), LiveDecode::None);
    }
}
