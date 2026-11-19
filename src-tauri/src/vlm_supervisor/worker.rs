//! The vision worker process: the only place llama.cpp native code runs.
//! Owns the language model and its multimodal projection, and serves
//! [`Request`]s until the parent closes its stdin — the same shape as the
//! transcription worker in `engine_supervisor::worker`.
//!
//! A dedicated thread reads stdin so the parent's writes never block, and
//! the worker exits the moment the parent goes away, even if hung.

use super::protocol::{
    read_message, write_message, GroundOptions, LoadedInfo, Request, Response,
    GROUNDING_SYSTEM_PROMPT, MEDIA_MARKER,
};
use super::{CPU_ONLY_FLAG, LOG_LEVEL_ENV};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatMessage, LlamaChatTemplate, LlamaModel};
use llama_cpp_2::mtmd::{MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputText};
use llama_cpp_2::sampling::LlamaSampler;
use log::{error, info, LevelFilter, Log, Metadata, Record};
use std::ffi::CString;
use std::fs::File;
use std::io::{self, BufReader, Write};
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

/// Prefix on worker log lines so the parent can tell them apart from raw
/// native output (an abort message right before a crash). Same as the
/// transcription worker's, which the parent's forwarding already knows.
pub(super) const LOG_LINE_PREFIX: &str = "\u{1}";

/// Context window: image tokens (capped below), the formatted prompt, and
/// the generated rewrite all fit; a 4B model's KV cache at this size stays
/// within a normal dictation's memory budget.
const CONTEXT_TOKENS: u32 = 8192;
/// Vision token budget per frame. A large monitor would otherwise flood the
/// context; llama.cpp scales the frame down to fit this.
const IMAGE_MAX_TOKENS: i32 = 2048;
const IMAGE_MIN_TOKENS: i32 = 512;
/// Batch for prefill and image-chunk decode. Must be at least the largest
/// image token budget: mtmd's helper adds the whole image slice to a batch
/// sized from the context's `n_batch`, and a smaller one aborts mid-encode
/// (`GGML_ASSERT(idx >= 0)` in mtmd-helper-common.h).
const BATCH_TOKENS: usize = IMAGE_MAX_TOKENS as usize;

/// A request as handed from the stdin reader to the main loop.
struct Incoming {
    request: Request,
    blob: Vec<u8>,
}

/// The loaded vision model, kept alive between requests. The backend lives
/// for the process's whole life (created once in [`run`]).
struct Loaded {
    backend: &'static LlamaBackend,
    model: LlamaModel,
    projection: MtmdContext,
    /// From the GGUF's own metadata; built once per load.
    template: LlamaChatTemplate,
}

