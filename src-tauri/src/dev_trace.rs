//! Dev-only grounding-trace capture (the D10 compile-time privacy rule).
//!
//! In dev builds, a grounded dictation can be recorded for study: the raw
//! transcript prompt, the captured screenshot, the vision model's extracted
//! reading, and how it changed the final pasted output — one session folder
//! per dictation under `Data/logs/grounding/`. That is exactly what a dev
//! build may capture; a **release build compiles this entire module into
//! inert stubs**, so it is structurally incapable of writing any of it, no
//! matter what the config file asks for. The `dev.grounding_trace` config
//! key only gates whether a dev build actually writes.
//!
//! Compiled only where the rest of the dev telemetry also lives (the
//! `image` dependency and the capture path exist on the same targets):
//! Windows x64 and Linux dev builds. Everything else gets the stub too.
//!
//! Nothing here blocks the dictation: every step is fire-and-forget, and a
//! failure to persist costs one log line.

use crate::screen_capture::ScreenFrame;

#[cfg(all(
    debug_assertions,
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        target_os = "linux"
    )
))]
pub use imp::GroundingTrace;

#[cfg(not(all(
    debug_assertions,
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        target_os = "linux"
    )
)))]
pub use stub::GroundingTrace;

#[cfg(all(
    debug_assertions,
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        target_os = "linux"
    )
))]
mod imp {
    use std::path::{Path, PathBuf};

    use log::{debug, warn};

    use super::ScreenFrame;

    /// The VLM protocol (system prompt, user-message shape) only exists
    /// where the llama.cpp worker is compiled in — the same targets this
    /// module is — so the trace always records the prompt verbatim.
    fn grounding_prompt(transcript: &str) -> Option<(String, String)> {
        Some((
            crate::vlm_supervisor::GROUNDING_SYSTEM_PROMPT.to_string(),
            crate::vlm_supervisor::grounding_user_message(transcript),
        ))
    }

    /// The trace's on-disk shape (also the schema marker for future readers).
    const TRACE_SCHEMA: u32 = 1;

    /// One grounded dictation's trace, written when dropped: even a
    /// cancelled dictation leaves the partial record behind instead of a
    /// folder with only a screenshot.
    pub struct GroundingTrace {
        /// The session folder this trace owns, and writes `trace.json` into
        /// on drop. `None` = inactive (capture off, or the folder could not
        /// be created): every record call is then a no-op.
        dir: Option<PathBuf>,
        started_utc: String,
        transcript: Option<String>,
        prompt: Option<(String, String)>,
        reading: Option<String>,
        reading_failed: bool,
        final_output: Option<String>,
    }

    impl GroundingTrace {
        /// Begins a trace for one grounded dictation, filed under the app's
        /// `Data/logs/grounding/`. The verbatim binding passes `grounded =
        /// false` and gets an inert trace (nothing recorded, nothing
        /// written). `frame` is the screen captured at release:
        /// it is persisted immediately (it will be gone by the time the
        /// grounding pass returns). `None` when capture itself failed — the
        /// trace still records the text halves.
        pub fn start(grounded: bool, frame: Option<&ScreenFrame>) -> Self {
            if !grounded {
                return Self::inactive();
            }
            // The config key only gates dev builds; release builds never
            // reach this code at all (see the stub below).
            let enabled = crate::config_file::load_or_create()
                .dev
                .grounding_trace
                .unwrap_or(true);
            if !enabled {
                debug!("Dev grounding trace disabled in the config file");
                return Self::inactive();
            }
            let root = crate::portable::data_dir().map(|dir| dir.join("logs").join("grounding"));
            Self::start_at(root, frame)
        }

        /// `start` with an explicit sessions folder (the test seam).
        fn start_at(root: Option<PathBuf>, frame: Option<&ScreenFrame>) -> Self {
            let Some(root) = root else {
                debug!("Dev grounding trace: no data dir; trace not started");
                return Self::inactive();
            };
            let dir = root.join(chrono::Utc::now().format("%Y%m%d-%H%M%S%.3f").to_string());
            if let Err(e) = std::fs::create_dir_all(&dir) {
                warn!(
                    "Dev grounding trace: could not create {}: {e}",
                    dir.display()
                );
                return Self::inactive();
            }

            let started_utc = chrono::Utc::now().to_rfc3339();
            if let Some(frame) = frame {
                if let Err(e) = write_screenshot(&dir, frame) {
                    warn!("Dev grounding trace: screenshot not saved: {e}");
                }
            }

            Self {
                dir: Some(dir),
                started_utc,
                transcript: None,
                prompt: None,
                reading: None,
                reading_failed: false,
                final_output: None,
            }
        }

