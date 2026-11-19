//! Process isolation for the screen-grounding vision model (llama.cpp).
//!
//! All llama.cpp native code (backend init, model load, vision encoding,
//! generation) runs in a child process: the Voicey executable re-launched
//! with [`WORKER_FLAG`]. A GPU driver fault, a `GGML_ASSERT` abort or a hang
//! then kills only the worker, and [`VlmSupervisor`] recovers — mirroring
//! [`crate::engine_supervisor`], which isolates the transcription engine the
//! same way.
//!
//! The screen frame travels to the worker as raw RGBA bytes in the same
//! framed shape the transcription engine uses (one JSON section + one binary
//! section). It lives in memory only: the parent's copy is dropped once the
//! request is written, and the worker's copies (bitmap, context) are dropped
//! before the response is even returned. Nothing is ever written to disk in
//! release builds (the dev-only telemetry that *can* persist captures is
//! compiled out of them; see `screen_capture`).
//!
//! Compiled only where the transcription engine also keeps its ggml out of
//! the executable (Windows x64, Linux): there, transcribe-cpp's ggml lives
//! in its own shared library, so this statically linked llama.cpp is the
//! executable's only one. On other platforms the supervisor is a stub whose
//! grounded paste degrades to verbatim with a notice.

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    target_os = "linux"
))]
mod protocol;
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    target_os = "linux"
))]
mod supervisor;
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    target_os = "linux"
))]
mod worker;

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    target_os = "linux"
))]
pub use protocol::GroundOptions;
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    target_os = "linux"
))]
pub use supervisor::{CancelMark, VlmError, VlmLoadSpec, VlmSupervisor};

#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    target_os = "linux"
)))]
mod stub;
#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    target_os = "linux"
)))]
pub use stub::{CancelMark, VlmError, VlmLoadSpec, VlmSupervisor};

/// Hidden first argument that turns the executable into the vision worker.
/// Checked in `main` before CLI parsing, Tauri, or single-instance handling.
pub const WORKER_FLAG: &str = "--vlm-worker";
/// Worker argument: keep every layer on the CPU (the post-GPU-fault retry).
pub const CPU_ONLY_FLAG: &str = "--vlm-cpu-only";
const LOG_LEVEL_ENV: &str = "HANDY_VLM_WORKER_LOG";

/// Whether this process was launched as the vision worker.
pub fn is_worker_invocation() -> bool {
    std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == WORKER_FLAG)
}

/// Run the worker until the parent closes its stdin. Returns the exit code.
/// Unsupported platforms never get here (the supervisor is a stub there),
/// but the dispatch stays total so `main` needs no cfg.
pub fn run_worker() -> i32 {
    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        target_os = "linux"
    ))]
    {
        worker::run()
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        target_os = "linux"
    )))]
    {
        eprintln!("vlm worker: this build does not include llama.cpp");
        2
    }
}