pub fn run() -> i32 {
    // Same allocator tuning as the app and the transcription worker.
    crate::memory::init_allocator();
    #[cfg(unix)]
    ignore_app_signals();
    #[cfg(target_os = "linux")]
    set_process_name();
    // Take the real stdout for the protocol before any native code runs;
    // ggml writes to stdout in places, and a stray byte there would corrupt
    // the protocol stream.
    let protocol_out = match take_stdout_for_protocol() {
        Ok(file) => file,
        Err(e) => {
            eprintln!("vlm worker: failed to claim stdout: {e}");
            return 2;
        }
    };
    init_logger();

    let (requests_tx, requests) = mpsc::channel();
    if let Err(e) = thread::Builder::new()
        .name("vlm-worker-in".into())
        .spawn(move || read_requests(requests_tx))
    {
        error!("Failed to start the request reader: {}", e);
        return 2;
    }

    // Registered backends are process-global; keep one instance for the
    // whole worker so nothing re-runs driver code per request.
    let backend = match LlamaBackend::init() {
        Ok(backend) => Box::leak(Box::new(backend)),
        Err(e) => {
            // No retry could fix this; the hello reports it.
            error!("Failed to initialize llama.cpp backends: {e:?}");
            serve_without_backend(protocol_out, requests);
            return 0;
        }
    };
    let cpu_only = std::env::args_os().any(|arg| arg == CPU_ONLY_FLAG);
    info!(
        "llama.cpp backends initialized ({})",
        if cpu_only { "cpu-only" } else { "all" }
    );

    let mut output = protocol_out;
    let mut loaded: Option<Loaded> = None;
    let mut sampler = LlamaSampler::chain_simple([LlamaSampler::greedy()]);

    while let Ok(Incoming { request, blob }) = requests.recv() {
        let response = match request {
            Request::Hello => Response::Hello,
            Request::Load {
                model_path,
                mmproj_path,
                gpu_layers,
            } => {
                // Free any previous model first to avoid holding two at once.
                loaded = None;
                match load(backend, &model_path, &mmproj_path, gpu_layers) {
                    Ok(new) => {
                        let on_gpu = gpu_layers > 0;
                        loaded = Some(new);
                        Response::Loaded(LoadedInfo { on_gpu })
                    }
                    Err(e) => Response::Error(e),
                }
            }
            Request::Ground { options } => match loaded.as_mut() {
                Some(state) => match ground(state, &mut sampler, &options, &blob) {
                    Ok(text) => Response::Grounded(text),
                    Err(e) => Response::Error(e),
                },
                None => Response::Error("no vision model is loaded".to_string()),
            },
        };
        if write_message(&mut output, &response, None).is_err() {
            error!("Protocol write failed");
            return 1;
        }
        // The frame never outlives the request it grounded, whatever the
        // outcome. Large transients go back to the OS rather than sitting
        // in malloc arenas between dictations.
        drop(blob);
        crate::memory::trim_freed_memory();
    }
    0
}

/// Answer hello-only after a failed backend init, so the parent reads its
/// error instead of seeing a spawn that died silently.
fn serve_without_backend(mut output: File, requests: mpsc::Receiver<Incoming>) {
    while let Ok(Incoming { request, .. }) = requests.recv() {
        let response = match request {
            Request::Hello => Response::Error("failed to initialize llama.cpp backends".into()),
            _ => Response::Error("the vision worker has no compute backend".into()),
        };
        if write_message(&mut output, &response, None).is_err() {
            return;
        }
    }
}

/// The stdin reader. Forwards requests to the main loop, and ends the process
/// when the parent closes stdin (unload, quit, or the parent died). `_exit`
/// skips C++ static destructors, so a model still alive at that point can't
/// trip teardown asserts, and the OS reclaims its CPU and GPU memory.
fn read_requests(requests: mpsc::Sender<Incoming>) -> ! {
    let mut input = BufReader::new(io::stdin().lock());
    loop {
        match read_message::<Request>(&mut input) {
            Ok(Some((request, blob))) => {
                if requests.send(Incoming { request, blob }).is_err() {
                    exit_now(1);
                }
            }
            Ok(None) => exit_now(0),
            Err(e) => {
                error!("Protocol read failed: {}", e);
                exit_now(1);
            }
        }
    }
}

fn exit_now(code: i32) -> ! {
    let _ = io::stderr().flush();
    // SAFETY: `_exit` only ends the process; nothing runs afterwards.
    unsafe { libc::_exit(code) }
}

/// SIGUSR2 toggles transcription in the app (and WebKitGTK uses SIGUSR1).
/// Aimed at Voicey by name, one can reach this worker instead, and its
/// default action would kill it.
#[cfg(unix)]
fn ignore_app_signals() {
    // SAFETY: setting a signal's disposition to SIG_IGN has no preconditions.
    unsafe {
        libc::signal(libc::SIGUSR1, libc::SIG_IGN);
        libc::signal(libc::SIGUSR2, libc::SIG_IGN);
    }
}

/// Started from /proc/self/exe, the kernel names this process "exe"; give it
/// a name of its own that no `pkill` pattern aimed at the app matches.
#[cfg(target_os = "linux")]
fn set_process_name() {
    // SAFETY: PR_SET_NAME copies a NUL-terminated name of up to 16 bytes.
    unsafe {
        libc::prctl(libc::PR_SET_NAME, c"voicey-vlm".as_ptr());
    }
}

