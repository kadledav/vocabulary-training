//! The binary's subcommands: `check`, `serve` (first-run setup through the
//! logged setup code), `healthcheck`, `reset-password`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_vocab-server");

/// Synthetic password only.
const PASSWORD: &str = "test-password-for-the-cli-tests";

struct Env {
    root: tempfile::TempDir,
    /// Configuration variables (`VOCAB_PUBLIC_ORIGIN`, …).
    vars: Vec<(String, String)>,
}

impl Env {
    /// Nothing exists below the root: the app creates `data/` and
    /// `data/cache/` itself.
    fn new(vars: &[(&str, &str)]) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        Self {
            root,
            vars: vars
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    /// The binary with the test's data directory (the cache directory is
    /// the default one inside it).
    fn command(&self, subcommand: &str) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.arg(subcommand)
            .env_clear()
            .env("VOCAB_DATA_DIR", self.path("data"))
            .envs(self.vars.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, subcommand: &str) -> Output {
        self.command(subcommand).output().expect("run binary")
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port()
}

const ORIGIN: (&str, &str) = ("VOCAB_PUBLIC_ORIGIN", "https://vocab.example.home");

/// Configuration of a server on `127.0.0.1:port`.
fn env_listening(port: u16) -> Env {
    Env::new(&[
        ("VOCAB_PUBLIC_ORIGIN", &format!("http://localhost:{port}")),
        ("VOCAB_LISTEN", &format!("127.0.0.1:{port}")),
        ("VOCAB_LOG_LEVEL", "warn"),
    ])
}

/// One HTTP/1.1 request on a fresh connection; returns the whole response.
fn http(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.write_all(request.as_bytes()).expect("send");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read");
    response
}

/// `POST` of a JSON body as the page sends it; returns the whole response.
fn post(port: u16, path: &str, body: &str, cookie: Option<&str>) -> String {
    let cookie = cookie
        .map(|c| format!("Cookie: {c}\r\n"))
        .unwrap_or_default();
    http(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{port}\r\n\
             Content-Type: application/json\r\n{cookie}Content-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

fn session_cookie(response: &str) -> String {
    response
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .and_then(|value| value.split(';').next())
        .expect("session cookie")
        .to_owned()
}

fn get_today(port: u16, cookie: &str) -> String {
    http(
        port,
        &format!(
            "GET /api/today HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"
        ),
    )
}

/// A running `serve` whose log lines arrive on a channel.
struct Server {
    _child: KillOnDrop,
    lines: mpsc::Receiver<String>,
}

impl Server {
    /// Start `serve` and wait until the health check passes.
    fn start(env: &Env) -> Self {
        let mut child = env
            .command("serve")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn serve");
        let stdout = child.stdout.take().expect("stdout");
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let child = KillOnDrop(child);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !env.run("healthcheck").status.success() {
            assert!(Instant::now() < deadline, "server did not become healthy");
            std::thread::sleep(Duration::from_millis(100));
        }
        Self {
            _child: child,
            lines,
        }
    }

    /// Every log line that arrives within `wait`.
    fn log(&self, wait: Duration) -> Vec<String> {
        let deadline = Instant::now() + wait;
        let mut lines = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.lines.recv_timeout(left) {
                Ok(line) => lines.push(line),
                Err(_) => break,
            }
        }
        lines
    }

    /// The setup code of the WARN line `Setup required: … setup code XXXX-XXXX-XX`.
    fn setup_code(&self) -> String {
        let log = self.log(Duration::from_secs(1));
        let lines: Vec<&String> = log
            .iter()
            .filter(|l| l.contains("Setup required"))
            .collect();
        assert_eq!(lines.len(), 1, "{log:?}");
        assert!(lines[0].contains(" WARN "), "{}", lines[0]);
        let code = lines[0]
            .split("enter setup code ")
            .nth(1)
            .expect("code")
            .trim()
            .to_owned();
        assert_eq!(code.len(), 12, "{code}");
        code
    }
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn r85_check_and_reset_password_need_an_existing_database() {
    // A wrong or empty mount: refused, nothing created.
    let env = Env::new(&[ORIGIN]);
    for subcommand in ["check", "reset-password"] {
        let output = env.run(subcommand);
        assert_eq!(output.status.code(), Some(1), "{subcommand}");
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(
            log.contains("there is no database in"),
            "{subcommand}: {log}"
        );
        assert!(
            !env.path("data").exists(),
            "{subcommand} created the data dir"
        );
        std::fs::create_dir(env.path("data")).expect("mkdir");
        assert!(
            !env.run(subcommand).status.success(),
            "{subcommand} on an empty dir"
        );
        assert!(
            files_in(&env.path("data")).is_empty(),
            "{subcommand} wrote into it"
        );
        std::fs::remove_dir(env.path("data")).expect("rmdir");
    }
}

#[test]
fn r85_serve_creates_private_directories_and_files_and_check_verifies_them() {
    let port = free_port();
    let env = env_listening(port);
    // An existing, world-readable data dir keeps its mode (managed by the
    // NAS), but everything the app creates in it is owner-only (umask 077).
    std::fs::create_dir(env.path("data")).expect("mkdir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(env.path("data"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
    let server = Server::start(&env);
    let files = files_in(&env.path("data"));
    for expected in ["cache", "vocabulary.lock", "vocabulary.sqlite"] {
        assert!(files.iter().any(|f| f == expected), "{files:?}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &str| {
            std::fs::metadata(env.path(p))
                .expect("meta")
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("data"), 0o755, "an existing directory is not changed");
        assert_eq!(mode("data/cache"), 0o700);
        for file in &files {
            if file != "cache" {
                assert_eq!(mode(&format!("data/{file}")), 0o600, "{file}");
            }
        }
    }
    assert!(files_in(&env.path("data/cache")).is_empty());
    drop(server);
    let output = env.run("check");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn r82_reset_password_and_check_refuse_while_the_server_runs() {
    let port = free_port();
    let env = env_listening(port);
    let server = Server::start(&env);
    let code = server.setup_code();
    let body = format!(r#"{{"code":"{code}","password":"{PASSWORD}"}}"#);
    let cookie = session_cookie(&post(port, "/api/auth/setup", &body, None));
    for subcommand in ["reset-password", "check"] {
        let output = env.run(subcommand);
        assert_eq!(output.status.code(), Some(1), "{subcommand}");
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(log.contains("stop the stack first"), "{subcommand}: {log}");
    }
    // Nothing changed behind the server's back.
    assert!(get_today(port, &cookie).starts_with("HTTP/1.1 200"));
    drop(server);
    assert!(env.run("reset-password").status.success());
}

#[test]
fn r85_invalid_config_or_data_dir_stops_the_app() {
    for (vars, expected) in [
        (vec![], "VOCAB_PUBLIC_ORIGIN is not set"),
        (
            vec![ORIGIN, ("VOCAB_BACKUP", "1")],
            "unknown setting VOCAB_BACKUP",
        ),
        (
            vec![ORIGIN, ("VOCAB_CONFIG", "/config/config.json")],
            "unknown setting VOCAB_CONFIG",
        ),
        // The M5 secrets are gone: a leftover .env is a clear error.
        (
            vec![ORIGIN, ("VOCAB_PASSWORD_HASH_B64", "x")],
            "unknown setting VOCAB_PASSWORD_HASH_B64",
        ),
        (
            vec![ORIGIN, ("VOCAB_IMPORT_TOKEN", "x")],
            "unknown setting VOCAB_IMPORT_TOKEN",
        ),
        (
            vec![ORIGIN, ("VOCAB_TIMEZONE", "Nowhere/City")],
            "VOCAB_TIMEZONE",
        ),
        (
            vec![ORIGIN, ("VOCAB_DAY_ROLLOVER_HOUR", "25")],
            "invalid VOCAB_DAY_ROLLOVER_HOUR",
        ),
        (
            vec![ORIGIN, ("VOCAB_LOG_LEVEL", "loud")],
            "invalid VOCAB_LOG_LEVEL",
        ),
        (
            vec![("VOCAB_PUBLIC_ORIGIN", "https://vocab.example.home:443")],
            "invalid VOCAB_PUBLIC_ORIGIN",
        ),
    ] {
        let env = Env::new(&vars);
        for subcommand in ["check", "serve"] {
            let output = env.run(subcommand);
            assert_eq!(output.status.code(), Some(1), "{subcommand} {vars:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains(expected), "{subcommand} {vars:?}: {stderr}");
        }
    }

    // A data directory that cannot be created (a file is in the way).
    let env = Env::new(&[ORIGIN]);
    std::fs::write(env.path("data"), b"not a directory").expect("file");
    let output = env.run("serve");
    assert_eq!(output.status.code(), Some(1));
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(log.contains("storage is not usable"), "{log}");
    assert!(log.contains("cannot create the data directory"), "{log}");
}

#[test]
fn r85_unknown_subcommand_prints_usage() {
    let env = Env::new(&[]);
    let output = env.run("frobnicate");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage: vocab-server"));
}

#[test]
fn r83_serve_answers_and_healthcheck_reports_its_state() {
    let port = free_port();
    let env = env_listening(port);

    // Nothing listens yet: the health check fails.
    assert!(!env.run("healthcheck").status.success());

    let server = Server::start(&env);

    // A real HTTP round trip to the shell.
    let response = http(
        port,
        "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response
            .to_ascii_lowercase()
            .contains("content-security-policy: default-src 'self'")
    );

    // First start: setup state, the code is in the log (once, at WARN).
    let code = server.setup_code();
    let response = http(
        port,
        "GET /api/auth/setup HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(response.ends_with(r#"{"required":true}"#), "{response}");
    let response = http(
        port,
        "GET /api/today HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    let body = format!(r#"{{"code":"{code}","password":"{PASSWORD}"}}"#);
    let response = post(port, "/api/auth/setup", &body, None);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let cookie = session_cookie(&response);
    assert!(get_today(port, &cookie).starts_with("HTTP/1.1 200"));
    // The code is used up and was never logged again.
    let response = post(port, "/api/auth/setup", &body, None);
    assert!(response.starts_with("HTTP/1.1 409"), "{response}");
    let later = server.log(Duration::from_millis(300));
    assert!(later.iter().all(|l| !l.contains(&code)), "{later:?}");

    // The data dir holds the SQLite file set (WAL mode) and the cache dir.
    let files = files_in(&env.path("data"));
    assert!(
        files
            .iter()
            .all(|f| f.starts_with("vocabulary.") || f == "cache"),
        "{files:?}"
    );
    drop(server);

    // A restart once set up: no setup line, the password logs in.
    let server = Server::start(&env);
    let log = server.log(Duration::from_millis(500));
    assert!(log.iter().all(|l| !l.contains("Setup required")), "{log:?}");
    let body = format!(r#"{{"password":"{PASSWORD}"}}"#);
    let response = post(port, "/api/auth/login", &body, None);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(get_today(port, &cookie).starts_with("HTTP/1.1 200"));
    drop(server);

    // reset-password (stack stopped): back to setup state, sessions gone.
    let output = env.run("reset-password");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("next start is in setup state"), "{stdout}");
    let server = Server::start(&env);
    let new_code = server.setup_code();
    assert_ne!(new_code, code, "a fresh code every start");
    assert!(get_today(port, &cookie).starts_with("HTTP/1.1 401"));
    let response = post(port, "/api/auth/login", &body, None);
    assert!(response.starts_with("HTTP/1.1 409"), "{response}");
    drop(server);
}

#[test]
fn r82_serve_shows_the_setup_code_even_when_warnings_are_not_logged() {
    let port = free_port();
    let mut env = env_listening(port);
    env.vars.retain(|(k, _)| k != "VOCAB_LOG_LEVEL");
    env.vars
        .push(("VOCAB_LOG_LEVEL".to_owned(), "error".to_owned()));
    let mut child = env
        .command("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr"));
    let _server = KillOnDrop(child);
    let mut line = String::new();
    stderr.read_line(&mut line).expect("read");
    assert!(
        line.starts_with("Setup required: open the app and enter setup code "),
        "{line}"
    );
}
