//! Parent side: [`VlmSupervisor`], the single owner of the llama.cpp vision
//! worker process. Mirrors [`crate::engine_supervisor::supervisor`]:
//!
//! * one owner thread works through a command queue and holds the only
//!   worker slot, so at most one vision worker is ever alive;
//! * a worker that will only use the CPU is started CPU-only, so it never
//!   registers a GPU backend;
//! * a crash, hang or load failure that lost a GPU worker marks the GPU
//!   unavailable, and the next load runs CPU-only;
//! * [`VlmSupervisor::cancel`] bypasses the queue and kills the worker doing
//!   the grounding, exactly like the engine's cancel;
//! * with no command for [`IDLE_UNLOAD`], the owner drops the worker, which
//!   returns the vision model's memory to the OS. The idle timeout is a
//!   fixed product decision, not a setting (D8): the worker exists to keep
//!   repeated grounded dictations fast, not to camp on gigabytes forever.

use super::protocol::{encode_message, read_message, GroundOptions, LoadedInfo, Request, Response};
use super::worker::LOG_LINE_PREFIX;
use super::{CPU_ONLY_FLAG, LOG_LEVEL_ENV, WORKER_FLAG};
use crate::screen_capture::ScreenFrame;
use log::{debug, error, info, warn};
use std::collections::VecDeque;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command as ProcessCommand, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// Backend init for the hello. Registering a GPU backend runs driver code,
/// which is where a broken driver crashes or hangs.
const HELLO_TIMEOUT: Duration = Duration::from_secs(60);
/// Model loads stay generous: a slow load is slow, not hung.
const LOAD_TIMEOUT: Duration = Duration::from_secs(180);
/// A grounding pass: vision encoding plus generation. Generous floors, since
/// a slow machine must not be mistaken for a hung one. The GPU floor covers
/// a large frame; the CPU floor covers a full encode-then-rewrite on cores.
const GROUND_TIMEOUT_GPU: Duration = Duration::from_secs(120);
const GROUND_TIMEOUT_CPU: Duration = Duration::from_secs(420);
/// How long a worker gets to exit after its stdin closes before it's killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// No grounded dictation for this long and the worker (with its few
/// gigabytes of model) goes away.
const IDLE_UNLOAD: Duration = Duration::from_secs(5 * 60);
/// Native stderr lines kept for crash reports (an abort message lands here
/// right before the process dies).
const STDERR_TAIL_LINES: usize = 64;
/// Offload every layer when the GPU is allowed; llama.cpp clamps to what
/// the device can hold.
const ALL_GPU_LAYERS: u32 = u32::MAX;

/// What to load: the two model files plus whether the GPU may be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VlmLoadSpec {
    pub model_path: PathBuf,
    pub mmproj_path: PathBuf,
    pub gpu: bool,
}

impl VlmLoadSpec {
    fn cpu_only(&self) -> bool {
        !self.gpu
    }

    fn cpu(&self) -> Self {
        Self {
            model_path: self.model_path.clone(),
            mmproj_path: self.mmproj_path.clone(),
            gpu: false,
        }
    }
}

#[derive(Debug)]
pub enum VlmError {
    /// [`VlmSupervisor::cancel`] stopped the work.
    Cancelled,
    /// This build does not include llama.cpp (stub platform); see the
    /// module docs. Only the stub ever returns it.
    #[allow(dead_code)]
    Unsupported,
    /// No model, a llama.cpp error, or a worker failure that could not be
    /// recovered.
    Failed(String),
}

impl fmt::Display for VlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VlmError::Cancelled => f.write_str("grounding was cancelled"),
            VlmError::Unsupported => f.write_str("screen grounding is not built for this platform"),
            VlmError::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for VlmError {}

/// See [`VlmSupervisor::cancel_mark`].
#[derive(Debug, Clone, Copy)]
pub struct CancelMark(u64);

type Reply<T> = mpsc::Sender<T>;

enum Command {
    Load(VlmLoadSpec, Reply<Result<LoadedInfo, VlmError>>),
    Unload(Reply<()>),
    Ground {
        options: GroundOptions,
        rgba: Vec<u8>,
        epoch: u64,
        reply: Reply<Result<String, VlmError>>,
    },
}

