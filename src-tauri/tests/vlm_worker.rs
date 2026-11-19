//! End-to-end checks for the `--vlm-worker` subprocess: the real executable,
//! the real protocol over stdio, and the real pinned model files.
//!
//! Ignored by default — the pinned Qwen3-VL files are multi-gigabyte
//! downloads pointed at by the harness. Run with:
//!
//! ```text
//! HANDY_VLM_MODEL=<q4 gguf> HANDY_VLM_MMPROJ=<mmproj gguf> `
//!   cargo test --test vlm_worker -- --ignored --nocapture
//! ```

use std::io::{BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Loading a multi-gigabyte model on CPU is minutes, not seconds; the
/// harness grants the worker generous deadlines.
const LOAD_DEADLINE: Duration = Duration::from_secs(600);
const GROUND_DEADLINE: Duration = Duration::from_secs(600);
/// The hello only initializes backends; a minute is plenty.
const HELLO_DEADLINE: Duration = Duration::from_secs(60);

type SharedReader = Arc<Mutex<BufReader<std::process::ChildStdout>>>;

/// One framed request/response section, little-endian length prefix — the
/// same shape `vlm_supervisor::protocol` encodes (mirrored here so this test
/// speaks the wire format independently of the app's own encoder).
fn section(bytes: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(bytes);
    frame
}

fn frame(json: &str, blob: &[u8]) -> Vec<u8> {
    let mut out = section(json.as_bytes());
    out.extend_from_slice(&section(blob));
    out
}

fn read_section(reader: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    reader.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

fn read_response(reader: &mut impl Read) -> std::io::Result<String> {
    let json = read_section(reader)?;
    let _blob = read_section(reader)?;
    Ok(String::from_utf8(json).expect("response is utf-8 json"))
}

struct Worker {
    child: Child,
    reader: SharedReader,
}

impl Worker {
    fn spawn() -> Self {
        let exe = env!("CARGO_BIN_EXE_voicey");
        let mut child = Command::new(exe)
            .arg("--vlm-worker")
            .env("HANDY_VLM_WORKER_LOG", "debug")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn the vision worker exe");
        let stdout = child.stdout.take().expect("stdout is piped");
        Self {
            child,
            reader: Arc::new(Mutex::new(BufReader::new(stdout))),
        }
    }

    /// Send one request and wait up to `deadline` for the response JSON. A
    /// hang fails the test instead of wedging it: the reading thread owns the
    /// lock and the caller walks away on timeout.
    fn call(&mut self, json: &str, blob: &[u8], deadline: Duration) -> String {
        {
            let mut stdin = self.child.stdin.as_mut().expect("stdin is piped");
            stdin
                .write_all(&frame(json, blob))
                .expect("write request frame");
            stdin.flush().expect("flush request frame");
        }
        let reader = Arc::clone(&self.reader);
        let (reply, answer) = mpsc::channel();
        thread::spawn(move || {
            // Holding the lock while blocked is the point: the deadline
            // below is what bounds the wait.
            let mut guard = reader.lock().expect("reader lock");
            let _ = reply.send(read_response(&mut *guard));
        });
        let started = Instant::now();
        let response = match answer.recv_timeout(deadline) {
            Ok(Ok(response)) => response,
            Ok(Err(e)) => panic!("protocol read failed: {e}"),
            Err(_) => panic!("worker did not answer {json} within {deadline:?}"),
        };
        println!("[vlm_worker] answered in {:?}", started.elapsed());
        response
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

/// A synthetic frame: four color quadrants; the bottom-right one white with
/// dark horizontal bars like lines of text (the transcripts name them).
fn synthetic_frame() -> (u32, u32, Vec<u8>) {
    let width = 512u32;
    let height = 512u32;
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            let (r, g, b) = if y < height / 2 {
                if x < width / 2 {
                    (200, 40, 40)
                } else {
                    (40, 40, 200)
                }
            } else if x < width / 2 {
                (40, 180, 40)
            } else if (y % 16) < 8 {
                (10, 10, 10)
            } else {
                (245, 245, 245)
            };
            rgba[i] = r;
            rgba[i + 1] = g;
            rgba[i + 2] = b;
            rgba[i + 3] = 255;
        }
    }
    (width, height, rgba)
}

fn env_path(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

fn load_request(model_path: &str, mmproj_path: &str) -> String {
    serde_json::json!({
        "Load": {
            "model_path": model_path,
            "mmproj_path": mmproj_path,
            "gpu_layers": 0
        }
    })
    .to_string()
}

fn ground_request(width: u32, height: u32, transcript: &str, max_tokens: u32) -> String {
    serde_json::json!({
        "Ground": {
            "options": {
                "width": width,
                "height": height,
                "transcript": transcript,
                "max_tokens": max_tokens
            }
        }
    })
    .to_string()
}

#[test]
#[ignore = "needs the real multi-gigabyte model files"]
fn hello_load_ground_round_trip() {
    let model_path = env_path("HANDY_VLM_MODEL");
    let mmproj_path = env_path("HANDY_VLM_MMPROJ");

    let mut worker = Worker::spawn();

    // Hello: answered once the worker's backends are up.
    let hello = worker.call(
        serde_json::json!({"Hello": null}).to_string().as_str(),
        &[],
        HELLO_DEADLINE,
    );
    assert!(hello.contains("\"Hello\""), "expected Hello, got: {hello}");

    // Load the pinned model on the CPU (the machine may be using its GPU).
    let loaded = worker.call(&load_request(&model_path, &mmproj_path), &[], LOAD_DEADLINE);
    assert!(
        loaded.contains("\"Loaded\""),
        "expected Loaded, got: {loaded}"
    );

    // Ground a transcript against the synthetic frame.
    let (width, height, rgba) = synthetic_frame();
    let request = ground_request(
        width,
        height,
        "the red box is in the top left corner and the blue one is in the top right corner, name both",
        256,
    );
    let grounded = worker.call(&request, &rgba, GROUND_DEADLINE);
    assert!(
        grounded.contains("\"Grounded\""),
        "expected Grounded, got: {grounded}"
    );
    // Empty text is a failed grounding wearing a success costume.
    assert!(
        !grounded.contains("\"Grounded\":\"\""),
        "grounding produced empty text: {grounded}"
    );
    println!("grounded: {grounded}");
}

#[test]
#[ignore = "needs the real multi-gigabyte model files"]
fn a_frame_with_the_wrong_size_is_refused_before_encoding() {
    let model_path = env_path("HANDY_VLM_MODEL");
    let mmproj_path = env_path("HANDY_VLM_MMPROJ");

    let mut worker = Worker::spawn();
    let _ = worker.call(
        serde_json::json!({"Hello": null}).to_string().as_str(),
        &[],
        HELLO_DEADLINE,
    );
    let loaded = worker.call(&load_request(&model_path, &mmproj_path), &[], LOAD_DEADLINE);
    assert!(
        loaded.contains("\"Loaded\""),
        "expected Loaded, got: {loaded}"
    );

    // Claims 4x2 pixels but sends 3: the worker must refuse it before the
    // vision encoder ever runs (that is the whole point of the size check).
    let request = ground_request(4, 2, "anything", 8);
    let rejected = worker.call(&request, &[1, 2, 3], HELLO_DEADLINE);
    assert!(
        rejected.contains("\"Error\""),
        "expected a size Error, got: {rejected}"
    );
}
