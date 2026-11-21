//! The engine process: the model is in memory only while there is work.
//!
//! The model and the lexicons are nearly all of the service's memory, and
//! the service is idle nearly all the time (the app makes its clips once,
//! in the background). So the HTTP front loads nothing: [`Worker`] starts
//! an engine process (`vocab-tts worker`, the same binary) for the first
//! request, sends it the requests one at a time, and stops it after
//! `VOCAB_TTS_IDLE_SECONDS` without one; when it exits, all its memory goes
//! back to the system (dropping a model inside a process would leave most
//! of it in the allocator).
//!
//! The engine process loads the lexicons and the model, answers one line
//! (ready, or why not), then serves requests from its stdin until stdin is
//! closed. Protocol: one JSON object per line ([`Ready`], [`Request`],
//! [`Reply`]); the engine process logs to stderr (the container log), its
//! stdout carries only the protocol.
//!
//! * A start that fails (model missing, unreadable, no answer within
//!   [`START_TIMEOUT`]) is an error on the request and in the log, and
//!   [`Worker::healthy`] is false until a start succeeds.
//! * An engine process that dies while it speaks (killed by the kernel when
//!   the container is out of memory, or a crash) is [`SpeakError::Crashed`]:
//!   the app counts it like a crash of the whole service before.
//! * Cancelled work (request timeout, client gone) stops the engine process
//!   at once; the next request starts a new one.

use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::process::{ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::engine::{self, Cancel};
use crate::phonemize::Phonemizer;
use crate::speaker::{Backend, Local, SpeakError, Spoken, Work};
use crate::speech::{SpeakText, Speed, Voice};

/// Subcommand of the binary that runs the engine process.
pub const SUBCOMMAND: &str = "worker";
/// Loading lexicons and model takes about a second on a fast CPU; a slow
/// one at half a CPU may take many times that.
pub const START_TIMEOUT: Duration = Duration::from_secs(120);
/// Time an engine process gets to exit after its stdin is closed.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the idle engine process is looked at.
const IDLE_CHECK: Duration = Duration::from_secs(1);
/// Longest line of the protocol: a reply holds the MP3 as hex (60 s of
/// speech is about 0.5 MB of MP3).
const MAX_REPLY_LINE: u64 = 4 << 20;
/// A request is a validated text of at most 600 characters.
const MAX_REQUEST_LINE: u64 = 16 << 10;

/// The engine process is ready (first line it writes), with its load times.
#[derive(Debug, Serialize, Deserialize)]
struct Ready {
    lexicons_ms: u64,
    model_ms: u64,
}

/// First line of the engine process.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Hello {
    Ready(Ready),
    Failed(String),
}

#[derive(Debug, Serialize, Deserialize)]
struct Request {
    text: String,
    voice: String,
    speed: f64,
}

/// Why the engine process did not speak a text. Carries no text.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Refusal {
    NothingToSay,
    TooLong(usize),
    Failed(String),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reply {
    Spoken(Spoken),
    Refused(Refusal),
}

impl From<SpeakError> for Refusal {
    fn from(err: SpeakError) -> Self {
        match err {
            SpeakError::NothingToSay => Self::NothingToSay,
            SpeakError::TooLong(symbols) => Self::TooLong(symbols),
            other => Self::Failed(other.to_string()),
        }
    }
}

impl From<Refusal> for SpeakError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::NothingToSay => Self::NothingToSay,
            Refusal::TooLong(symbols) => Self::TooLong(symbols),
            Refusal::Failed(message) => Self::Worker(message),
        }
    }
}

// ── the engine process ──────────────────────────────────────────────────────

