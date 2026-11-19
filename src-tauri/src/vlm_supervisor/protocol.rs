//! Wire protocol between Voicey and its llama.cpp vision worker.
//!
//! Strict request/response, same framed shape as the transcription engine's:
//! `[u32 json_len][json][u32 blob_len][blob bytes]` (lengths little-endian).
//! The blob section carries the screen frame as raw RGBA bytes; the JSON
//! carries its dimensions, so the worker's bitmaps and the parent's frames
//! can never disagree about the geometry.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::PathBuf;

/// Refuse frames larger than this: a garbage length must not turn into a
/// multi-gigabyte allocation. An 8K monitor's RGBA frame is ~132 MB.
const MAX_SECTION_BYTES: usize = 256 << 20;

/// Media marker the worker's mtmd context is configured with, and which the
/// grounding prompt embeds where the frame belongs. Must stay identical to
/// what the worker passes in `MtmdContextParams::media_marker` (the
/// consistency is asserted by a worker-side unit test).
pub const MEDIA_MARKER: &str = "<__media__>";

/// The fixed system instruction: resolve on-screen references into the
/// transcript, keep everything else as dictated.
pub const GROUNDING_SYSTEM_PROMPT: &str = "You resolve references in a dictated transcript \
against a screenshot of the user's screen. Rewrite the transcript so that every reference to \
on-screen content is replaced by the actual text or label it points at: button labels, menu \
items, window titles, file and folder names, error messages, code identifiers, values visible \
in fields, the text of a selection. Keep the transcript's own wording, language, punctuation \
and intent; change only what a reference needs resolved. If the screen does not contain what \
something refers to, leave that part unchanged. Reply with the rewritten transcript only — \
no preamble, no quotation marks, no commentary.";

/// The user message sent with each frame: the frame's marker first, then the
/// transcript it grounds.
pub fn grounding_user_message(transcript: &str) -> String {
    format!("{MEDIA_MARKER}\n{transcript}")
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// First request to every worker; answered once backends are initialized
    /// (with an error if that failed, which no retry could fix).
    Hello,
    Load {
        model_path: PathBuf,
        mmproj_path: PathBuf,
        /// GPU layers to offload; 0 keeps every layer on the CPU.
        gpu_layers: u32,
    },
    /// Followed by the RGBA frame to ground against.
    Ground { options: GroundOptions },
}

/// Per-grounding generation limits, decided by the parent (it knows the
/// transcript's length; the worker just obeys).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroundOptions {
    pub width: u32,
    pub height: u32,
    /// Transcript whose on-screen references are being resolved.
    pub transcript: String,
    /// Generation cap: enough for the rewrite plus resolved labels, never
    /// an unbounded runaway.
    pub max_tokens: u32,
}

