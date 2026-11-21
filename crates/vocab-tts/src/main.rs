//! Command line entry point of the TTS service.
//!
//! Subcommands:
//! * `serve` (default): load the lexicons and the model, check that the
//!   engine speaks, then serve HTTP.
//! * `healthcheck`: `GET /healthz` on the local listener; exit 0 when
//!   healthy (the runtime image has no shell or curl).

use std::process::ExitCode;
use std::time::{Duration, Instant};

use vocab_tts::config::Config;
use vocab_tts::engine::Kokoro;
use vocab_tts::http;
use vocab_tts::phonemize::Phonemizer;
use vocab_tts::speaker::Speaker;
use vocab_tts::speech::{SpeakText, Speed, Voice};

const USAGE: &str = "usage: vocab-tts [serve | healthcheck]

  serve        load the model and serve the TTS API (default)
  healthcheck  GET /healthz on the local listener; exit code 0 when healthy

Configuration (environment): $VOCAB_TTS_LISTEN (default 0.0.0.0:8090),
$VOCAB_TTS_MODEL_DIR (default /model), $VOCAB_TTS_THREADS (default 2),
$VOCAB_TTS_LOG_LEVEL (default info). No secrets.";

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
    let speaker = match load_speaker(&config) {
        Ok(speaker) => speaker,
        Err(message) => {
            tracing::error!("{message}");
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
        let listener = tokio::net::TcpListener::bind(config.listen).await?;
        tracing::info!(listen = %listener.local_addr()?, "serving");
        http::serve(listener, http::app(speaker), shutdown_signal()).await
    });
    match result {
        Ok(()) => {
            tracing::info!("stopped");
            ExitCode::SUCCESS
        }
        Err(err) => {
            tracing::error!(error = %err, "server failed");
            ExitCode::FAILURE
        }
    }
}

/// Lexicons, model, and a self-test of every voice; any failure is a clear
/// startup error.
fn load_speaker(config: &Config) -> Result<std::sync::Arc<Speaker>, String> {
    let started = Instant::now();
    let phonemizer = Phonemizer::load(&config.model_dir)
        .map_err(|err| format!("cannot load the lexicons: {err}"))?;
    let lexicons_ms = started.elapsed().as_millis();
    let started = Instant::now();
    let engine = Kokoro::load(&config.model_dir, config.threads)
        .map_err(|err| format!("cannot load the model: {err}"))?;
    tracing::info!(
        model_dir = %config.model_dir.display(),
        threads = config.threads,
        lexicons_ms,
        model_ms = started.elapsed().as_millis(),
        "model loaded"
    );
    let speaker = Speaker::new(phonemizer, Box::new(engine), WAITING_REQUESTS);
    let text = SpeakText::new(SELF_TEST_TEXT).map_err(|err| err.to_string())?;
    for voice in Voice::ALL {
        let spoken = speaker
            .speak_now(&text, voice, Speed::NORMAL)
            .map_err(|err| format!("self-test with voice {} failed: {err}", voice.id()))?;
        tracing::info!(
            voice = voice.id(),
            audio_ms = spoken.clip.duration_ms(),
            synthesis_ms = spoken.synthesis_ms,
            "self-test passed"
        );
    }
    Ok(speaker)
}

/// Resolves on SIGTERM (`docker stop`) or Ctrl-C.
async fn shutdown_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown requested");
}
