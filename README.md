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