#[cfg(unix)]
fn take_stdout_for_protocol() -> io::Result<File> {
    use std::os::fd::FromRawFd;
    // SAFETY: plain fd duplication; the new fd is owned by the returned File.
    unsafe {
        let fd = libc::dup(libc::STDOUT_FILENO);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(File::from_raw_fd(fd))
    }
}

#[cfg(windows)]
fn take_stdout_for_protocol() -> io::Result<File> {
    use std::os::windows::io::FromRawHandle;
    // Native code writes through the CRT's fd 1, so redirect that one. The
    // protocol uses the OS handle behind a CRT duplicate of the original.
    // SAFETY: CRT fd duplication; the duplicate's handle is owned by the File
    // for the rest of the process.
    unsafe {
        let fd = libc::dup(1);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::dup2(2, 1) < 0 {
            return Err(io::Error::last_os_error());
        }
        let handle = libc::get_osfhandle(fd);
        if handle == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(File::from_raw_handle(handle as _))
    }
}

/// Logs go to stderr as `\x01LEVEL\ttarget\tmessage` lines; the parent
/// re-logs them at the same level.
struct StderrLogger;

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let message = record.args().to_string();
        let mut stderr = io::stderr().lock();
        for line in message.lines() {
            let _ = writeln!(
                stderr,
                "{LOG_LINE_PREFIX}{}\t{}\t{}",
                record.level(),
                record.target(),
                line
            );
        }
    }

    fn flush(&self) {
        let _ = io::stderr().flush();
    }
}

fn init_logger() {
    static LOGGER: StderrLogger = StderrLogger;
    let level = std::env::var(LOG_LEVEL_ENV)
        .ok()
        .and_then(|v| v.parse::<LevelFilter>().ok())
        .unwrap_or(LevelFilter::Info);
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(level);
    }
}

/// Load the language model and its multimodal projection.
fn load(
    backend: &'static LlamaBackend,
    model_path: &Path,
    mmproj_path: &Path,
    gpu_layers: u32,
) -> Result<Loaded, String> {
    let started = std::time::Instant::now();
    let model_params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);
    let model = LlamaModel::load_from_file(backend, model_path, &model_params)
        .map_err(|e| format!("failed to load the vision model: {e}"))?;

    let media_marker = CString::new(MEDIA_MARKER).expect("marker has no null bytes");
    let mtmd_params = MtmdContextParams {
        use_gpu: gpu_layers > 0,
        print_timings: false,
        n_threads: worker_threads(),
        media_marker,
        image_min_tokens: IMAGE_MIN_TOKENS,
        image_max_tokens: IMAGE_MAX_TOKENS,
    };
    let mmproj = mmproj_path
        .to_str()
        .ok_or_else(|| "mmproj path is not UTF-8".to_string())?;
    let projection = MtmdContext::init_from_file(mmproj, &model, &mtmd_params)
        .map_err(|e| format!("failed to load the multimodal projection: {e}"))?;
    if !projection.support_vision() {
        return Err("the multimodal projection does not support vision".to_string());
    }
    let template = model
        .chat_template(None)
        .map_err(|e| format!("the model carries no chat template: {e}"))?;

    info!(
        "Vision model loaded in {:?} ({} GPU layers)",
        started.elapsed(),
        gpu_layers
    );
    Ok(Loaded {
        backend,
        model,
        projection,
        template,
    })
}

fn worker_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
}