/// Run the engine process: load, then serve the requests on stdin until it
/// is closed. Logs go to stderr.
pub fn run(model_dir: &Path, threads: u16) -> ExitCode {
    let stdin = std::io::stdin().lock();
    let stdout = std::io::stdout().lock();
    let started = Instant::now();
    let loaded = Phonemizer::load(model_dir)
        .map_err(|err| format!("cannot load the lexicons: {err}"))
        .and_then(|phonemizer| {
            let lexicons_ms = elapsed_ms(started);
            let started = Instant::now();
            let engine = engine::load(model_dir, threads)
                .map_err(|err| format!("cannot load the model: {err}"))?;
            let ready = Ready {
                lexicons_ms,
                model_ms: elapsed_ms(started),
            };
            Ok((Local::new(phonemizer, engine), ready))
        });
    match loaded {
        Ok((local, ready)) => match serve(stdin, stdout, &local, ready) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                tracing::error!(error = %err, "engine process: protocol failed");
                ExitCode::FAILURE
            }
        },
        Err(message) => {
            tracing::error!("{message}");
            let mut stdout = stdout;
            write_line(&mut stdout, &Hello::Failed(message)).ok();
            ExitCode::FAILURE
        }
    }
}

/// Say ready, then answer each request line until `input` ends.
fn serve(
    mut input: impl BufRead,
    mut output: impl Write,
    local: &Local,
    ready: Ready,
) -> std::io::Result<()> {
    write_line(&mut output, &Hello::Ready(ready))?;
    let mut line = String::new();
    loop {
        line.clear();
        let read = input.by_ref().take(MAX_REQUEST_LINE).read_line(&mut line)?;
        if read == 0 {
            return Ok(()); // stdin closed: stop
        }
        if !line.ends_with('\n') {
            return Err(std::io::Error::other("request line too long"));
        }
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(request) => speak(local, &request),
            Err(_) => Reply::Refused(Refusal::Failed("malformed request".to_owned())),
        };
        write_line(&mut output, &reply)?;
    }
}

/// One request; validated again (the front validated it already).
fn speak(local: &Local, request: &Request) -> Reply {
    let text = match SpeakText::new(&request.text) {
        Ok(text) => text,
        Err(err) => return Reply::Refused(Refusal::Failed(err.to_string())),
    };
    let (Some(voice), Ok(speed)) = (Voice::parse(&request.voice), Speed::new(request.speed)) else {
        return Reply::Refused(Refusal::Failed("invalid voice or speed".to_owned()));
    };
    match local.speak_now(&text, voice, speed) {
        Ok(spoken) => Reply::Spoken(spoken),
        Err(err) => Reply::Refused(err.into()),
    }
}

fn write_line(output: &mut impl Write, message: &impl Serialize) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    line.push(b'\n');
    output.write_all(&line)?;
    output.flush()
}

// ── the front ───────────────────────────────────────────────────────────────

/// How to start the engine process.
#[derive(Debug, Clone)]
pub struct WorkerCommand {
    program: std::path::PathBuf,
    args: Vec<String>,
}

impl WorkerCommand {
    /// This binary with the [`SUBCOMMAND`] (it reads the same environment).
    pub fn this_binary() -> std::io::Result<Self> {
        Ok(Self {
            program: std::env::current_exe()?,
            args: vec![SUBCOMMAND.to_owned()],
        })
    }

    /// Any program that speaks the protocol (tests).
    pub fn new(program: impl Into<std::path::PathBuf>, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|&a| a.to_owned()).collect(),
        }
    }
}

/// The engine process, started on demand and stopped when idle.
#[derive(Debug)]
pub struct Worker {
    command: WorkerCommand,
    idle: Duration,
    slot: Mutex<Slot>,
    healthy: AtomicBool,
}

#[derive(Debug)]
struct Slot {
    running: Option<Running>,
    last_used: Instant,
}

/// A started engine process.
#[derive(Debug)]
struct Running {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pid: u32,
    started: Instant,
    served: u64,
}

/// Why an exchange with the engine process failed.
enum Broken {
    /// It closed its stdout (it exited or died).
    Gone,
    /// It wrote something that is not the protocol, or the pipe failed.
    Protocol(String),
}

