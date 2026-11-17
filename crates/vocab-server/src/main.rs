//! Command line entry point: wires the modules together.
//!
//! Subcommands (design §3.2):
//! * `serve` (default): open/check/migrate the database, serve HTTP.
//! * `healthcheck`: `GET /healthz` on the local listener; exit 0 when healthy
//!   (the runtime image has no shell or curl).
//! * `check`: open the database, run `quick_check` and migrations, exit.

use std::process::ExitCode;
use std::time::Duration;

use vocab_server::clock::{Moment, Timestamp};
use vocab_server::config::Config;
use vocab_server::db::Db;
use vocab_server::http::{self, AppState};
use vocab_server::workflows;

const USAGE: &str = "usage: vocab-server [serve | healthcheck | check]

  serve        open the database (integrity check + migrations) and serve HTTP (default)
  healthcheck  GET /healthz on the local listener; exit code 0 when healthy
  check        open the database (integrity check + migrations) and exit

Configuration: $VOCAB_CONFIG (default /config/config.json),
$VOCAB_DATA_DIR (default /data), $VOCAB_CACHE_DIR (default /cache).";

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
        ["check"] => check(),
        ["help" | "-h" | "--help"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        ["--version" | "-V"] => {
            println!("vocab-server {}", env!("CARGO_PKG_VERSION"));
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
        eprintln!("vocab-server: {err}");
        ExitCode::FAILURE
    })
}

fn init_logging(config: &Config) {
    tracing_subscriber::fmt()
        .with_max_level(config.log_level)
        .with_target(false)
        .with_ansi(false)
        .init();
}

fn open_db(config: &Config) -> Result<Db, ExitCode> {
    Db::open(&config.paths.data_dir).map_err(|err| {
        tracing::error!(error = %err, data_dir = %config.paths.data_dir.display(), "database check failed");
        ExitCode::FAILURE
    })
}

fn check() -> ExitCode {
    let result = load_config().and_then(|config| {
        init_logging(&config);
        open_db(&config)
    });
    match result {
        Ok(_db) => {
            tracing::info!("database check passed");
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
}

fn healthcheck() -> ExitCode {
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    match http::healthcheck(config.listen, Duration::from_secs(3)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("vocab-server healthcheck: {err}");
            ExitCode::FAILURE
        }
    }
}

fn serve() -> ExitCode {
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    init_logging(&config);
    let db = match open_db(&config) {
        Ok(db) => db,
        Err(code) => return code,
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!(error = %err, "cannot start the async runtime");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run_server(config, db)) {
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

async fn run_server(config: Config, db: Db) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(
        listen = %listener.local_addr()?,
        time_zone = config.day_rule.time_zone_name().unwrap_or("?"),
        "serving"
    );
    let now = Moment::of(Timestamp::now(), &config.day_rule);
    match workflows::ensure_all_cards(&db, now).await {
        Ok(0) => {}
        Ok(created) => tracing::info!(created, "created missing cards"),
        Err(err) => {
            return Err(std::io::Error::other(format!(
                "card backfill failed: {err}"
            )));
        }
    }
    let app = http::app(AppState::new(db, config.day_rule.clone()), &config.limits);
    http::serve(listener, app, &config.limits, shutdown_signal()).await
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