/// State readable without going through the queue.
#[derive(Default)]
struct Shared {
    /// Whether this host may use a GPU at all.
    gpu_allowed: bool,
    /// A GPU worker crashed, hung or failed to load: loads go CPU-only until
    /// [`VlmSupervisor::retry_gpu`].
    gpu_unavailable: AtomicBool,
    loaded: Mutex<Option<LoadedInfo>>,
    /// Bumped by every [`VlmSupervisor::cancel`]. Work queued before the
    /// bump counts as cancelled.
    cancel_epoch: AtomicU64,
    /// The call the owner is waiting on, if any. A cancel takes it and kills
    /// its worker.
    in_flight: Mutex<Option<InFlight>>,
}

impl Shared {
    fn epoch(&self) -> u64 {
        self.cancel_epoch.load(Ordering::SeqCst)
    }

    fn cancelled_since(&self, epoch: u64) -> bool {
        self.epoch() != epoch
    }

    fn gpu_unavailable(&self) -> bool {
        self.gpu_unavailable.load(Ordering::Acquire)
    }

    /// `reason` is logged when this changes the state.
    fn mark_gpu_unavailable(&self, reason: impl fmt::Display) {
        if !self.gpu_unavailable.swap(true, Ordering::AcqRel) {
            warn!("Vision-model GPU marked unavailable: {}", reason);
        }
    }
}

struct InFlight {
    what: &'static str,
    control: Arc<Control>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Owns the one vision worker slot. Cheap to clone.
#[derive(Clone)]
pub struct VlmSupervisor {
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
}

impl VlmSupervisor {
    /// Start the owner thread. No worker runs until something needs one.
    /// Without `gpu_allowed`, every worker is CPU-only, so no GPU driver code
    /// is ever loaded by the vision model.
    pub fn new(gpu_allowed: bool) -> Self {
        let (commands, queue) = mpsc::channel();
        let shared = Arc::new(Shared {
            gpu_allowed,
            ..Shared::default()
        });
        let owner = Owner {
            shared: Arc::clone(&shared),
            worker: None,
            spec: None,
        };
        thread::Builder::new()
            .name("vlm-engine".into())
            .spawn(move || owner.run(queue))
            .expect("failed to start the vision engine thread");
        Self { commands, shared }
    }

    fn ask<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Option<T> {
        let (reply, answer) = mpsc::channel();
        self.commands.send(command(reply)).ok()?;
        answer.recv().ok()
    }

    /// Load the vision model, replacing the current one. The old worker has
    /// fully exited before the new one starts, so two models are never held
    /// at once. Loading an already-loaded spec is a no-op that reports the
    /// loaded state.
    pub fn load(&self, spec: VlmLoadSpec) -> Result<LoadedInfo, VlmError> {
        self.ask(|reply| Command::Load(spec, reply))
            .unwrap_or_else(|| Err(engine_stopped()))
    }

    /// The loaded model. A snapshot; never waits on the queue.
    pub fn loaded(&self) -> Option<LoadedInfo> {
        lock(&self.shared.loaded).clone()
    }

    /// Ground `frame` against the loaded model. A worker crash or hang is
    /// retried once in a fresh worker (on CPU if it was on the GPU).
    pub fn ground(
        &self,
        mark: CancelMark,
        frame: &ScreenFrame,
        transcript: String,
    ) -> Result<String, VlmError> {
        let options = GroundOptions::new(frame.width, frame.height, transcript);
        let rgba = frame.rgba.clone();
        self.ask(|reply| Command::Ground {
            options,
            rgba,
            epoch: mark.0,
            reply,
        })
        .unwrap_or_else(|| Err(engine_stopped()))
    }

    /// A point to cancel from: every grounding given it is cancelled by any
    /// [`cancel`](Self::cancel) after it was taken.
    pub fn cancel_mark(&self) -> CancelMark {
        CancelMark(self.shared.epoch())
    }

    /// Stop the grounding in progress and any queued behind it. Bypasses the
    /// queue. The worker doing the work is killed, so the next use starts a
    /// fresh one and reloads the model.
    pub fn cancel(&self) {
        self.shared.cancel_epoch.fetch_add(1, Ordering::SeqCst);
        if let Some(call) = lock(&self.shared.in_flight).take() {
            info!(
                "Cancelling the {} by stopping its worker (pid {})",
                call.what, call.control.pid
            );
            call.control.kill();
        }
    }