impl Worker {
    /// No engine process yet; `idle`: how long one stays without work.
    /// Call [`Worker::watch_idle`] to have idle engine processes stopped.
    pub fn new(command: WorkerCommand, idle: Duration) -> Arc<Self> {
        Arc::new(Self {
            command,
            idle,
            slot: Mutex::new(Slot {
                running: None,
                last_used: Instant::now(),
            }),
            healthy: AtomicBool::new(true),
        })
    }

    /// Stop the engine process after [`Worker`]'s idle time without work
    /// (a task on the runtime; ends when the worker is dropped).
    pub fn watch_idle(self: &Arc<Self>) {
        let worker: Weak<Self> = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(IDLE_CHECK).await;
                let Some(worker) = worker.upgrade() else {
                    return;
                };
                // Busy (a request holds the slot): not idle.
                let Ok(mut slot) = worker.slot.try_lock() else {
                    continue;
                };
                if slot.last_used.elapsed() >= worker.idle
                    && let Some(running) = slot.running.take()
                {
                    running.stop("idle").await;
                }
            }
        });
    }

    /// Whether an engine process is running and idle (waits for the work
    /// in progress, if any).
    pub async fn is_running(&self) -> bool {
        self.slot.lock().await.running.is_some()
    }

    /// Stop the engine process now (after the work it is doing).
    pub async fn release(&self) {
        if let Some(running) = self.slot.lock().await.running.take() {
            running.stop("released").await;
        }
    }

    async fn start(&self, cancel: &Cancel) -> Result<Running, SpeakError> {
        let started = Instant::now();
        let mut child = Command::new(&self.command.program)
            .args(&self.command.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| self.start_failed(format!("cannot start the engine process: {err}")))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(self.start_failed("the engine process has no pipes".to_owned()));
        };
        let mut running = Running {
            pid: child.id().unwrap_or(0),
            child,
            stdin,
            stdout: BufReader::new(stdout),
            started,
            served: 0,
        };
        let hello = tokio::select! {
            hello = tokio::time::timeout(START_TIMEOUT, running.read_line()) => Some(hello),
            () = cancel.cancelled() => None,
        };
        let Some(hello) = hello else {
            running.abandon("cancelled while loading").await;
            return Err(SpeakError::Cancelled);
        };
        let failure = match hello {
            Ok(Ok(line)) => match serde_json::from_str::<Hello>(&line) {
                Ok(Hello::Ready(ready)) => {
                    self.healthy.store(true, Ordering::SeqCst);
                    tracing::info!(
                        pid = running.pid,
                        lexicons_ms = ready.lexicons_ms,
                        model_ms = ready.model_ms,
                        start_ms = elapsed_ms(started),
                        "engine process started"
                    );
                    return Ok(running);
                }
                Ok(Hello::Failed(message)) => message,
                Err(_) => "the engine process did not say it is ready".to_owned(),
            },
            Ok(Err(Broken::Gone)) => {
                let status = running.exit_status().await;
                format!("the engine process exited while loading ({status})")
            }
            Ok(Err(Broken::Protocol(message))) => message,
            Err(_) => format!(
                "the engine process was not ready within {} s",
                START_TIMEOUT.as_secs()
            ),
        };
        running.abandon("failed to start").await;
        Err(self.start_failed(failure))
    }

    fn start_failed(&self, message: String) -> SpeakError {
        self.healthy.store(false, Ordering::SeqCst);
        tracing::error!(error = %message, "the engine could not be started");
        SpeakError::Worker(message)
    }

    async fn speak_in_process(
        &self,
        text: SpeakText,
        voice: Voice,
        speed: Speed,
        cancel: Cancel,
    ) -> Result<Spoken, SpeakError> {
        let mut slot = self.slot.lock().await;
        // Waited for the engine: is anybody still waiting for this?
        if cancel.is_cancelled() {
            return Err(SpeakError::Cancelled);
        }
        let mut running = match slot.running.take() {
            Some(running) => running,
            None => self.start(&cancel).await?,
        };
        let request = Request {
            text: text.as_str().to_owned(),
            voice: voice.id().to_owned(),
            speed: f64::from(speed.get()),
        };
        let outcome = tokio::select! {
            reply = running.exchange(&request) => Some(reply),
            () = cancel.cancelled() => None,
        };
        let Some(outcome) = outcome else {
            running.abandon("cancelled").await;
            return Err(SpeakError::Cancelled);
        };
        match outcome {
            Ok(reply) => {
                running.served += 1;
                slot.last_used = Instant::now();
                if self.idle.is_zero() {
                    running.stop("idle").await;
                } else {
                    slot.running = Some(running);
                }
                match reply {
                    Reply::Spoken(spoken) => Ok(spoken),
                    Reply::Refused(refusal) => Err(refusal.into()),
                }
            }
            Err(Broken::Gone) => {
                let status = running.exit_status().await;
                tracing::error!(pid = running.pid, %status, "the engine process died while speaking");
                Err(SpeakError::Crashed(status))
            }
            Err(Broken::Protocol(message)) => {
                running.abandon("protocol error").await;
                Err(SpeakError::Worker(message))
            }
        }
    }
}

