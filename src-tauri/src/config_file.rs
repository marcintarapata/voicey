//! The Voicey config file: every opinion-shaped default in one place.
//!
//! Voicey's settings surface is exactly two controls (the dictation shortcut
//! and the microphone); everything else the app does is a settled default.
//! This module carries those defaults in `Data/config.toml` — a small,
//! hand-editable file the app creates on first run and never rewrites. The
//! file is the tuning surface for deliberate changes (a future companion
//! automation skill edits it); the app never mutates it after creation.
//!
//! Precedence, per key:
//!
//! 1. The two UI settings — a shortcut or microphone chosen in Settings —
//!    always win over this file.
//! 2. A key present in the file wins over the compiled default.
//! 3. An absent key falls back to the compiled default (which can be
//!    platform-specific, e.g. paste delays).
//!
//! A file that fails to parse is logged and ignored — the app runs on the
//! compiled defaults rather than failing to start, and the user's file is
//! never touched.

use std::path::Path;

use log::{debug, warn};
use serde::Deserialize;

use crate::settings::{self, AppSettings};

/// The config file's name inside the portable `Data/` folder.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Where the config file lives: `Data/config.toml` in portable mode, the
/// app-data folder otherwise (mirrors `settings::SETTINGS_STORE_PATH`).
pub fn config_path() -> std::path::PathBuf {
    crate::portable::store_path(CONFIG_FILE_NAME)
}

/// Every key in the file, in TOML shape. Every field is `Option`: only a key
/// actually present in the file overrides anything.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VoiceyConfig {
    pub overlay: OverlayConfig,
    pub paste: PasteConfig,
    pub vad: VadConfig,
    pub binding: BindingConfig,
    pub audio: AudioConfig,
    pub dev: DevConfig,
}

/// The recording overlay's shape and placement.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    /// `"live"` (transcript preview while speaking), `"minimal"` (pill only)
    /// or `"none"` (no overlay).
    pub style: Option<settings::OverlayStyle>,
    /// `"top"` or `"bottom"`.
    pub position: Option<settings::OverlayPosition>,
    /// How often, in milliseconds, the live preview re-decodes the audio that
    /// is not yet committed to a chunk, so text appears while speech is still
    /// going. Lower is snappier but spends more CPU re-running the model.
    pub preview_interval_ms: Option<u64>,
}

/// How dictated text is delivered to the focused app.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PasteConfig {
    /// Append a trailing space so the cursor lands ready to type.
    pub trailing_space: Option<bool>,
    /// Milliseconds to wait after the shortcut before the paste key fires.
    pub delay_ms: Option<u64>,
    /// Milliseconds to wait after pasting before the previous clipboard
    /// content is restored.
    pub restore_delay_ms: Option<u64>,
    /// Press Enter after pasting (e.g. to send a chat message).
    pub auto_submit: Option<bool>,
}

/// Voice-activity detection and long-form chunked decoding.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VadConfig {
    /// Drop silence from recordings.
    pub enabled: Option<bool>,
    /// Cut long dictation at pauses and decode each chunk as it becomes
    /// certain — what makes the live preview able to show text mid-speech.
    pub chunking: Option<bool>,
}

/// Default shortcut bindings. A binding changed in Settings wins; resetting
/// it there (back to its default) hands control back to this file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BindingConfig {
    /// The dictation binding, e.g. `"ctrl+alt+space"`.
    pub dictation: Option<String>,
    /// The grounded dictation binding, e.g. `"ctrl+alt+shift+space"`.
    pub grounded: Option<String>,
}

/// The default microphone.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// `"default"` (the system microphone) or a device name as shown in
    /// Settings. A microphone chosen in Settings wins.
    pub microphone: Option<String>,
}

/// Dev-build-only switches. Release builds compile the features these gate
/// out entirely (the D10 privacy rule); the keys are inert there.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DevConfig {
    /// Dev builds: record a grounding trace (screenshot + prompts + outputs)
    /// per grounded dictation under `Data/logs/grounding/`.
    pub grounding_trace: Option<bool>,
}

/// Loads the config file, creating it with documented defaults on first run.
/// A missing, unreadable or unparseable file yields the empty config (every
/// key absent → compiled defaults apply).
pub fn load_or_create() -> VoiceyConfig {
    let path = config_path();
    if !path.exists() {
        if let Err(e) = write_default_file(&path) {
            warn!(
                "Could not create the default config file at {}: {e}",
                path.display()
            );
        }
        return VoiceyConfig::default();
    }
    load_from(&path)
}

/// Parses an existing config file. Never writes.
pub fn load_from(path: &Path) -> VoiceyConfig {
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<VoiceyConfig>(&text) {
            Ok(config) => config,
            Err(e) => {
                warn!(
                    "Config file {} is not valid TOML for Voicey ({e}); \
                     running on compiled defaults until it is fixed",
                    path.display()
                );
                VoiceyConfig::default()
            }
        },
        Err(e) => {
            warn!("Could not read the config file at {}: {e}", path.display());
            VoiceyConfig::default()
        }
    }
}