        fn inactive() -> Self {
            Self {
                dir: None,
                started_utc: String::new(),
                transcript: None,
                prompt: None,
                reading: None,
                reading_failed: false,
                final_output: None,
            }
        }

        /// The raw transcript (the verbatim ASR output the grounding pass
        /// receives) and the exact prompt sent with the frame.
        pub fn record_request(&mut self, transcript: &str) {
            if self.dir.is_none() {
                return;
            }
            self.transcript = Some(transcript.to_string());
            self.prompt = grounding_prompt(transcript);
        }

        /// What the vision model read off the screen.
        pub fn record_reading(&mut self, grounded_text: &str) {
            if self.dir.is_none() {
                return;
            }
            self.reading = Some(grounded_text.to_string());
        }

        /// The pass was asked for but produced nothing (failed, cancelled,
        /// or blank): the dictation fell back to the verbatim transcript.
        pub fn record_reading_failed(&mut self) {
            self.reading_failed = true;
        }

        /// The text the dictation actually pasted.
        pub fn record_final_output(&mut self, text: &str) {
            if self.dir.is_none() {
                return;
            }
            self.final_output = Some(text.to_string());
        }

        /// Everything recorded so far, as `trace.json`. Called from Drop.
        fn write(&self) {
            let Some(dir) = &self.dir else { return };
            let prompt = self
                .prompt
                .as_ref()
                .map(|(system, user)| serde_json::json!({ "system": system, "user": user }));
            let record = serde_json::json!({
                "schema": TRACE_SCHEMA,
                "started_utc": self.started_utc,
                "model": crate::managers::model::SCREEN_VLM_ID,
                // The exact inputs to the vision pass.
                "transcript": self.transcript,
                "prompt": prompt,
                // What came out.
                "reading": self.reading,
                "reading_failed": self.reading_failed,
                "final_output": self.final_output,
                // False when the dictation was cancelled before a final
                // output existed.
                "completed": self.final_output.is_some(),
                "screenshot": "screenshot.png",
            });
            let path = dir.join("trace.json");
            match serde_json::to_string_pretty(&record) {
                Ok(text) => {
                    if let Err(e) = std::fs::write(&path, text) {
                        warn!(
                            "Dev grounding trace: could not write {}: {e}",
                            path.display()
                        );
                    } else {
                        debug!("Dev grounding trace recorded: {}", path.display());
                    }
                }
                Err(e) => warn!("Dev grounding trace could not serialize: {e}"),
            }
        }
    }

    impl Drop for GroundingTrace {
        fn drop(&mut self) {
            self.write();
        }
    }