impl Backend for Worker {
    fn speak(self: Arc<Self>, text: SpeakText, voice: Voice, speed: Speed, cancel: Cancel) -> Work {
        Box::pin(async move { self.speak_in_process(text, voice, speed, cancel).await })
    }

    fn healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }
}

impl Running {
    /// One protocol line (without the line break).
    async fn read_line(&mut self) -> Result<String, Broken> {
        let mut line = String::new();
        loop {
            line.clear();
            let read = (&mut self.stdout)
                .take(MAX_REPLY_LINE)
                .read_line(&mut line)
                .await
                .map_err(|err| Broken::Protocol(format!("engine process output: {err}")))?;
            if read == 0 {
                return Err(Broken::Gone);
            }
            if !line.ends_with('\n') {
                return Err(Broken::Protocol(
                    "engine process output: line too long".to_owned(),
                ));
            }
            if line.starts_with('{') {
                line.pop();
                return Ok(line);
            }
            // Not ours (a library printing to stdout): skipped, and only its
            // size is logged (it could hold phonemes).
            tracing::warn!(
                bytes = line.len(),
                "engine process wrote something else to stdout; ignored"
            );
        }
    }

    async fn exchange(&mut self, request: &Request) -> Result<Reply, Broken> {
        let mut line =
            serde_json::to_vec(request).map_err(|err| Broken::Protocol(err.to_string()))?;
        line.push(b'\n');
        // A process that died makes the write fail (broken pipe) or the read
        // end: both mean it is gone.
        if self.stdin.write_all(&line).await.is_err() || self.stdin.flush().await.is_err() {
            return Err(Broken::Gone);
        }
        let reply = self.read_line().await?;
        serde_json::from_str(&reply).map_err(|_| Broken::Protocol("malformed reply".to_owned()))
    }

    /// Close its stdin and let it exit; killed if it does not in time.
    async fn stop(self, why: &str) {
        let Self {
            mut child,
            stdin,
            pid,
            started,
            served,
            ..
        } = self;
        drop(stdin);
        let status = match tokio::time::timeout(STOP_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => describe(status),
            _ => {
                child.kill().await.ok();
                "killed".to_owned()
            }
        };
        tracing::info!(
            pid,
            why,
            served,
            uptime_s = started.elapsed().as_secs(),
            %status,
            "engine process stopped"
        );
    }

    /// Kill it now (it may be computing; it would not read its stdin).
    async fn abandon(mut self, why: &str) {
        self.kill().await;
        tracing::info!(
            pid = self.pid,
            why,
            served = self.served,
            uptime_s = self.started.elapsed().as_secs(),
            "engine process stopped"
        );
    }

    async fn kill(&mut self) {
        self.child.kill().await.ok();
    }

    /// How it ended (it closed its stdout: it exits or has exited).
    async fn exit_status(&mut self) -> String {
        match tokio::time::timeout(STOP_TIMEOUT, self.child.wait()).await {
            Ok(Ok(status)) => describe(status),
            _ => {
                self.kill().await;
                "closed its output; killed".to_owned()
            }
        }
    }
}