/// Applies the config's present keys onto loaded settings. Keys the user
/// owns through the UI (a shortcut or microphone they actually changed)
/// keep their stored value; see the module docs for the precedence.
pub fn apply_to(settings: &mut AppSettings, config: &VoiceyConfig) {
    if let Some(style) = config.overlay.style {
        settings.overlay_style = style;
    }
    if let Some(position) = config.overlay.position {
        settings.overlay_position = position;
    }
    if let Some(interval) = config.overlay.preview_interval_ms {
        settings.live_preview_interval_ms = interval;
    }
    if let Some(trailing_space) = config.paste.trailing_space {
        settings.append_trailing_space = trailing_space;
    }
    if let Some(delay_ms) = config.paste.delay_ms {
        settings.paste_delay_ms = delay_ms;
    }
    if let Some(restore_delay_ms) = config.paste.restore_delay_ms {
        settings.paste_delay_after_ms = restore_delay_ms;
    }
    if let Some(auto_submit) = config.paste.auto_submit {
        settings.auto_submit = auto_submit;
    }
    if let Some(enabled) = config.vad.enabled {
        settings.vad_enabled = enabled;
    }
    if let Some(chunking) = config.vad.chunking {
        settings.long_form_chunking = chunking;
    }
    if let Some(microphone) = &config.audio.microphone {
        // None in the store means the user never picked one in Settings;
        // the config file supplies the default device instead.
        if settings.selected_microphone.is_none() && microphone != "default" {
            settings.selected_microphone = Some(microphone.clone());
        }
    }
    apply_binding_default(settings, "transcribe", &config.binding.dictation);
    apply_binding_default(settings, "transcribe_grounded", &config.binding.grounded);
}

/// Removes values this file owns from a settings snapshot before it is
/// persisted, so the store never records a config-provided value as if the
/// user had chosen it in Settings. Without this, one Settings write (say,
/// changing the microphone) would snapshot the config's shortcut into the
/// store, and a later edit of the file would then look like a user choice
/// and stop applying. Reads re-apply the config on top of the compiled
/// defaults, so stripping converges back to the file's value.
///
/// Only the two UI-owned values need this: the scalar keys (overlay, paste,
/// VAD) have no Settings surface, so the file always wins over any stale
/// snapshot of them anyway.
pub fn strip_config_owned(settings: &mut AppSettings) {
    strip_config_owned_with(settings, &load_or_create());
}

/// `strip_config_owned` with an explicit config (the test seam).
fn strip_config_owned_with(settings: &mut AppSettings, config: &VoiceyConfig) {
    strip_binding_default(settings, "transcribe", config.binding.dictation.as_deref());
    strip_binding_default(
        settings,
        "transcribe_grounded",
        config.binding.grounded.as_deref(),
    );
    if let Some(microphone) = &config.audio.microphone {
        if microphone != "default" && settings.selected_microphone.as_deref() == Some(microphone) {
            settings.selected_microphone = None;
        }
    }
}

fn strip_binding_default(settings: &mut AppSettings, binding_id: &str, config_value: Option<&str>) {
    let Some(config_value) = config_value else {
        return;
    };
    let compiled = settings::get_default_settings();
    let Some(compiled_binding) = compiled.bindings.get(binding_id) else {
        return;
    };
    if let Some(binding) = settings.bindings.get_mut(binding_id) {
        if binding.current_binding == config_value {
            binding.default_binding = compiled_binding.default_binding.clone();
            binding.current_binding = compiled_binding.current_binding.clone();
        }
    }
}

/// Replaces a binding's default — and its current binding, but only when
/// the current binding is still the compiled default (the user never
/// customized it in Settings). A stored custom shortcut always wins.
fn apply_binding_default(settings: &mut AppSettings, binding_id: &str, default: &Option<String>) {
    let Some(default) = default else { return };
    let compiled = settings::get_default_settings();
    let Some(compiled_binding) = compiled.bindings.get(binding_id) else {
        return;
    };
    if let Some(binding) = settings.bindings.get_mut(binding_id) {
        if binding.current_binding == compiled_binding.current_binding {
            binding.default_binding = default.clone();
            binding.current_binding = default.clone();
            debug!(
                "Config file sets the '{}' binding default to '{default}'",
                binding_id
            );
        }
    }
}

/// Writes the documented default config, embedding this platform's defaults
/// so the file is a truthful starting point.
fn write_default_file(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, default_file_contents())
}