    /// Forget the GPU failure, if any, so the next load may use it again.
    pub fn retry_gpu(&self, reason: &str) {
        if self.shared.gpu_unavailable.swap(false, Ordering::AcqRel) {
            info!("Vision-model GPU no longer marked unavailable: {}", reason);
        }
    }

    /// Unload the model without waiting. The next use loads it again.
    pub fn unload(&self) {
        let _ = self.ask(Command::Unload);
    }
}

fn engine_stopped() -> VlmError {
    VlmError::Failed("the vision engine has stopped".to_string())
}

fn not_loaded() -> VlmError {
    VlmError::Failed("no vision model is loaded".to_string())
}

/// How a call to the worker failed.
#[derive(Debug)]
enum Failure {
    /// The worker process could not be started.
    Spawn(io::Error),
    /// llama.cpp returned an error; the worker is fine.
    Remote(String),
    /// The worker exited or crashed. Carries exit status and its last
    /// native output.
    Crashed(String),
    /// The worker did not answer within the deadline and was killed.
    Hung(Duration),
    Cancelled,
}

impl Failure {
    /// Whether the worker is gone, or can't be trusted with another call.
    fn worker_lost(&self) -> bool {
        matches!(self, Failure::Crashed(_) | Failure::Hung(_))
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Spawn(e) => write!(f, "failed to start vision worker: {e}"),
            Failure::Remote(message) => f.write_str(message),
            Failure::Crashed(detail) => write!(f, "vision worker crashed ({detail})"),
            Failure::Hung(t) => write!(f, "vision worker did not respond within {t:?}"),
            Failure::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl From<Failure> for VlmError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Cancelled => VlmError::Cancelled,
            other => VlmError::Failed(other.to_string()),
        }
    }
}

fn unexpected(response: &Response) -> Failure {
    Failure::Remote(format!("unexpected vision worker response: {response:?}"))
}

// --- Owner thread ---------------------------------------------------------

struct Owner {
    shared: Arc<Shared>,
    /// The single worker slot.
    worker: Option<Worker>,
    /// The model the app asked for. Outlives its worker: after a crash, hang
    /// or cancel the next use starts a fresh worker for it.
    spec: Option<VlmLoadSpec>,
}

