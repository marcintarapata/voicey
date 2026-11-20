# Voicey

Voicey is a privacy-first Windows dictation tool. Hold a shortcut, speak,
release — your words land verbatim at the cursor. It lives in the tray, runs
entirely on your own machine, and keeps every model, setting, and piece of
history inside its own folder.

**Two bindings:**

- **Dictation** (default `Ctrl+Alt+Space`) — hold to talk, release to paste.
  Tap-tap also works (hold-or-toggle); `Escape` cancels. The default avoids
  `Ctrl+Space`, which several Windows input methods (Microsoft Pinyin among
  them) use to toggle the IME, and `Alt+Space`, which opens a window's system
  menu. The shortcut is user-settable in Settings.
- **Grounded dictation** (default `Ctrl+Alt+Shift+Space`) — the same
  dictation, plus one frame of your screen captured at release and resolved
  by a local vision model (Qwen3-VL 4B): "paste the error code at the top"
  pastes the actual text. The frame stays in memory for those seconds and
  is never stored; if capture fails (an elevated or DRM-protected window),
  the dictation still pastes verbatim and the grounding pass quietly skips.

**Two settings — that's all:** the dictation shortcut and the microphone.
Everything else is a settled default: one dictation model (Parakeet TDT
0.6B v3) auto-downloaded on first run, voice-activity detection on, text
pasted exactly as spoken (with a trailing space), the fixed dark theme.
`Start with Windows` is available from the tray menu and off by default.
Every one of those defaults lives in `Data/config.toml` (see
[Configuration](#configuration-datconfigtoml)) — hand-editable, never shown
as UI.

Voicey is built on [Handy](https://github.com/cjpais/Handy) (MIT, Copyright
(c) 2025 CJ Pais), reusing its Rust/Tauri backend (transcribe worker, model
manager, paste transaction, overlays, history) with a rebranded, stripped-down
frontend. Third-party and model licenses: [NOTICE.md](NOTICE.md). Product
direction: [docs/product-brief.md](docs/product-brief.md).

## Enterprise-friendly: local, and staying that way

Voicey is built for environments where data must not leave the machine.

- **Zero telemetry.** No analytics, no crash reporting, no accounts, no
  phone-home of any kind. There is no telemetry code to turn off — the app
  simply has none.
- **No external services — ever.** Transcription and screen grounding run
  local models on your own hardware; there is no API-key option anywhere in
  the product, because there is no code path that could use one. The only
  network activity in Voicey's life is the one-time first-run model download
  (straight from Hugging Face, verified against a recorded SHA-256). After
  that, the machine can be offline forever.
- **Screen frames live in memory only.** The grounded binding captures one
  frame at release, resolves the reference, pastes the text, and drops the
  pixels. Frames are never written to disk. The dev-only capture switch
  (`dev.grounding_trace`) is compiled out of release builds entirely — a
  release binary cannot write a trace no matter what the config file says.
- **Everything in one folder.** Models, settings, and history live in `Data/`
  beside the executable (portable layout). Delete the folder and Voicey is
  gone; back it up and you have everything.
- **This is a design commitment, not a setting.** The defaults described
  below cannot drift toward the cloud, because the code that would have to
  drift isn't there.

## Install (portable zip)

1. Unzip `Voicey-portable-<version>.zip` anywhere — no admin rights needed.
2. Run `Voicey.exe`. The first run downloads the two models into the
   `Data/` folder beside the executable: Parakeet (~740 MB) and the screen
   vision model (~3.1 GB). One-time; nothing is installed anywhere else.

## Updating

The zip contains only program files — it never contains a `Data/` folder —
so an update is a folder replace:

1. Quit Voicey (tray → Quit).
2. Extract the new `Voicey-portable-<version>.zip` **over your existing
   Voicey folder** (or extract to a fresh folder and copy your old `Data/`
   into it).
3. Run `Voicey.exe` again. Your shortcut, microphone, downloaded models,
   and history are exactly where they were.

Do not delete `Data/` — it is the only copy of your settings, models, and
history.

## Where everything lives

```
Voicey/
├── Voicey.exe
├── LICENSE, NOTICE.md
└── Data/                    # created on first run; everything lives here
    ├── settings_store.json # the two settings (shortcut, microphone)
    ├── config.toml         # every other default, hand-editable (see below)
    ├── models/              # model files (mirror-fallback downloads, manual drop-ins)
    ├── huggingface/hub/     # first-run model downloads land here
    ├── history.db           # local transcription history (text only — never audio)
    ├── logs/
    └── webview/             # WebView UI cache
```

Dictation audio is never written to disk. History keeps text only, and the
grounded binding keeps the resolved text — never pixels.

## Configuration (`Data/config.toml`)

The app creates this file on first run and never rewrites it — edit it
freely, then start a new dictation (or restart Voicey) for changes to apply.
Unknown keys are ignored and absent keys keep the built-in default, so a
file from an older or newer version still works. A shortcut or microphone
chosen in Settings always wins; reset it there to hand control back to the
file.

| Key                      | Default                                 | What it does                                                                                                                                                                                                                                                                                                                                                |
| ------------------------ | --------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `overlay.style`          | `"live"`                                | `"live"`: show the transcript as it forms while you speak — the recording pill until the first text lands, then the preview panel. `"minimal"`: the pill only. `"none"`: no overlay.                                                                                                                                                                        |
| `overlay.position`       | `"bottom"`                              | Where the overlay sits: `"top"` or `"bottom"`.                                                                                                                                                                                                                                                                                                              |
| `paste.trailing_space`   | `true`                                  | Append a trailing space after pasted text so the cursor lands ready to type.                                                                                                                                                                                                                                                                                |
| `paste.delay_ms`         | `60`                                    | Milliseconds to wait after the shortcut before the paste key fires.                                                                                                                                                                                                                                                                                         |
| `paste.restore_delay_ms` | `60` (Linux: `500`)                     | Milliseconds to wait after pasting before your previous clipboard content is restored.                                                                                                                                                                                                                                                                      |
| `paste.auto_submit`      | `false`                                 | Press Enter after pasting (e.g. to send the message you dictated).                                                                                                                                                                                                                                                                                          |
| `vad.enabled`            | `true`                                  | Drop silence from recordings.                                                                                                                                                                                                                                                                                                                               |
| `vad.chunking`           | `true`                                  | Cut long dictation at pauses and decode each chunk while you keep speaking — what lets the live preview show text mid-speech.                                                                                                                                                                                                                               |
| `binding.dictation`      | `"ctrl+alt+space"` (per-platform)       | The default dictation shortcut. A shortcut changed in Settings wins.                                                                                                                                                                                                                                                                                        |
| `binding.grounded`       | `"ctrl+alt+shift+space"` (per-platform) | The default grounded-dictation shortcut, same precedence rule.                                                                                                                                                                                                                                                                                              |
| `audio.microphone`       | `"default"`                             | The default microphone: `"default"` (the system one) or a device name as shown in Settings. A microphone chosen in Settings wins.                                                                                                                                                                                                                           |
| `dev.grounding_trace`    | `true`                                  | **Dev builds only.** Record one trace folder per grounded dictation under `Data/logs/grounding/`: `screenshot.png`, and `trace.json` with the raw transcript, the exact vision-model prompt, the model's extracted reading, and the final pasted output. Release builds compile the capture code out entirely and cannot write one whatever this file says. |

## Development

Requires [Rust](https://rustup.rs/) and [Bun](https://bun.sh/); see
[BUILD.md](BUILD.md) for platform setup, [AGENTS.md](AGENTS.md) for the
architecture map.

```bash
bun install
bun run tauri dev
```

Building the Windows app from WSL: `scripts/winbuild.sh [--smoke]` stages the
checkout to a Windows temp dir through explicit WSL interop, builds it
there, and copies the artifacts back.