    /// The captured frame as the session's `screenshot.png`.
    fn write_screenshot(dir: &Path, frame: &ScreenFrame) -> std::io::Result<()> {
        use image::ImageFormat;
        use std::io::Cursor;

        let image = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone())
            .ok_or_else(|| std::io::Error::other("frame buffer does not match its dimensions"))?;
        let mut bytes = Cursor::new(Vec::new());
        image
            .write_to(&mut bytes, ImageFormat::Png)
            .map_err(|e| std::io::Error::other(format!("PNG encode failed: {e}")))?;
        std::fs::write(dir.join("screenshot.png"), bytes.into_inner())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A full trace writes the screenshot and a record carrying every
        /// documented half: transcript, prompt, reading, final output.
        #[test]
        fn a_full_trace_writes_every_documented_half() {
            let root = std::env::temp_dir().join("voicey-dev-trace-test-full");
            std::fs::create_dir_all(&root).expect("temp dir");

            let frame = ScreenFrame::new(2, 1, vec![10, 20, 30, 255, 40, 50, 60, 255])
                .expect("valid frame");
            let mut trace = GroundingTrace::start_at(Some(root.clone()), Some(&frame));
            trace.record_request("paste the error code");
            trace.record_reading("paste the E1702 error code");
            trace.record_final_output("paste the E1702 error code");
            drop(trace);

            let dirs = session_dirs(&root);
            let dir = dirs.last().expect("the session folder");
            let screenshot = std::fs::read(dir.join("screenshot.png")).expect("screenshot");
            // A PNG the image crate wrote reads back with the same signature.
            assert!(screenshot.starts_with(&[0x89, b'P', b'N', b'G']));
            let record: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(dir.join("trace.json")).expect("trace.json"),
            )
            .expect("valid JSON");
            assert_eq!(record["transcript"], "paste the error code");
            assert_eq!(record["reading"], "paste the E1702 error code");
            assert_eq!(record["final_output"], "paste the E1702 error code");
            assert_eq!(record["completed"], true);
            assert!(!record["reading_failed"].as_bool().unwrap());
            // The prompt halves are recorded where the VLM worker is compiled
            // in; on this target it is, so they must be present and verbatim.
            let prompt = &record["prompt"];
            assert!(prompt["system"]
                .as_str()
                .unwrap()
                .contains("resolve references"));
            assert_eq!(
                prompt["user"].as_str().unwrap(),
                format!(
                    "{}\npaste the error code",
                    crate::vlm_supervisor::MEDIA_MARKER
                )
            );
        }

        /// A cancelled dictation still leaves a marked-incomplete record
        /// instead of a folder with only a screenshot.
        #[test]
        fn an_incomplete_trace_records_its_halts() {
            let root = std::env::temp_dir().join("voicey-dev-trace-test-incomplete");
            std::fs::create_dir_all(&root).expect("temp dir");

            let mut trace = GroundingTrace::start_at(Some(root.clone()), None);
            trace.record_request("look at the dialog");
            drop(trace);

            let dir = session_dirs(&root)
                .last()
                .expect("the new session folder")
                .clone();
            assert!(!dir.join("screenshot.png").exists(), "capture had failed");
            let record: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(dir.join("trace.json")).expect("trace.json"),
            )
            .expect("valid JSON");
            assert_eq!(record["completed"], false);
        }

        /// The dev capture path end to end (D10): a synthetic grounded run
        /// through the REAL in-process vision model — synthetic frame,
        /// synthetic transcript, real Qwen3-VL reading — with the trace
        /// driven exactly as the dictation pipeline drives it, then checked
        /// on disk. Ignored by default: it needs the multi-gigabyte pinned
        /// model files.
        ///
        /// Run with:
        /// `HANDY_VLM_MODEL=<q4 gguf> HANDY_VLM_MMPROJ=<mmproj gguf> cargo
        ///  test --lib dev_trace -- --ignored --nocapture`
        #[test]
        #[ignore = "needs the real multi-gigabyte model files"]
        fn a_synthetic_grounded_run_writes_its_trace() {
            let model_path = std::env::var_os("HANDY_VLM_MODEL")
                .expect("set HANDY_VLM_MODEL to the pinned Qwen3-VL gguf");
            let mmproj_path = std::env::var_os("HANDY_VLM_MMPROJ")
                .expect("set HANDY_VLM_MMPROJ to the pinned mmproj gguf");

            // The trace files under the app's real Data/logs/grounding (the
            // portable layout the test binary carries; see portable tests).
            crate::portable::init();
            let root = crate::portable::data_dir()
                .expect("data dir")
                .join("logs")
                .join("grounding");
            let before = session_dirs(&root).len();

            // A small synthetic "screenshot": distinct color quadrants, the
            // bottom-right one striped like lines of text.
            let width = 512u32;
            let height = 512u32;
            let mut rgba = vec![0u8; (width * height * 4) as usize];
            for y in 0..height {
                for x in 0..width {
                    let i = ((y * width + x) * 4) as usize;
                    let (r, g, b) = if y < height / 2 {
                        if x < width / 2 {
                            (200, 40, 40)
                        } else {
                            (40, 40, 200)
                        }
                    } else if x < width / 2 {
                        (40, 180, 40)
                    } else if (y % 16) < 8 {
                        (10, 10, 10)
                    } else {
                        (245, 245, 245)
                    };
                    rgba[i] = r;
                    rgba[i + 1] = g;
                    rgba[i + 2] = b;
                    rgba[i + 3] = 255;
                }
            }
            let frame =
                ScreenFrame::new(width, height, rgba.clone()).expect("valid synthetic frame");

            // The dictation pipeline's order: the trace starts when the
            // frame is captured, records the request before the pass, the
            // reading after it, and the final output last.
            let mut trace = GroundingTrace::start(true, Some(&frame));
            let transcript = "the red box is in the top left corner and the \
                blue one is in the top right corner, name both";
            trace.record_request(transcript);

            // The real in-process vision pass (same path the worker runs).
            let backend = Box::leak(Box::new(
                llama_cpp_2::llama_backend::LlamaBackend::init().expect("llama.cpp backends init"),
            ));
            let mut loaded = crate::vlm_supervisor::worker::load(
                backend,
                model_path.as_ref(),
                mmproj_path.as_ref(),
                // CPU: the test machines may share their GPU with the app.
                0,
            )
            .expect("load the pinned vision model");
            let options =
                crate::vlm_supervisor::GroundOptions::new(width, height, transcript.to_string());
            let mut sampler = llama_cpp_2::sampling::LlamaSampler::chain_simple(
                [llama_cpp_2::sampling::LlamaSampler::greedy()],
            );
            let grounded =
                crate::vlm_supervisor::worker::ground(&mut loaded, &mut sampler, &options, &rgba)
                    .expect("ground the transcript against the frame");
            eprintln!("grounded: {grounded}");
            assert!(!grounded.trim().is_empty());

            trace.record_reading(&grounded);
            trace.record_final_output(&grounded);
            drop(trace);

            // The record on disk: screenshot + trace.json carrying every
            // documented half, including the model's real reading.
            let dirs = session_dirs(&root);
            assert_eq!(dirs.len(), before + 1, "one new session folder");
            let dir = dirs.last().expect("the new session folder");
            assert!(dir.join("screenshot.png").exists());
            let record: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(dir.join("trace.json")).expect("trace.json"),
            )
            .expect("valid JSON");
            assert_eq!(record["transcript"], transcript);
            assert_eq!(record["reading"], grounded);
            assert_eq!(record["final_output"], grounded);
            assert_eq!(record["completed"], true);
            let prompt = &record["prompt"];
            assert!(prompt["system"]
                .as_str()
                .unwrap()
                .contains("resolve references"));
            assert_eq!(
                prompt["user"].as_str().unwrap(),
                format!("{}\n{transcript}", crate::vlm_supervisor::MEDIA_MARKER)
            );
            eprintln!("trace written: {}", dir.display());
        }

        fn session_dirs(root: &Path) -> Vec<PathBuf> {
            if !root.exists() {
                return Vec::new();
            }
            let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
                .expect("sessions folder")
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect();
            dirs.sort();
            dirs
        }
    }
}

