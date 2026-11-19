//! Portable (single-folder) storage — the only mode.
//!
//! Voicey keeps every piece of user data next to the executable in a `Data/`
//! directory: settings store, `models/`, `history.db`, `logs/`, the Hugging
//! Face download cache, and the WebView UI cache. There is no installer-mode
//! profile directory and no marker file to opt in: storage lands beside the
//! app unconditionally, so the shipped folder can be unzipped anywhere and
//! run without admin rights, and replacing the binaries while keeping `Data/`
//! is the whole update procedure.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::Manager;

static PORTABLE_DATA_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
static PREVIOUS_HF_HOME: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Activate single-folder storage. Must be called once at startup before
/// Tauri initializes.
///
/// Always succeeds in practice; the `None` fallback (standard profile
/// directories) only triggers if the executable's directory cannot be
/// resolved or `Data/` cannot be created — e.g. a read-only install location.
pub fn init() {
    let previous_hf_home = std::env::var_os("HF_HOME").map(PathBuf::from);
    let _ = PREVIOUS_HF_HOME.set(previous_hf_home);

    PORTABLE_DATA_DIR.get_or_init(|| {
        let exe_path = std::env::current_exe().ok()?;
        let exe_dir = exe_path.parent()?;
        let data_dir = exe_dir.join("Data");

        std::fs::create_dir_all(&data_dir).ok()?;

        let hf_home = hugging_face_home(&data_dir);
        std::env::set_var("HF_HOME", &hf_home);
        eprintln!("[portable] data dir: {}", data_dir.display());
        eprintln!("[portable] Hugging Face home: {}", hf_home.display());
        Some(data_dir)
    });
}

/// Return the Hugging Face home configured before storage redirected it.
///
/// If the environment already pointed `HF_HOME` somewhere, the model manager
/// can recognize existing downloads there without copying multi-gigabyte
/// files. New downloads never go here, but deleting a model removes its
/// copies here too, even though the cache may be shared with other
/// applications.
pub fn previous_hf_home() -> Option<&'static PathBuf> {
    PREVIOUS_HF_HOME.get().and_then(|path| path.as_ref())
}

/// Keep hf-hub downloads inside the data directory. hf-hub appends its own
/// `hub` component to `HF_HOME` for model snapshots and blobs.
fn hugging_face_home(data_dir: &Path) -> PathBuf {
    data_dir.join("huggingface")
}

/// Returns `true` when single-folder storage is active.
///
/// Always `true` unless `Data/` beside the executable could not be created;
/// kept as a query so call sites can degrade gracefully on that fallback.
pub fn is_portable() -> bool {
    PORTABLE_DATA_DIR.get().and_then(|v| v.as_ref()).is_some()
}

/// Get the data dir beside the executable (if active). Does not require an
/// AppHandle. Returns `None` only on the read-only-install fallback.
pub fn data_dir() -> Option<&'static PathBuf> {
    PORTABLE_DATA_DIR.get().and_then(|v| v.as_ref())
}

/// Storage-aware replacement for `app.path().app_data_dir()`.
///
/// Resolves to `Data/` beside the executable; falls back to Tauri's profile
/// directory only if that could not be created.
pub fn app_data_dir(app: &tauri::AppHandle) -> Result<PathBuf, tauri::Error> {
    if let Some(dir) = data_dir() {
        Ok(dir.clone())
    } else {
        app.path().app_data_dir()
    }
}

/// Storage-aware replacement for `app.path().app_log_dir()`.
pub fn app_log_dir(app: &tauri::AppHandle) -> Result<PathBuf, tauri::Error> {
    if let Some(dir) = data_dir() {
        Ok(dir.join("logs"))
    } else {
        app.path().app_log_dir()
    }
}

/// Resolve a relative path against the data directory (storage-aware).
/// Replaces `app.path().resolve(path, BaseDirectory::AppData)`.
pub fn resolve_app_data(app: &tauri::AppHandle, relative: &str) -> Result<PathBuf, tauri::Error> {
    Ok(app_data_dir(app)?.join(relative))
}

/// Get the path to use with `tauri-plugin-store`.
/// Returns an absolute path (so the store plugin writes into the app folder)
/// or the original relative path on the read-only-install fallback.
pub fn store_path(relative: &str) -> PathBuf {
    if let Some(dir) = data_dir() {
        dir.join(relative)
    } else {
        PathBuf::from(relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hugging_face_home_is_inside_data_dir() {
        let data_dir = Path::new("app-root").join("Data");

        assert_eq!(hugging_face_home(&data_dir), data_dir.join("huggingface"));
    }

    #[test]
    fn test_init_activates_storage_beside_the_executable() {
        // init() is process-global; call it exactly once across the test
        // binary and assert the layout: a Data/ dir beside the test exe.
        init();

        assert!(is_portable());
        let data_dir = data_dir().expect("data dir must resolve after init");
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .expect("test exe must have a parent dir");
        assert_eq!(data_dir.as_path(), exe_dir.join("Data"));
        assert!(data_dir.join("huggingface").starts_with(data_dir));
        // HF_HOME must point inside the data dir.
        assert_eq!(
            std::env::var_os("HF_HOME"),
            Some(data_dir.join("huggingface").into_os_string())
        );

        // Leave the test target directory clean.
        std::fs::remove_dir_all(data_dir).ok();
    }
}
