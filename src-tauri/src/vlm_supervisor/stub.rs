//! The vision supervisor on platforms where llama.cpp is not compiled in
//! (see the parent module's docs: static ggml would collide with the
//! transcription engine's). The API is identical so callers need no cfg;
//! every operation reports [`VlmError::Unsupported`], and the grounded
//! binding degrades to a verbatim paste with a notice.

use super::super::screen_capture::ScreenFrame;

pub struct VlmLoadSpec {
    #[allow(dead_code)]
    pub model_path: std::path::PathBuf,
    #[allow(dead_code)]
    pub mmproj_path: std::path::PathBuf,
    #[allow(dead_code)]
    pub gpu: bool,
}

pub enum VlmError {
    #[allow(dead_code)]
    Cancelled,
    Unsupported,
    #[allow(dead_code)]
    Failed(String),
}

impl std::fmt::Display for VlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VlmError::Cancelled => f.write_str("grounding was cancelled"),
            VlmError::Unsupported => f.write_str("screen grounding is not built for this platform"),
            VlmError::Failed(message) => f.write_str(message),
        }
    }
}

impl std::fmt::Debug for VlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_string())
    }
}

impl std::error::Error for VlmError {}

#[derive(Debug, Clone, Copy)]
pub struct CancelMark;

#[derive(Clone, Default)]
pub struct VlmSupervisor;

impl VlmSupervisor {
    pub fn new(_gpu_allowed: bool) -> Self {
        Self
    }

    pub fn load(&self, _spec: VlmLoadSpec) -> Result<LoadedInfo, VlmError> {
        Err(VlmError::Unsupported)
    }

    pub fn loaded(&self) -> Option<LoadedInfo> {
        None
    }

    pub fn ground(
        &self,
        _mark: CancelMark,
        _frame: &ScreenFrame,
        _transcript: String,
    ) -> Result<String, VlmError> {
        Err(VlmError::Unsupported)
    }

    pub fn cancel_mark(&self) -> CancelMark {
        CancelMark
    }

    pub fn cancel(&self) {}

    pub fn retry_gpu(&self, _reason: &str) {}

    pub fn unload(&self) {}
}

#[derive(Debug, Clone)]
pub struct LoadedInfo {
    pub on_gpu: bool,
}