/// Ground one transcript against one frame: encode the frame, prefill the
/// chat, and generate the rewritten transcript.
fn ground(
    state: &mut Loaded,
    sampler: &mut LlamaSampler,
    options: &GroundOptions,
    rgba: &[u8],
) -> Result<String, String> {
    let started = std::time::Instant::now();
    let expected = options.width as usize * options.height as usize * 4;
    if rgba.len() != expected {
        return Err(format!(
            "frame is {} bytes; {}x{} RGBA is {}",
            rgba.len(),
            options.width,
            options.height,
            expected
        ));
    }
    let rgb = rgba_to_rgb(rgba);

    let bitmap = MtmdBitmap::from_image_data(options.width, options.height, &rgb)
        .map_err(|e| format!("frame rejected by the vision encoder: {e:?}"))?;

    // A fresh context per grounding: no KV cache bookkeeping between
    // dictations, and a failed pass leaves nothing behind.
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(CONTEXT_TOKENS))
        .with_n_threads(worker_threads())
        // See BATCH_TOKENS: at least the image token budget, or the mtmd
        // helper cannot hold one image slice in its decode batch.
        .with_n_batch(IMAGE_MAX_TOKENS as u32);
    let mut context = state
        .model
        .new_context(state.backend, ctx_params)
        .map_err(|e| format!("failed to create the vision context: {e}"))?;

    let system = LlamaChatMessage::new("system".into(), GROUNDING_SYSTEM_PROMPT.into())
        .map_err(|e| format!("system message: {e:?}"))?;
    let user = LlamaChatMessage::new(
        "user".into(),
        super::protocol::grounding_user_message(&options.transcript),
    )
    .map_err(|e| format!("user message: {e:?}"))?;
    let formatted = state
        .model
        .apply_chat_template(&state.template, &[system, user], true)
        .map_err(|e| format!("failed to apply the chat template: {e}"))?;

    let input = MtmdInputText {
        text: formatted,
        add_special: true,
        parse_special: true,
    };
    let chunks = state
        .projection
        .tokenize(input, &[&bitmap])
        .map_err(|e| format!("failed to tokenize the grounding prompt: {e:?}"))?;
    drop(bitmap);

    let n_past = chunks
        .eval_chunks(
            &mut state.projection,
            &mut context,
            0,
            0,
            BATCH_TOKENS as i32,
            true,
        )
        .map_err(|e| format!("vision encoding failed: {e:?}"))?;
    drop(chunks);

    let mut batch = LlamaBatch::new(BATCH_TOKENS, 1);
    let vocab = state.model.vocab();
    let mut text = Vec::new();
    // `n_past` is the position of the next token; the loop counter offsets it.
    let position_base = n_past;
    for step in 0..options.max_tokens {
        let token = sampler.sample(&context, -1);
        sampler.accept(token);
        if vocab.is_eog(token) {
            break;
        }
        text.extend_from_slice(&vocab.token_to_piece(token, true, None));
        batch.clear();
        batch
            .add(token, position_base + step as i32, &[0], true)
            .map_err(|e| format!("batch: {e:?}"))?;
        context
            .decode(&mut batch)
            .map_err(|e| format!("generation failed: {e:?}"))?;
    }

    let grounded = String::from_utf8_lossy(&text).trim().to_string();
    info!(
        "Grounded a {}x{} frame with {} transcript characters in {:?}",
        options.width,
        options.height,
        options.transcript.len(),
        started.elapsed()
    );
    Ok(grounded)
}