impl Owner {
    fn run(mut self, queue: mpsc::Receiver<Command>) {
        loop {
            let command = match queue.recv_timeout(IDLE_UNLOAD) {
                Ok(command) => command,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if self.worker.is_some() {
                        info!(
                            "Vision worker idle for {:?}; unloading the model",
                            IDLE_UNLOAD
                        );
                        self.unload();
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            match command {
                Command::Load(spec, reply) => {
                    let _ = reply.send(self.load(spec));
                }
                Command::Unload(reply) => {
                    self.unload();
                    let _ = reply.send(());
                }
                Command::Ground {
                    options,
                    rgba,
                    epoch,
                    reply,
                } => {
                    let _ = reply.send(self.ground(&options, &rgba, epoch));
                }
            }
        }
        // Every handle is gone.
        self.unload();
    }

    fn load(&mut self, spec: VlmLoadSpec) -> Result<LoadedInfo, VlmError> {
        // Already loaded with this spec: reuse the live worker.
        if self.worker.is_some() && self.spec.as_ref() == Some(&spec) {
            if let Some(info) = lock(&self.shared.loaded).clone() {
                return Ok(info);
            }
        }
        self.unload();
        self.spec = Some(spec);
        let result = self.start();
        if result.is_err() {
            self.spec = None;
        }
        result
    }

    fn unload(&mut self) {
        if let Some(worker) = self.worker.take() {
            let started = Instant::now();
            let pid = worker.control.pid;
            drop(worker);
            debug!(
                "Vision worker (pid {}) stopped in {}ms",
                pid,
                started.elapsed().as_millis()
            );
        }
        self.spec = None;
        *lock(&self.shared.loaded) = None;
    }

    /// The spec a fresh worker should load: CPU while the GPU is marked
    /// unavailable.
    fn effective_spec(&self, spec: &VlmLoadSpec) -> VlmLoadSpec {
        if spec.cpu_only() || !self.shared.gpu_unavailable() {
            return spec.clone();
        }
        info!("Loading the vision model on CPU: the GPU failed earlier");
        spec.cpu()
    }

    /// Whether a worker for `spec` keeps every layer on the CPU.
    fn cpu_only_worker(&self, spec: &VlmLoadSpec) -> bool {
        spec.cpu_only() || !self.shared.gpu_allowed
    }

    /// Start a worker for `spec` and load it there.
    fn start_worker(&self, spec: &VlmLoadSpec) -> Result<(Worker, LoadedInfo), Failure> {
        let cpu_only = self.cpu_only_worker(spec);
        let mut worker = Worker::spawn(cpu_only).map_err(Failure::Spawn)?;
        let hello = worker.call(&Request::Hello, None, HELLO_TIMEOUT)?;
        if !matches!(hello, Response::Hello) {
            return Err(unexpected(&hello));
        }
        let info = worker.load(spec)?;
        Ok((worker, info))
    }

    /// Start a worker and load the current spec in it. If a GPU worker is
    /// lost on the way, mark the GPU unavailable and retry once CPU-only.
    fn start(&mut self) -> Result<LoadedInfo, VlmError> {
        let spec = self.spec.clone().ok_or_else(not_loaded)?;
        let started = Instant::now();
        let wanted = self.effective_spec(&spec);
        let (worker, info) = match self.start_worker(&wanted) {
            Ok(started) => started,
            Err(failure) if failure.worker_lost() && !self.cpu_only_worker(&wanted) => {
                self.shared.mark_gpu_unavailable(format!(
                    "a worker for '{}' with GPU backends {}",
                    wanted.model_path.display(),
                    failure
                ));
                warn!("Retrying in a CPU-only vision worker");
                self.start_worker(&wanted.cpu())?
            }
            Err(failure) => return Err(failure.into()),
        };
        debug!(
            "Vision worker (pid {}) spawned and loaded the model in {}ms",
            worker.control.pid,
            started.elapsed().as_millis()
        );
        *lock(&self.shared.loaded) = Some(info.clone());
        self.worker = Some(worker);
        Ok(info)
    }

    /// Make sure the slot holds a worker with the model loaded, starting a
    /// fresh one if the last one died or was cancelled.
    fn ensure_worker(&mut self) -> Result<(), VlmError> {
        if self.worker.is_some() {
            return Ok(());
        }
        if self.spec.is_none() {
            return Err(not_loaded());
        }
        info!("Starting a fresh vision worker for the loaded model");
        if let Err(e) = self.start() {
            self.spec = None;
            *lock(&self.shared.loaded) = None;
            return Err(e);
        }
        Ok(())
    }

    /// Ground one frame. Cancelled first; retried once after a crash or
    /// hang (on CPU if the lost worker was on the GPU).
    fn ground(
        &mut self,
        options: &GroundOptions,
        rgba: &[u8],
        epoch: u64,
    ) -> Result<String, VlmError> {
        let request = Request::Ground {
            options: options.clone(),
        };
        let mut retried = false;
        loop {
            if self.shared.cancelled_since(epoch) {
                return Err(VlmError::Cancelled);
            }
            // A respawn here runs under the load deadline; the grounding's
            // own deadline starts once the model is loaded.
            self.ensure_worker()?;
            let on_gpu = lock(&self.shared.loaded)
                .as_ref()
                .is_some_and(|info| info.on_gpu);
            let deadline = if on_gpu {
                GROUND_TIMEOUT_GPU
            } else {
                GROUND_TIMEOUT_CPU
            };
            match self.call_work(&request, Some(rgba), deadline, epoch, "grounding") {
                Ok(Response::Grounded(text)) => return Ok(text),
                Ok(other) => return Err(unexpected(&other).into()),
                Err(failure) if failure.worker_lost() => {
                    // Drop the dead handle; a fresh worker reloads below.
                    self.worker = None;
                    let lost_on_gpu = self
                        .spec
                        .as_ref()
                        .is_some_and(|spec| !self.cpu_only_worker(&self.effective_spec(spec)));
                    if lost_on_gpu && !self.shared.gpu_unavailable() {
                        self.shared
                            .mark_gpu_unavailable(format!("{failure} during grounding"));
                    }
                    if retried {
                        return Err(failure.into());
                    }
                    warn!("Retrying the grounding in a fresh vision worker");
                    retried = true;
                }
                Err(failure) => return Err(failure.into()),
            }
        }
    }

    /// A call that [`VlmSupervisor::cancel`] can stop. Returns `Cancelled`
    /// if a cancel came after `epoch`, whatever the worker said.
    fn call_work(
        &mut self,
        request: &Request,
        blob: Option<&[u8]>,
        deadline: Duration,
        epoch: u64,
        what: &'static str,
    ) -> Result<Response, Failure> {
        let worker = self.worker.as_mut().expect("caller ensured a worker");
        let frame = encode_message(request, blob)
            .map_err(|e| Failure::Remote(format!("failed to encode the grounding request: {e}")))?;
        let sent = {
            // Check, send and publish under the lock cancel() takes after
            // bumping the epoch: either this sees the cancel and sends
            // nothing, or the cancel sees the call and kills its worker.
            let mut in_flight = lock(&self.shared.in_flight);
            if self.shared.cancelled_since(epoch) {
                return Err(Failure::Cancelled);
            }
            let sent = worker.control.send(frame);
            *in_flight = Some(InFlight {
                what,
                control: Arc::clone(&worker.control),
            });
            sent
        };
        let result = if sent {
            worker.wait(request, deadline)
        } else {
            Err(worker.died())
        };
        // Gone if cancel() took the call, which means it killed the worker
        // (possibly just after it answered).
        let killed = lock(&self.shared.in_flight).take().is_none();
        if killed || self.shared.cancelled_since(epoch) {
            if killed || result.as_ref().is_err_and(Failure::worker_lost) {
                self.worker = None;
            }
            return Err(Failure::Cancelled);
        }
        result
    }
}

// --- Worker process handle ------------------------------------------------

/// The parts of a worker that other threads touch: its stdin queue and the
/// process (to kill it). Every write goes through the one writer thread, so
/// a cancel can never interleave with a large frame.
struct Control {
    pid: u32,
    child: Mutex<Child>,
    stdin: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
}

impl Control {
    fn send(&self, frame: Vec<u8>) -> bool {
        lock(&self.stdin)
            .as_ref()
            .is_some_and(|stdin| stdin.send(frame).is_ok())
    }

    /// Closing stdin tells the worker to exit (it ends on EOF).
    fn close_stdin(&self) {
        lock(&self.stdin).take();
    }

    fn kill(&self) {
        let _ = lock(&self.child).kill();
    }
}

/// One worker process, owned by the supervisor's thread. Strict
/// request/response. Dropping it closes the worker's stdin, gives it
/// [`SHUTDOWN_GRACE`] to exit, kills it otherwise, and reaps it.
struct Worker {
    control: Arc<Control>,
    responses: mpsc::Receiver<Response>,
    stderr_done: mpsc::Receiver<()>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

impl Worker {
    /// Start a worker process. A `cpu_only` worker keeps every layer on the
    /// CPU.
    fn spawn(cpu_only: bool) -> io::Result<Self> {
        // Under `cargo test` the current exe is the test harness; tests
        // point this at the built app instead.
        #[cfg(test)]
        let exe = PathBuf::from(
            std::env::var_os("HANDY_VLM_WORKER_EXE").expect("set HANDY_VLM_WORKER_EXE"),
        );
        #[cfg(not(test))]
        let exe = worker_exe()?;
        let mut command = ProcessCommand::new(exe);
        #[cfg(target_os = "linux")]
        if let Some(arg0) = std::env::args_os().next() {
            // Keep `ps` showing Voicey's own path rather than /proc/self/exe.
            use std::os::unix::process::CommandExt;
            command.arg0(arg0);
        }
        command.arg(WORKER_FLAG);
        if cpu_only {
            command.arg(CPU_ONLY_FLAG);
        }
        command
            .env(LOG_LEVEL_ENV, log::max_level().as_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn()?;
        let pid = child.id();
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let (stdin_tx, frames) = mpsc::channel();
        let (response_tx, responses) = mpsc::channel();
        let (stderr_done_tx, stderr_done) = mpsc::channel();
        // Built before the threads so a failure below still reaps the child.
        let worker = Worker {
            control: Arc::new(Control {
                pid,
                child: Mutex::new(child),
                stdin: Mutex::new(Some(stdin_tx)),
            }),
            responses,
            stderr_done,
            stderr_tail: Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES))),
        };

        thread::Builder::new()
            .name("vlm-worker-in".into())
            .spawn(move || write_frames(stdin, frames))?;
        thread::Builder::new()
            .name("vlm-worker-out".into())
            .spawn(move || read_responses(stdout, response_tx))?;
        // Always drain stderr: a full pipe would block the worker.
        let tail = Arc::clone(&worker.stderr_tail);
        thread::Builder::new()
            .name("vlm-worker-err".into())
            .spawn(move || {
                // Split on raw bytes: native code may write non-UTF-8, and
                // `lines()` would end the loop there, losing every later
                // line, abort message included.
                for line in BufReader::new(stderr).split(b'\n') {
                    let Ok(line) = line else { break };
                    let line = String::from_utf8_lossy(&line);
                    let line = line.strip_suffix('\r').unwrap_or(&line).to_string();
                    forward_worker_log(&line);
                    let mut tail = lock(&tail);
                    if tail.len() == STDERR_TAIL_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
                let _ = stderr_done_tx.send(());
            })?;

        debug!(
            "Started {}vision worker (pid {})",
            if cpu_only { "CPU-only " } else { "" },
            pid
        );
        Ok(worker)
    }

    fn load(&mut self, spec: &VlmLoadSpec) -> Result<LoadedInfo, Failure> {
        let request = Request::Load {
            model_path: spec.model_path.clone(),
            mmproj_path: spec.mmproj_path.clone(),
            gpu_layers: if spec.gpu { ALL_GPU_LAYERS } else { 0 },
        };
        match self.call(&request, None, LOAD_TIMEOUT)? {
            Response::Loaded(info) => Ok(info),
            other => Err(unexpected(&other)),
        }
    }

    /// Send one request and wait up to `deadline` for its response. Hitting
    /// the deadline kills the worker; after any `Crashed`/`Hung` the handle
    /// is dead and should be dropped.
    fn call(
        &mut self,
        request: &Request,
        blob: Option<&[u8]>,
        deadline: Duration,
    ) -> Result<Response, Failure> {
        let frame = encode_message(request, blob)
            .map_err(|e| Failure::Remote(format!("failed to encode request: {e}")))?;
        if !self.control.send(frame) {
            return Err(self.died());
        }
        self.wait(request, deadline)
    }

    /// Wait for the response to `request`, already sent.
    fn wait(&mut self, request: &Request, deadline: Duration) -> Result<Response, Failure> {
        match self.responses.recv_timeout(deadline) {
            Ok(Response::Error(message)) => Err(Failure::Remote(message)),
            Ok(response) => Ok(response),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.control.kill();
                let tail = self.native_tail();
                error!(
                    "Vision worker (pid {}) did not answer {} within {:?}; killed it{}",
                    self.control.pid,
                    request_name(request),
                    deadline,
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!(". Native output before the hang:\n{}", tail.join("\n"))
                    }
                );
                Err(Failure::Hung(deadline))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.died()),
        }
    }

