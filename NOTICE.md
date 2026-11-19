# Voicey — third-party notices and licenses

Voicey is MIT-licensed software (see [LICENSE](LICENSE)). It is built on
[Handy](https://github.com/cjpais/Handy) by CJ Pais (MIT, Copyright (c) 2025
CJ Pais); this file records everything else that ships with, or is fetched
by, the app, with the license that covers each.

## AI models

Models are never bundled inside the app package except where noted; the two
dictation models are downloaded on first run into `Data/models` next to the
executable (see the README). Every file is fetched from its pinned source
repository and verified against a recorded SHA-256 before use.

| Model                                                                                                                                                          | License    | Source                                                                                                                                                         |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Parakeet TDT 0.6B v3 (NVIDIA), Q8_0 GGUF (`parakeet-tdt-0.6b-v3-Q8_0.gguf`, ~740 MB)                                                                           | CC-BY-4.0  | `handy-computer/parakeet-tdt-0.6b-v3-gguf` @ `85ac09ea12fc4b1112fa76810059364bc6adc9de` (GGUF conversion of NVIDIA's `nvidia/parakeet-tdt-0.6b-v3`, CC-BY-4.0) |
| Qwen3-VL-4B-Instruct (Qwen team, Alibaba), Q4_K_M GGUF + F16 mmproj (`Qwen3VL-4B-Instruct-Q4_K_M.gguf` ~2.5 GB, `mmproj-Qwen3VL-4B-Instruct-F16.gguf` ~836 MB) | Apache-2.0 | `Qwen/Qwen3-VL-4B-Instruct-GGUF` @ `1cd86afb9a95c410a6038ab3b40d8b578c892266`                                                                                  |
| Silero VAD v6.2, F32 GGUF (`silero-vad-v6.2-F32.gguf`, ~1.2 MB) — bundled with the app as a resource                                                           | MIT        | `handy-computer/silero-vad-v6.2-gguf` @ `97bbd75179f535e4708bccdf61e9278b8b60dfc7` (GGUF conversion of `snakers4/silero-vad`, MIT)                             |

CC-BY-4.0 and Apache-2.0 require attribution, which this table provides; the
full license texts are published at
<https://creativecommons.org/licenses/by/4.0/legalcode> and
<https://www.apache.org/licenses/LICENSE-2.0>.

## Native libraries shipped in the Windows package

- **transcribe-cpp** and its **GGML/ggml** engine (MIT) — the local speech
  recognition engine, built into the app. The runtime libraries staged into
  `transcribe-libs/` (`transcribe.dll`, `ggml-vulkan.dll`, CPU-variant DLLs)
  are transcribe-cpp/ggml build outputs and ship beside `Voicey.exe`.
- **llama.cpp** (MIT) — runs the screen-grounding vision model in the
  supervised `--vlm-worker` process; linked statically into `voicey.exe`
  via the `llama-cpp-2` crate.
- **opus** (BSD-3-Clause, Xiph.Org Foundation) — audio decoding, via the
  `opusic-sys` crate (Windows ARM64 builds only; not in the x64 package).

## Other bundled assets

- **Feedback sounds** (`marimba_start.wav`, `marimba_stop.wav`,
  `pop_start.wav`, `pop_stop.wav`) — inherited from the Handy base (MIT).
- **`src/assets/fonts/cjk-subset.woff2`** — a subset of **Noto Sans CJK**
  (© Google LLC, SIL Open Font License 1.1), embedded for UI strings;
  built by `scripts/build-cjk-font.ts` from the pinned Noto Sans CJK
  source (`notofonts/noto-cjk`, Sans 2.004).

The full dependency graph (Rust crates, npm packages) is recorded with each
package's license in the standard manifests: `src-tauri/Cargo.lock` /
`bun.lock`. Nothing in Voicey phones home: models download directly from
Hugging Face on first run, and all processing stays on this computer.
