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

Voicey is built on [Handy](https://github.com/cjpais/Handy) (MIT, Copyright
(c) 2025 CJ Pais), reusing its Rust/Tauri backend (transcribe worker, model
manager, paste transaction, overlays, history) with a rebranded, stripped-down
frontend. Third-party and model licenses: [NOTICE.md](NOTICE.md). Product
direction: [docs/product-brief.md](docs/product-brief.md).

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
    ├── models/              # model files (mirror-fallback downloads, manual drop-ins)
    ├── huggingface/hub/     # first-run model downloads land here
    ├── history.db           # local transcription history (text only — never audio)
    ├── logs/
    └── webview/             # WebView UI cache
```

Dictation audio is never written to disk. History keeps text only, and the
grounded binding keeps the resolved text — never pixels.

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
