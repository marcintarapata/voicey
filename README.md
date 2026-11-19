# Voicey

Voicey is a personal, privacy-first Windows dictation tool: press a shortcut,
speak, and your words land verbatim at the cursor. It lives in the tray, ships
one fixed dark theme, and keeps every model and setting on your own machine.

It is built on [Handy](https://github.com/cjpais/Handy) (MIT, Copyright (c)
2025 CJ Pais), reusing its Rust/Tauri backend (transcribe worker, model
manager, paste transaction, overlays, history) with a rebranded, stripped-down
frontend. See [docs/product-brief.md](docs/product-brief.md) for the product
direction and [LICENSE](LICENSE) + the About page for upstream attribution.

## Status

Phase 0 — rebrand & strip: Voicey identity (own app identifier, icons,
window/tray titles), mascot/onboarding/what's-new/sponsor/update surfaces
removed, English-only UI, near-black fixed dark theme with a single red
recording accent, minimal recording pill overlay. All 76 catalog models and
all settings remain functional.

Phase 1 — collapse to defaults: Parakeet v3 (`parakeet-tdt-0.6b-v3`, Q8_0)
pinned as the only dictation model, auto-downloaded on first run; the model
picker and every non-essential setting removed (exactly two controls remain:
shortcut + microphone).

Phase 2 — one-package portable mode: single-folder storage is the only mode
(see below) and the Windows build's primary artifact is the portable zip.

## Single-folder storage (the only mode)

Voicey keeps everything next to its executable — there is no profile-folder
mode and nothing is written to `%APPDATA%`. On startup the app creates a
`Data/` directory beside `Voicey.exe` (or beside the dev binary) and stores:

```
Data/
├── settings_store.json   # the two settings (shortcut, microphone)
├── models/                # model files (mirror-fallback downloads, manual drop-ins)
├── huggingface/hub/       # first-run Parakeet download lands here (HF_HOME)
├── history.db             # local transcription history
├── logs/
└── webview/               # WebView UI cache
```

Unzip the portable zip anywhere (no admin rights needed), run `Voicey.exe`,
and the first run downloads Parakeet straight into that `Data/` folder —
never into a user-profile cache.

### Updating without losing anything

The zip contains only program files — it never contains a `Data/` folder —
so an update is a folder replace:

1. Quit Voicey (tray → quit).
2. Extract the new `Voicey-portable-<version>.zip` **over your existing
   Voicey folder** (or extract it to a fresh folder and copy your old `Data/`
   into it).
3. Run `Voicey.exe` again. Your shortcut, microphone, downloaded models,
   and history are exactly where they were.

Do not delete `Data/`; it is the only copy of your settings, models, and
history.

## Development

Requires [Rust](https://rustup.rs/) and [Bun](https://bun.sh/); see
[BUILD.md](BUILD.md) for platform setup, [AGENTS.md](AGENTS.md) for the
architecture map.

```bash
bun install
bun run tauri dev
```

Building the Windows app from WSL: `scripts/winbuild.sh` stages the checkout
to a Windows temp dir through explicit WSL interop, builds it there, and
copies the artifacts back.
