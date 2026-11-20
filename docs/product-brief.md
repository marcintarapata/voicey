# Voicey product brief

Pre-implementation seed. Captures the stated product direction before the design
session; decisions recorded during the design session supersede any open
question below.

## Base project

- **Handy** (MIT License, Copyright (c) 2025 CJ Pais): offline speech-to-text
  desktop app. Upstream: https://github.com/cjpais/Handy.
- Observed stack (to be verified in depth during examination): Tauri 2 (Rust
  backend), Vite + TypeScript + Tailwind frontend, Silero VAD, Whisper and
  Parakeet V3 transcription models, model downloads managed via the
  `@handy-computer/models` package.
- MIT licensing permits reuse of any part of the base.

## Product direction

Voicey is a personal tool for use at work, rebuilt from the Handy base with a
decidedly different character:

1. **Sleek, minimal-configuration product.** Very few exposed settings: most
   behavior is decided and built as the default, so the user rarely opens a
   settings screen at all.
2. **Sleek visual design.** A black/grayish, high-readability UI — mature and
   understated rather than toy-like. The current Handy look is the negative
   reference.
3. **Robust transcript capture.** Dictation captured and transcribed as-is,
   preserving what was actually said.
4. **Parakeet only for dictation.** Parakeet V3 (the base's CPU-optimized
   dictation model with automatic language detection) is the single model of
   interest for transcription.
5. **Screen-context pipeline extension.** Beyond dictation, capture screen
   content and run it through a small local vision language model (~4B
   parameters) so spoken references to on-screen material can be grounded.
6. **One self-contained package.** Everything ships in one installable
   package; models download automatically on first run and are stored inside
   the project/application directory rather than scattered caches.

## Open questions (design session)

To be settled before implementation, among others:

- Target platform priority (Windows first, given daily use at work?).
- Packaging and windowing approach: stay on Tauri or move to another stack.
- Which ~4B local vision model for screen content, and its download source.
- How screen capture is triggered (continuous, hotkey, or alongside
  dictation) and how much screen context is retained.
- Exact transcript output behavior (paste target, history, formatting).
- What minimal set of settings, if any, remains exposed.