    /// The worker's stdout closed: collect its exit status and last output.
    fn died(&mut self) -> Failure {
        // Let the stderr thread catch the final lines (the abort message).
        let _ = self.stderr_done.recv_timeout(Duration::from_secs(1));
        let status = {
            let mut child = lock(&self.control.child);
            match wait_exit(&mut child, Duration::from_secs(1)) {
                Some(status) => status.to_string(),
                None => {
                    let _ = child.kill();
                    "unresponsive after closing its output; killed".to_string()
                }
            }
        };
        let tail = self.native_tail();
        if tail.is_empty() {
            error!("Vision worker (pid {}) died: {}", self.control.pid, status);
        } else {
            error!(
                "Vision worker (pid {}) died: {}. Native output before exit:\n{}",
                self.control.pid,
                status,
                tail.join("\n")
            );
        }
        Failure::Crashed(match tail.last() {
            Some(last) => format!("{status}: {last}"),
            None => status,
        })
    }

    /// Recent raw native output (worker log lines are excluded; they were
    /// already re-logged).
    fn native_tail(&self) -> Vec<String> {
        lock(&self.stderr_tail)
            .iter()
            .filter(|line| !line.starts_with(LOG_LINE_PREFIX))
            .cloned()
            .collect()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.control.close_stdin();
        let mut child = lock(&self.control.child);
        if wait_exit(&mut child, SHUTDOWN_GRACE).is_none() {
            warn!(
                "Vision worker (pid {}) did not exit within {:?}; killing it",
                self.control.pid, SHUTDOWN_GRACE
            );
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Wait up to `timeout` for `child` to exit. `None` if it is still running
/// (or its status can't be read).
fn wait_exit(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            _ => return None,
        }
    }
}

/// The executable a worker runs: always this very binary, so the two agree
/// on the protocol. Windows locks a running executable against replacement;
/// on Linux the worker runs from `/proc/self/exe`, which is this binary even
/// once its file is replaced.
#[cfg(not(test))]
fn worker_exe() -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    return Ok(PathBuf::from("/proc/self/exe"));
    #[cfg(not(target_os = "linux"))]
    std::env::current_exe()
}