/// An exit status for the log; a SIGKILL is most likely the kernel's OOM
/// killer (the container's memory limit).
fn describe(status: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return if signal == 9 {
                "killed by signal 9 (out of memory?)".to_owned()
            } else {
                format!("killed by signal {signal}")
            };
        }
    }
    status.to_string()
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineError, Synthesizer};
    use crate::phonemize::{Lexicon, Phonemes, parse_symbols};
    use crate::postprocess::Pcm;

    /// Half a second of a tone, whatever it is asked to say.
    struct Tone;

    impl Synthesizer for Tone {
        fn synthesize(
            &mut self,
            _: &Phonemes,
            _: Voice,
            _: Speed,
            _: &Cancel,
        ) -> Result<Pcm, EngineError> {
            let samples = (0..12_000)
                .map(|i| (0.3 * (f64::from(i) * 0.06).sin()) as f32)
                .collect();
            Ok(Pcm::new(samples, 24_000))
        }
    }

    fn local() -> Arc<Local> {
        let lexicon = || Lexicon::parse("hello h ə l ˈ O\n").expect("test setup");
        let symbols = parse_symbols("h 50\nl 54\nO 31\nə 83\nˈ 156\n").expect("test setup");
        Local::new(
            Phonemizer::new(lexicon(), lexicon(), symbols),
            Box::new(Tone),
        )
    }

    #[test]
    fn r40_the_engine_process_says_ready_then_answers_each_line() {
        let requests = [
            r#"{"text": "hello", "voice": "af_heart", "speed": 1.0}"#,
            r#"{"text": "?!", "voice": "bf_emma", "speed": 1.0}"#,
            r#"{"text": "hello", "voice": "xx_none", "speed": 1.0}"#,
            "not json",
        ]
        .join("\n")
            + "\n";
        let mut output = Vec::new();
        let ready = Ready {
            lexicons_ms: 1,
            model_ms: 2,
        };
        serve(requests.as_bytes(), &mut output, &local(), ready).expect("serves");
        let lines: Vec<String> = String::from_utf8(output)
            .expect("utf-8")
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert!(matches!(
            serde_json::from_str(&lines[0]),
            Ok(Hello::Ready(Ready {
                lexicons_ms: 1,
                model_ms: 2
            }))
        ));
        let Ok(Reply::Spoken(spoken)) = serde_json::from_str::<Reply>(&lines[1]) else {
            panic!("{}", &lines[1][..80]);
        };
        assert!(spoken.clip.duration_ms() >= 500);
        assert_eq!(spoken.clip.mp3()[0], 0xFF);
        assert_eq!(spoken.tally.lexicon, 1);
        assert!(matches!(
            serde_json::from_str(&lines[2]),
            Ok(Reply::Refused(Refusal::NothingToSay))
        ));
        for line in &lines[3..] {
            assert!(
                matches!(
                    serde_json::from_str(line),
                    Ok(Reply::Refused(Refusal::Failed(_)))
                ),
                "{line}"
            );
        }
    }

    #[test]
    fn r84_an_overlong_request_line_ends_the_engine_process() {
        let line = "x".repeat(usize::try_from(MAX_REQUEST_LINE).expect("fits") + 10);
        let ready = Ready {
            lexicons_ms: 0,
            model_ms: 0,
        };
        assert!(serve(line.as_bytes(), Vec::new(), &local(), ready).is_err());
    }

    #[test]
    fn r83_refusals_carry_no_text_and_map_back() {
        assert!(matches!(
            SpeakError::from(Refusal::from(SpeakError::TooLong(901))),
            SpeakError::TooLong(901)
        ));
        assert!(matches!(
            SpeakError::from(Refusal::from(SpeakError::NothingToSay)),
            SpeakError::NothingToSay
        ));
        assert!(matches!(
            SpeakError::from(Refusal::from(SpeakError::Engine(EngineError::NoAudio))),
            SpeakError::Worker(message) if message == "the engine produced no audio"
        ));
    }
}
