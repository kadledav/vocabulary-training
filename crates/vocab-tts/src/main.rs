//! Command line entry point of the TTS service.
//!
//! Subcommands:
//! * `serve` (default): the HTTP front; it checks once that the engine
//!   speaks (a self-test in an engine process), then serves HTTP. The engine
//!   process runs only while there is work (`vocab_tts::worker`). SIGTERM
//!   or Ctrl-C (also during the self-test) kills the engine process at once,
//!   answers a request in progress with `503 shutting_down`, and exits.
//! * `worker`: the engine process (started by `serve`, not by hand).
//! * `healthcheck`: `GET /healthz` on the local listener; exit 0 when
//!   healthy (the runtime image has no shell or curl).

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use vocab_tts::config::Config;
use vocab_tts::engine::Cancel;
use vocab_tts::http;
use vocab_tts::speaker::Speaker;
use vocab_tts::speech::{SpeakText, Speed, Voice};
use vocab_tts::worker::{self, Worker, WorkerCommand};

const USAGE: &str = "usage: vocab-tts [serve | healthcheck]

  serve        serve the TTS API (default); the model is loaded in an
               engine process while there is work
  healthcheck  GET /healthz on the local listener; exit code 0 when healthy

Configuration (environment): $VOCAB_TTS_LISTEN (default 0.0.0.0:8090),
$VOCAB_TTS_MODEL_DIR (default /model), $VOCAB_TTS_THREADS (default 1),
$VOCAB_TTS_IDLE_SECONDS (default 180), $VOCAB_TTS_LOG_LEVEL (default info).
No secrets.";

/// Requests that may wait while one is being synthesised (the app sends
/// one at a time; a few more absorb retries and manual checks).
const WAITING_REQUESTS: usize = 4;
/// Spoken once per voice at start: proves model, voices and lexicons work.
const SELF_TEST_TEXT: &str = "Hello.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] | ["serve"] => serve(),
        [worker::SUBCOMMAND] => run_worker(),
        ["healthcheck"] => healthcheck(),
        ["help" | "-h" | "--help"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        ["--version" | "-V"] => {
            println!("vocab-tts {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn load_config() -> Result<Config, ExitCode> {
    Config::load().map_err(|err| {
        eprintln!("vocab-tts: {err}");
        ExitCode::FAILURE
    })
}

fn healthcheck() -> ExitCode {
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    match http::healthcheck(config.listen, Duration::from_secs(3)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("vocab-tts healthcheck: {err}");
            ExitCode::FAILURE
        }
    }
}

/// The engine process: its stdout is the protocol, so it logs to stderr.
fn run_worker() -> ExitCode {
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    tracing_subscriber::fmt()
        .with_max_level(config.log_level)
        .with_target(false)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
    worker::run(&config.model_dir, config.threads)
}

fn serve() -> ExitCode {
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    tracing_subscriber::fmt()
        .with_max_level(config.log_level)
        .with_target(false)
        .with_ansi(false)
        .init();
    let command = match WorkerCommand::this_binary() {
        Ok(command) => command,
        Err(err) => {
            tracing::error!(error = %err, "cannot find this program to start the engine process");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!(error = %err, "cannot start the async runtime");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async {
        // Before anything else: a stop during the self-test must work too
        // (as PID 1 of a container, a signal without a handler is ignored).
        let stop = stop_requested()?;
        let worker = Worker::new(command, config.idle);
        worker.watch_idle();
        let speaker = Speaker::new(Arc::clone(&worker) as _, WAITING_REQUESTS);
        let tested = tokio::select! {
            tested = self_test(&speaker) => tested,
            () = stop.cancelled() => {
                speaker.shut_down();
                worker.release().await;
                tracing::info!("stopped during the start self-test");
                return Ok(true);
            }
        };
        if let Err(message) = tested {
            tracing::error!("{message}");
            return Ok(false);
        }
        // Nothing to do yet: give the memory back until the first request.
        worker.release().await;
        let listener = tokio::net::TcpListener::bind(config.listen).await?;
        tracing::info!(
            listen = %listener.local_addr()?,
            threads = config.threads,
            idle_s = config.idle.as_secs(),
            "serving"
        );
        let shutdown = {
            let speaker = Arc::clone(&speaker);
            async move {
                stop.cancelled().await;
                // Stops the engine now: the request in progress gets its
                // answer (503 shutting_down) before the connections close.
                speaker.shut_down();
            }
        };
        http::serve(listener, http::app(speaker), shutdown).await?;
        worker.release().await;
        Ok::<_, std::io::Error>(true)
    });
    match result {
        Ok(true) => {
            tracing::info!("stopped");
            ExitCode::SUCCESS
        }
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            tracing::error!(error = %err, "server failed");
            ExitCode::FAILURE
        }
    }
}

/// Every voice speaks once, in an engine process, through the same path as
/// a request: lexicons, model and the process protocol work.
async fn self_test(speaker: &Arc<Speaker>) -> Result<(), String> {
    for voice in Voice::ALL {
        let text = SpeakText::new(SELF_TEST_TEXT).map_err(|err| err.to_string())?;
        let spoken = speaker
            .speak(text, voice, Speed::NORMAL)
            .await
            .map_err(|err| format!("self-test with voice {} failed: {err}", voice.id()))?;
        tracing::info!(
            voice = voice.id(),
            audio_ms = spoken.clip.duration_ms(),
            synthesis_ms = spoken.synthesis_ms,
            "self-test passed"
        );
    }
    Ok(())
}

/// Set on SIGTERM (`docker stop`) or Ctrl-C (SIGINT). The handlers are in
/// place when this returns.
fn stop_requested() -> std::io::Result<Cancel> {
    let stop = Cancel::new();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        let stop = stop.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = terminate.recv() => {},
                _ = interrupt.recv() => {},
            }
            tracing::info!("shutdown requested");
            stop.cancel();
        });
    }
    #[cfg(not(unix))]
    {
        let stop = stop.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!("shutdown requested");
                stop.cancel();
            }
        });
    }
    Ok(stop)
}