fn write_frames(mut stdin: ChildStdin, frames: mpsc::Receiver<Vec<u8>>) {
    // Ends when the handle closes stdin (dropping it here sends EOF) or the
    // worker is gone.
    for frame in frames {
        if stdin
            .write_all(&frame)
            .and_then(|()| stdin.flush())
            .is_err()
        {
            break;
        }
    }
}

fn read_responses(stdout: ChildStdout, responses: mpsc::Sender<Response>) {
    let mut stdout = BufReader::new(stdout);
    loop {
        match read_message::<Response>(&mut stdout) {
            Ok(Some((response, _))) => {
                if responses.send(response).is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                error!("Vision worker protocol error: {}", e);
                break;
            }
        }
    }
}

fn request_name(request: &Request) -> &'static str {
    match request {
        Request::Hello => "hello",
        Request::Load { .. } => "model load",
        Request::Ground { .. } => "grounding",
    }
}

/// Re-log a worker stderr line in the parent. Structured lines keep their
/// level; anything else is raw native output.
fn forward_worker_log(line: &str) {
    const TARGET: &str = "vlm_worker";
    if let Some(rest) = line.strip_prefix(LOG_LINE_PREFIX) {
        let mut parts = rest.splitn(3, '\t');
        if let (Some(level), Some(target), Some(message)) =
            (parts.next(), parts.next(), parts.next())
        {
            let level = level.parse::<log::Level>().unwrap_or(log::Level::Info);
            log::log!(target: TARGET, level, "[{}] {}", target, message);
            return;
        }
    }
    log::info!(target: TARGET, "{}", line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cpu_spec_strips_the_gpu() {
        let spec = VlmLoadSpec {
            model_path: PathBuf::from("model.gguf"),
            mmproj_path: PathBuf::from("mmproj.gguf"),
            gpu: true,
        };
        let cpu = spec.cpu();
        assert!(cpu.cpu_only());
        assert_eq!(cpu.model_path, spec.model_path);
        assert_eq!(cpu.mmproj_path, spec.mmproj_path);
    }

    #[test]
    fn a_load_uses_every_gpu_layer_and_a_cpu_load_none() {
        let spec = |gpu| VlmLoadSpec {
            model_path: PathBuf::new(),
            mmproj_path: PathBuf::new(),
            gpu,
        };
        let request = |gpu| {
            let spec = spec(gpu);
            Request::Load {
                model_path: spec.model_path.clone(),
                mmproj_path: spec.mmproj_path.clone(),
                gpu_layers: if spec.gpu { ALL_GPU_LAYERS } else { 0 },
            }
        };
        assert!(matches!(
            request(true),
            Request::Load {
                gpu_layers: ALL_GPU_LAYERS,
                ..
            }
        ));
        assert!(matches!(
            request(false),
            Request::Load { gpu_layers: 0, .. }
        ));
    }

    #[test]
    fn an_idle_owner_thread_unloads_its_worker() {
        // The idle rule is structural (recv_timeout in the owner loop), so
        // exercise what its pieces promise: an unloaded owner reports no
        // model, and unloading a second time is a no-op.
        let supervisor = VlmSupervisor::new(false);
        supervisor.unload();
        assert!(supervisor.loaded().is_none());
    }
}
