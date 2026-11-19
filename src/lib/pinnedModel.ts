/**
 * The only dictation model Voicey ships: Parakeet TDT 0.6B v3 (Q8_0) from the
 * pinned handy-computer Hugging Face repo. This is the *registry* id
 * (`{repo}/{filename}` — the same form every model id in the app takes),
 * matching `settings::PINNED_MODEL_ID` on the backend. The backend pins the
 * same id, auto-downloads it on first run, and there is no model picker
 * anywhere — this constant is how the frontend finds the model's download
 * state and progress.
 */
export const PINNED_MODEL_ID =
  "handy-computer/parakeet-tdt-0.6b-v3-gguf/parakeet-tdt-0.6b-v3-Q8_0.gguf";