/// RGBA (what a screen gives) to RGB (what mtmd bitmaps take). Screens are
/// opaque, so the alpha byte is simply dropped.
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for pixel in rgba.as_chunks::<4>().0 {
        rgb.extend_from_slice(&pixel[..3]);
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_media_marker_matches_the_default() {
        // The parent embeds MEDIA_MARKER in the prompt text; the worker
        // configures the mtmd context with the same string. llama.cpp's own
        // default marker exists so the two can never silently drift.
        assert_eq!(MEDIA_MARKER, llama_cpp_2::mtmd::mtmd_default_marker());
    }

    #[test]
    fn rgba_becomes_rgb_by_dropping_alpha() {
        assert_eq!(
            rgba_to_rgb(&[1, 2, 3, 4, 5, 6, 7, 8]),
            vec![1, 2, 3, 5, 6, 7]
        );
    }

    /// End-to-end against the real pinned model: load the language model and
    /// its projection, then ground a transcript against a synthetic frame
    /// (no subprocess — this exercises the llama.cpp/mtmd path in-process).
    ///
    /// Files are pointed at by the harness (they are the multi-gigabyte
    /// pinned downloads), so the test is ignored by default. Run with:
    /// `HANDY_VLM_MODEL=<q4 gguf> HANDY_VLM_MMPROJ=<mmproj gguf> cargo test
    ///  --lib worker -- --ignored --nocapture`
    #[test]
    #[ignore = "needs the real multi-gigabyte model files"]
    fn grounds_a_real_transcript_against_a_real_frame() {
        let model_path = std::env::var_os("HANDY_VLM_MODEL")
            .expect("set HANDY_VLM_MODEL to the pinned Qwen3-VL gguf");
        let mmproj_path = std::env::var_os("HANDY_VLM_MMPROJ")
            .expect("set HANDY_VLM_MMPROJ to the pinned mmproj gguf");

        let backend = Box::leak(Box::new(
            LlamaBackend::init().expect("llama.cpp backends init"),
        ));
        let mut loaded = super::load(
            backend,
            model_path.as_ref(),
            mmproj_path.as_ref(),
            // CPU: the test machines may share their GPU with the app.
            0,
        )
        .expect("load the pinned vision model");

        // A small synthetic "screenshot": distinct color quadrants, the
        // bottom-right one white with dark horizontal bars like lines of
        // text. The transcript asks about the quadrants so the model has
        // something concrete to resolve.
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

        let options = super::super::protocol::GroundOptions::new(
            width,
            height,
            "the red box is in the top left corner and the blue one is in the \
             top right corner, name both"
                .to_string(),
        );
        let mut sampler = LlamaSampler::chain_simple([LlamaSampler::greedy()]);
        let grounded = super::ground(&mut loaded, &mut sampler, &options, &rgba)
            .expect("ground a transcript against the frame");
        eprintln!("grounded: {grounded}");
        assert!(
            !grounded.trim().is_empty(),
            "grounding must produce text, got nothing"
        );
    }

    /// The quality bar for real use: ground a transcript against a real,
    /// text-bearing screenshot (committed at `tests/fixtures/`) and require
    /// the model to read the exact on-screen text it refers to. Ignored for
    /// the same reason as the test above — it needs the real model files.
    ///
    /// Run with:
    /// `HANDY_VLM_MODEL=<q4 gguf> HANDY_VLM_MMPROJ=<mmproj gguf> cargo test
    ///  --lib worker -- --ignored --nocapture`
    #[test]
    #[ignore = "needs the real multi-gigabyte model files"]
    fn grounding_reads_the_actual_on_screen_text() {
        let model_path = std::env::var_os("HANDY_VLM_MODEL")
            .expect("set HANDY_VLM_MODEL to the pinned Qwen3-VL gguf");
        let mmproj_path = std::env::var_os("HANDY_VLM_MMPROJ")
            .expect("set HANDY_VLM_MMPROJ to the pinned mmproj gguf");

        // The committed fixture: a rendered "build settings" window with an
        // error banner naming build 1437 and a green button labeled
        // "Deploy to Production".
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("grounding-fixture.png");
        let image = image::open(&fixture).expect("open the committed fixture");
        let rgba_image = image.to_rgba8();
        let (width, height) = rgba_image.dimensions();
        let rgba = rgba_image.into_raw();

        let backend = Box::leak(Box::new(
            LlamaBackend::init().expect("llama.cpp backends init"),
        ));
        let mut loaded = super::load(
            backend,
            model_path.as_ref(),
            mmproj_path.as_ref(),
            // CPU: the test machines may share their GPU with the app.
            0,
        )
        .expect("load the pinned vision model");

        let options = super::super::protocol::GroundOptions::new(
            width,
            height,
            "the build failed again, tell me exactly what the error message \
             at the top says and what the green button on the right is labeled"
                .to_string(),
        );
        let mut sampler = LlamaSampler::chain_simple([LlamaSampler::greedy()]);
        let grounded = super::ground(&mut loaded, &mut sampler, &options, &rgba)
            .expect("ground a transcript against the fixture");
        eprintln!("grounded: {grounded}");

        let lowered = grounded.to_lowercase();
        assert!(
            lowered.contains("1437"),
            "the grounded text must read the error's build number, got: {grounded}"
        );
        assert!(
            lowered.contains("deploy to production"),
            "the grounded text must name the button's actual label, got: {grounded}"
        );
    }
}