/// Release builds (and unsupported dev platforms): the module's API as inert
/// stubs. No file is opened, no path built, nothing written — the capability
/// does not exist here.
#[cfg(not(all(
    debug_assertions,
    any(
        all(target_os = "windows", target_arch = "x86_64"),
        target_os = "linux"
    )
)))]
mod stub {
    use super::ScreenFrame;

    pub struct GroundingTrace;

    impl GroundingTrace {
        pub fn start(_grounded: bool, _frame: Option<&ScreenFrame>) -> Self {
            GroundingTrace
        }

        pub fn record_request(&mut self, _transcript: &str) {}

        pub fn record_reading(&mut self, _grounded_text: &str) {}

        pub fn record_reading_failed(&mut self) {}

        pub fn record_final_output(&mut self, _text: &str) {}
    }
}

/// Compile-time proof of the privacy rule: release builds (and unsupported
/// dev platforms) carry the zero-sized inert stub; only dev builds on the
/// supported targets carry the real, writing trace.
#[cfg(test)]
mod tests {
    use super::GroundingTrace;

    #[test]
    fn release_builds_carry_an_inert_trace() {
        if cfg!(all(
            debug_assertions,
            any(
                all(target_os = "windows", target_arch = "x86_64"),
                target_os = "linux"
            )
        )) {
            assert!(
                std::mem::size_of::<GroundingTrace>() > 0,
                "dev builds on supported targets get the writing trace"
            );
        } else {
            assert_eq!(
                std::mem::size_of::<GroundingTrace>(),
                0,
                "this build must carry the inert stub: no trace can be written"
            );
        }
    }
}