impl GroundOptions {
    /// Frame geometry and transcript for a grounding pass, with the
    /// generation cap sized from the transcript (a rewrite is about as long
    /// as its input, plus room for resolved labels).
    pub fn new(width: u32, height: u32, transcript: String) -> Self {
        let max_tokens = (transcript.len() as u64 / 3 + 256).clamp(256, 2048) as u32;
        Self {
            width,
            height,
            transcript,
            max_tokens,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Hello,
    Loaded(LoadedInfo),
    Grounded(String),
    Error(String),
}

/// What the parent needs to know about a loaded vision model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedInfo {
    /// Whether the model was asked to offload to the GPU.
    pub on_gpu: bool,
}

/// Encode one frame. Refuses a section the reader would refuse, so an
/// oversized request fails cleanly here instead of killing the worker.
pub fn encode_message<T: Serialize>(message: &T, blob: Option<&[u8]>) -> io::Result<Vec<u8>> {
    let json = serde_json::to_vec(message).map_err(io::Error::other)?;
    check_section_len(json.len())?;
    let blob = blob.unwrap_or(&[]);
    check_section_len(blob.len())?;
    let mut frame = Vec::with_capacity(8 + json.len() + blob.len());
    frame.extend_from_slice(&(json.len() as u32).to_le_bytes());
    frame.extend_from_slice(&json);
    frame.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    frame.extend_from_slice(blob);
    Ok(frame)
}

pub fn write_message<T: Serialize>(
    w: &mut impl Write,
    message: &T,
    blob: Option<&[u8]>,
) -> io::Result<()> {
    w.write_all(&encode_message(message, blob)?)?;
    w.flush()
}

/// Read one frame. `Ok(None)` only on a clean EOF before the first byte of a
/// frame; a frame cut off part-way is an error.
pub fn read_message<T: for<'de> Deserialize<'de>>(
    r: &mut impl Read,
) -> io::Result<Option<(T, Vec<u8>)>> {
    let mut len = [0u8; 4];
    let mut filled = 0;
    while filled < len.len() {
        match r.read(&mut len[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let json = read_section_body(r, len)?;
    let message = serde_json::from_slice(&json).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("vision worker protocol message is not valid JSON: {e}"),
        )
    })?;
    let blob = read_section(r)?;
    Ok(Some((message, blob)))
}

fn read_section(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    read_section_body(r, len)
}

fn read_section_body(r: &mut impl Read, len: [u8; 4]) -> io::Result<Vec<u8>> {
    let len = u32::from_le_bytes(len) as usize;
    check_section_len(len)?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

fn check_section_len(len: usize) -> io::Result<()> {
    if len > MAX_SECTION_BYTES {
        return Err(io::Error::other(format!("frame section too large: {len}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_with_a_blob() {
        let options = GroundOptions::new(4, 2, "the button at the top".into());
        let request = Request::Ground {
            options: options.clone(),
        };
        let blob: Vec<u8> = (0..32u8).collect();
        let frame = encode_message(&request, Some(&blob)).unwrap();

        let mut cursor = io::Cursor::new(frame);
        let (back, back_blob) = read_message::<Request>(&mut cursor).unwrap().unwrap();
        match back {
            Request::Ground { options: seen } => {
                assert_eq!(seen.transcript, options.transcript);
                assert_eq!(seen.width, 4);
                assert_eq!(seen.height, 2);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(back_blob, blob);
    }

    #[test]
    fn empty_blob_round_trips() {
        let frame = encode_message(&Request::Hello, None).unwrap();
        let mut cursor = io::Cursor::new(frame);
        let (back, blob) = read_message::<Request>(&mut cursor).unwrap().unwrap();
        assert!(matches!(back, Request::Hello));
        assert!(blob.is_empty());
    }

    #[test]
    fn responses_round_trip() {
        for response in [
            Response::Hello,
            Response::Loaded(LoadedInfo { on_gpu: true }),
            Response::Grounded("resolved".into()),
            Response::Error("no".into()),
        ] {
            let frame = encode_message(&response, None).unwrap();
            let mut cursor = io::Cursor::new(frame);
            let (back, _) = read_message::<Response>(&mut cursor)
                .unwrap()
                .expect("one message");
            assert!(matches!(
                (back, &response),
                (Response::Hello, Response::Hello)
                    | (Response::Loaded(_), Response::Loaded(_))
                    | (Response::Grounded(_), Response::Grounded(_))
                    | (Response::Error(_), Response::Error(_))
            ));
        }
    }

    #[test]
    fn truncated_frame_is_an_error() {
        let frame = encode_message(&Request::Hello, Some(&[1, 2, 3])).unwrap();
        let mut cursor = io::Cursor::new(&frame[..frame.len() - 1]);
        assert!(read_message::<Request>(&mut cursor).is_err());
    }

    #[test]
    fn clean_eof_before_a_frame_is_none() {
        let mut cursor = io::Cursor::new(Vec::new());
        assert!(read_message::<Request>(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn oversized_section_is_refused() {
        let options = GroundOptions::new(4, 2, String::new());
        assert!(encode_message(&Request::Ground { options }, None).is_ok());
        // A fake length header over the limit is refused by the reader, not
        // turned into an allocation.
        let mut evil = vec![0u8; 8];
        evil[..4].copy_from_slice(&((MAX_SECTION_BYTES as u32) + 1).to_le_bytes());
        let mut cursor = io::Cursor::new(evil);
        assert!(read_message::<Request>(&mut cursor).is_err());
    }

    #[test]
    fn the_user_message_puts_the_frame_before_the_transcript() {
        assert_eq!(
            grounding_user_message("send it to the deploy channel"),
            "<__media__>\nsend it to the deploy channel"
        );
    }

    #[test]
    fn the_generation_cap_scales_with_the_transcript() {
        assert_eq!(GroundOptions::new(1, 1, String::new()).max_tokens, 256);
        assert_eq!(
            GroundOptions::new(1, 1, "x".repeat(300)).max_tokens,
            256 + 100
        );
        // A very long dictation still stays bounded.
        assert_eq!(
            GroundOptions::new(1, 1, "x".repeat(100_000)).max_tokens,
            2048
        );
    }
}