/// The default file's text. Every platform-specific value (default
/// shortcuts, paste delays) is taken from `get_default_settings()` so the
/// written file always matches what the app would actually do.
fn default_file_contents() -> String {
    let defaults = settings::get_default_settings();
    let dictation = defaults
        .bindings
        .get("transcribe")
        .map(|b| b.default_binding.clone())
        .unwrap_or_default();
    let grounded = defaults
        .bindings
        .get("transcribe_grounded")
        .map(|b| b.default_binding.clone())
        .unwrap_or_default();
    format!(
        r#"# Voicey configuration.
#
# Every opinion-shaped default the app honors lives here. The app creates
# this file on first run and never rewrites it; edit it freely and restart
# Voicey (or start a new dictation) for changes to apply. Unknown keys and
# sections are ignored, and absent keys keep the app's built-in default.
#
# Precedence: the two Settings choices (dictation shortcut, microphone)
# always win over this file. Reset a shortcut or the microphone in Settings
# to hand control back to this file.

[overlay]
# "live": show the transcript as it forms while you speak — the recording
# pill until the first text lands, then the preview panel.
# "minimal": the recording pill only, no transcript preview.
# "none": no overlay at all.
style = "live"
# Where the overlay sits.
position = "bottom"
# How often (milliseconds) the live preview re-decodes what you have said
# so far, so text keeps appearing while you speak. Lower is snappier but
# spends more CPU re-running the model.
preview_interval_ms = {preview_interval}

[paste]
# Append a trailing space after pasted text so the cursor lands ready to type.
trailing_space = true
# Milliseconds to wait after the shortcut before the paste key fires.
delay_ms = {paste_delay}
# Milliseconds to wait after pasting before your previous clipboard content
# is restored.
restore_delay_ms = {paste_restore_delay}
# Press Enter after pasting (e.g. to send the message you dictated).
auto_submit = false

[vad]
# Drop silence from recordings.
enabled = true
# Cut long dictation at pauses and decode each chunk while you keep
# speaking — this is what lets the live preview show text mid-speech.
chunking = true

[binding]
# Default shortcuts. A shortcut changed in Settings wins over these.
dictation = "{dictation}"
grounded = "{grounded}"

[audio]
# The default microphone: "default" (the system one) or a device name as
# shown in Settings. A microphone chosen in Settings wins.
microphone = "default"

[dev]
# Dev builds only (the privacy rule is compile-time): record a grounding
# trace — screenshot, prompts, the vision model's reading, the final output —
# for every grounded dictation under Data/logs/grounding/. Release builds
# compile the capture code out entirely and can never write one, whatever
# this file says.
grounding_trace = true
"#,
        paste_delay = defaults.paste_delay_ms,
        paste_restore_delay = defaults.paste_delay_after_ms,
        preview_interval = defaults.live_preview_interval_ms,
        dictation = dictation,
        grounded = grounded,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::OverlayStyle;

    /// Round-trip: the default file this module writes must parse back into
    /// the config the app would act on, with every documented key present.
    #[test]
    fn the_default_file_parses_back() {
        let contents = default_file_contents();
        let config: VoiceyConfig = toml::from_str(&contents).expect("default file parses");
        assert_eq!(config.overlay.style, Some(OverlayStyle::Live));
        assert_eq!(
            config.overlay.preview_interval_ms,
            Some(crate::settings::get_default_settings().live_preview_interval_ms)
        );
        assert_eq!(config.paste.trailing_space, Some(true));
        assert_eq!(config.vad.enabled, Some(true));
        assert_eq!(config.vad.chunking, Some(true));
        assert_eq!(config.dev.grounding_trace, Some(true));
        assert!(config.binding.dictation.is_some());
        assert!(config.binding.grounded.is_some());
    }

    /// A partial file: only the keys present apply.
    #[test]
    fn a_partial_file_only_applies_present_keys() {
        let config: VoiceyConfig =
            toml::from_str("[paste]\ntrailing_space = false\n").expect("partial file parses");
        assert_eq!(config.paste.trailing_space, Some(false));
        assert_eq!(config.overlay.style, None);
        assert_eq!(config.vad.chunking, None);
    }

    /// Unknown keys and sections are ignored, so files stay forward- and
    /// backward-compatible.
    #[test]
    fn unknown_keys_are_ignored() {
        let config: VoiceyConfig = toml::from_str(
            "[overlay]\nstyle = \"none\"\nfuture_key = true\n[future_section]\nx = 1\n",
        )
        .expect("unknown keys do not fail the parse");
        assert_eq!(config.overlay.style, Some(OverlayStyle::None));
    }

    /// Every override lands on the settings it documents.
    #[test]
    fn apply_to_overrides_the_documented_settings() {
        let config: VoiceyConfig = toml::from_str(
            r#"
            [overlay]
            style = "none"
            position = "top"
            preview_interval_ms = 500
            [paste]
            trailing_space = false
            delay_ms = 120
            restore_delay_ms = 130
            auto_submit = true
            [vad]
            enabled = false
            chunking = false
            "#,
        )
        .expect("config parses");

        let mut settings = settings::get_default_settings();
        apply_to(&mut settings, &config);
        assert_eq!(settings.overlay_style, OverlayStyle::None);
        assert_eq!(settings.overlay_position, settings::OverlayPosition::Top);
        assert_eq!(settings.live_preview_interval_ms, 500);
        assert!(!settings.append_trailing_space);
        assert_eq!(settings.paste_delay_ms, 120);
        assert_eq!(settings.paste_delay_after_ms, 130);
        assert!(settings.auto_submit);
        assert!(!settings.vad_enabled);
        assert!(!settings.long_form_chunking);
    }

    /// The binding default applies while the user never customized the
    /// shortcut, and a stored custom shortcut wins.
    #[test]
    fn binding_defaults_respect_the_ui_choice() {
        let config: VoiceyConfig = toml::from_str(
            r#"
            [binding]
            dictation = "ctrl+shift+f12"
            "#,
        )
        .expect("config parses");

        // Untouched store: the compiled default is still current.
        let mut settings = settings::get_default_settings();
        apply_to(&mut settings, &config);
        let binding = settings.bindings.get("transcribe").unwrap();
        assert_eq!(binding.current_binding, "ctrl+shift+f12");
        assert_eq!(binding.default_binding, "ctrl+shift+f12");

        // A shortcut the user set in Settings keeps winning.
        let mut settings = settings::get_default_settings();
        settings
            .bindings
            .get_mut("transcribe")
            .unwrap()
            .current_binding = "ctrl+f9".to_string();
        apply_to(&mut settings, &config);
        let binding = settings.bindings.get("transcribe").unwrap();
        assert_eq!(binding.current_binding, "ctrl+f9");
    }

    /// The microphone default only fills a never-chosen device; a device
    /// chosen in Settings wins, and "default" means the system microphone.
    #[test]
    fn microphone_default_respects_the_ui_choice() {
        let config: VoiceyConfig = toml::from_str(
            r#"
            [audio]
            microphone = "Yeti USB Mic"
            "#,
        )
        .expect("config parses");

        let mut settings = settings::get_default_settings();
        apply_to(&mut settings, &config);
        assert_eq!(
            settings.selected_microphone.as_deref(),
            Some("Yeti USB Mic")
        );

        let mut settings = settings::get_default_settings();
        settings.selected_microphone = Some("Headset".to_string());
        apply_to(&mut settings, &config);
        assert_eq!(settings.selected_microphone.as_deref(), Some("Headset"));

        let config: VoiceyConfig =
            toml::from_str("[audio]\nmicrophone = \"default\"\n").expect("config parses");
        let mut settings = settings::get_default_settings();
        apply_to(&mut settings, &config);
        assert_eq!(settings.selected_microphone, None);
    }

    /// The strip keeps the store honest: a snapshot that merely mirrors the
    /// config's shortcut or microphone drops back to the compiled defaults
    /// (reads re-apply the config), while a genuine UI choice survives.
    #[test]
    fn strip_returns_config_mirrors_to_the_compiled_defaults() {
        let config: VoiceyConfig = toml::from_str(
            r#"
            [binding]
            dictation = "ctrl+shift+f12"
            [audio]
            microphone = "Yeti USB Mic"
            "#,
        )
        .expect("config parses");

        let mut settings = settings::get_default_settings();
        let binding = settings.bindings.get_mut("transcribe").unwrap();
        binding.current_binding = "ctrl+shift+f12".to_string();
        settings.selected_microphone = Some("Yeti USB Mic".to_string());
        strip_config_owned_with(&mut settings, &config);
        let binding = settings.bindings.get("transcribe").unwrap();
        assert_eq!(
            binding.current_binding,
            settings::get_default_settings().bindings["transcribe"].current_binding
        );
        assert_eq!(settings.selected_microphone, None);

        // A genuine UI choice is not stripped.
        let mut settings = settings::get_default_settings();
        settings
            .bindings
            .get_mut("transcribe")
            .unwrap()
            .current_binding = "ctrl+f9".to_string();
        settings.selected_microphone = Some("Headset".to_string());
        strip_config_owned_with(&mut settings, &config);
        assert_eq!(settings.bindings["transcribe"].current_binding, "ctrl+f9");
        assert_eq!(settings.selected_microphone.as_deref(), Some("Headset"));
    }

    /// A broken file is ignored, not fatal.
    #[test]
    fn a_broken_file_parses_to_no_overrides() {
        let dir = std::env::temp_dir().join("voicey-config-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("broken.toml");
        std::fs::write(&path, "this is [ not toml").expect("write");
        let config = load_from(&path);
        assert_eq!(config.overlay.style, None);
    }
}
