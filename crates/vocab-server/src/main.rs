//! Command line entry point: wires the modules together.
//!
//! Subcommands (design §3.2):
//! * `serve` (default): open/check/migrate the database, serve HTTP.
//! * `healthcheck`: `GET /healthz` on the local listener; exit 0 when healthy
//!   (the runtime image has no shell or curl).
//! * `check`: open the existing database, run `quick_check` and migrations,
//!   exit.
//! * `reset-password`: with the app stopped, clear the password, every
//!   session, the connector token and the failed-login counters; the next
//!   start is in setup state.
//! * `logout-all`: with the app stopped, end every session (the password
//!   and the connector token stay). A running app has the same as
//!   "Log out everywhere" in Settings.
//!
//! `serve`, `check`, `reset-password` and `logout-all` lock the data
//! directory, so the last three refuse to run next to a running server.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use vocab_server::auth::{self, Auth, SessionLifetime, SetupCode};
use vocab_server::clock::{Moment, Timestamp};
use vocab_server::config::Config;
use vocab_server::db::{DataDirLock, Db, DbError};
use vocab_server::http::{self, AppState};
use vocab_server::workflows;

/// While the app waits to be set up, the setup code is logged again this
/// often, so it is still in the log after the log has rotated.
const SETUP_REMINDER_EVERY: Duration = Duration::from_secs(60 * 60);

const USAGE: &str =
    "usage: vocab-server [serve | healthcheck | check | reset-password | logout-all]

  serve           open the database (integrity check + migrations) and serve HTTP (default)
  healthcheck     GET /healthz on the local listener; exit code 0 when healthy
  check           open the existing database (integrity check + migrations) and exit
  reset-password  with the app stopped: clear the password, every session, the
                  connector token and the failed-login counters; the next start
                  asks for a new password (setup code in the log)
  logout-all      with the app stopped: end every session; the password and the
                  connector token stay

Configuration (environment): $VOCAB_PUBLIC_ORIGIN (required), $VOCAB_TIMEZONE
(default Europe/Prague), $VOCAB_DAY_ROLLOVER_HOUR (default 4), $VOCAB_LOG_LEVEL
(default info), $VOCAB_LISTEN (default 0.0.0.0:8080), $VOCAB_DATA_DIR (default
/data), $VOCAB_CACHE_DIR (default <data dir>/cache). No secrets: the password
and the connector token are set in the app.";

fn main() -> ExitCode {
    // Everything the app creates (database, WAL, lock, cache files and
    // directories) is for its own user only, also when /data already exists
    // with a wider mode. Existing directories are not changed.
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
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
        ["reset-password"] => reset_password(),
        ["logout-all"] => logout_all(),
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

/// Create the data and cache directories if needed, then open the database.
/// How a subcommand opens the database.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Open {
    /// `serve`: create the directories and the database when missing.
    Create,
    /// `check`, `reset-password`, `logout-all`: only an existing database
    /// (a wrong mount must not look like success).
    Existing,
}

/// Lock the data directory (refused while another process holds it), then
/// open the database. The lock lasts as long as the returned guard.
fn open_db(config: &Config, open: Open) -> Result<(DataDirLock, Db), ExitCode> {
    let data_dir = &config.paths.data_dir;
    let fail = |err: DbError| {
        tracing::error!(error = %err, data_dir = %data_dir.display(), "database check failed");
        ExitCode::FAILURE
    };
    match open {
        Open::Create => {
            if let Err(err) = config.paths.prepare() {
                tracing::error!(error = %err, "storage is not usable");
                return Err(ExitCode::FAILURE);
            }
        }
        Open::Existing => {
            if !Db::exists(data_dir) {
                return Err(fail(DbError::NoDatabase(data_dir.clone())));
            }
        }
    }
    let lock = DataDirLock::acquire(data_dir).map_err(fail)?;
    let db = match open {
        Open::Create => Db::open(data_dir),
        Open::Existing => Db::open_existing(data_dir),
    }
    .map_err(fail)?;
    Ok((lock, db))
}

fn check() -> ExitCode {
    let result = load_config().and_then(|config| {
        init_logging(&config);
        open_db(&config, Open::Existing)
    });
    match result {
        Ok(_opened) => {
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

/// Run one maintenance operation on the database of a stopped app: the
/// data directory is locked for the duration, so it is refused next to a
/// running server.
fn offline<T, F>(command: &str, work: impl FnOnce(Db) -> F) -> Result<T, ExitCode>
where
    F: std::future::Future<Output = Result<T, DbError>>,
{
    let (_lock, db) = load_config().and_then(|config| {
        init_logging(&config);
        open_db(&config, Open::Existing)
    })?;
    tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|err| err.to_string())
        .and_then(|runtime| runtime.block_on(work(db)).map_err(|err| err.to_string()))
        .map_err(|err| {
            eprintln!("vocab-server {command}: {err}");
            ExitCode::FAILURE
        })
}

/// Back to setup state (deploy/README.md: run it with the stack stopped).
fn reset_password() -> ExitCode {
    match offline("reset-password", |db| async move {
        auth::reset_password(&db).await
    }) {
        Ok(()) => {
            println!(
                "The password, every session and the connector token are cleared. The next \
                 start is in setup state: open the app and enter the setup code from the log \
                 (docker logs vocabulary), then create a new connector token in Settings."
            );
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
}

/// End every session (run it with the stack stopped).
fn logout_all() -> ExitCode {
    match offline("logout-all", |db| async move {
        auth::logout_everywhere(&db).await
    }) {
        Ok(ended) => {
            println!(
                "Every session is ended ({ended} in all): each device has to log in again. \
                 The password and the connector token are unchanged."
            );
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
}

fn serve() -> ExitCode {
    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };
    init_logging(&config);
    let lifetime = SessionLifetime {
        sliding_days: config.session_days,
        max_days: config.session_max_days,
    };
    let (auth, setup_code) = match Auth::new(&config.public_origin, lifetime) {
        Ok(auth) => auth,
        Err(err) => {
            tracing::error!(error = %err, "cannot make the setup code");
            return ExitCode::FAILURE;
        }
    };
    // Held until `serve` returns: no other process may use the database.
    let (_lock, db) = match open_db(&config, Open::Create) {
        Ok(opened) => opened,
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
    match runtime.block_on(run_server(config, db, auth, setup_code)) {
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

async fn run_server(
    config: Config,
    db: Db,
    auth: Auth,
    setup_code: SetupCode,
) -> std::io::Result<()> {
    let setup_required = auth
        .setup_required(&db)
        .await
        .map_err(|err| std::io::Error::other(format!("cannot read the owner's password: {err}")))?;
    let auth = Arc::new(auth);
    if setup_required {
        announce_setup(&setup_code);
        // Said again every hour until a password is set.
        let (auth, db) = (Arc::clone(&auth), db.clone());
        tokio::spawn(async move {
            auth.remind_setup(&db, SETUP_REMINDER_EVERY, || announce_setup(&setup_code))
                .await;
        });
    }
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
    let app = http::app(
        AppState::new(db, config.day_rule.clone(), auth),
        &config.limits,
    );
    http::serve(listener, app, &config.limits, shutdown_signal()).await
}

/// The one place the setup code is shown: a WARN line of the log (also on
/// stderr when the log level hides warnings, so it can never be lost),
/// written at the start and then hourly while the app waits to be set up.
fn announce_setup(code: &SetupCode) {
    let line = format!("Setup required: open the app and enter setup code {code}");
    if tracing::enabled!(tracing::Level::WARN) {
        tracing::warn!("{line}");
    } else {
        eprintln!("{line}");
    }
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
