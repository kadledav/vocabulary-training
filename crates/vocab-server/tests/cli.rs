//! The binary's subcommands: `check`, `serve`, `healthcheck`, `hash-password`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_vocab-server");

/// Synthetic secrets only.
const PASSWORD: &str = "test-password-for-the-cli-tests";
const IMPORT_TOKEN: &str = "test-import-token-for-the-cli-tests-0123456789";

/// Run `hash-password` with `input` on stdin.
fn hash_password(input: &str) -> Output {
    let mut child = Command::new(BIN)
        .arg("hash-password")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hash-password");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write password");
    child.wait_with_output().expect("hash-password output")
}

/// `VOCAB_PASSWORD_HASH_B64` for [`PASSWORD`], made by the binary itself.
fn password_hash_b64() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        let output = hash_password(&format!("{PASSWORD}\n"));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("utf-8")
            .trim()
            .to_owned()
    })
}

struct Env {
    root: tempfile::TempDir,
    /// Configuration variables (`VOCAB_PUBLIC_ORIGIN`, …).
    vars: Vec<(String, String)>,
}

impl Env {
    fn new(vars: &[(&str, &str)]) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        for dir in ["data", "cache"] {
            std::fs::create_dir(root.path().join(dir)).expect("mkdir");
        }
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

    /// The binary with the test directories and the synthetic secrets.
    fn command(&self, subcommand: &str) -> Command {
        let mut cmd = self.command_without_secrets(subcommand);
        cmd.env("VOCAB_PASSWORD_HASH_B64", password_hash_b64())
            .env("VOCAB_IMPORT_TOKEN", IMPORT_TOKEN);
        cmd
    }

    fn command_without_secrets(&self, subcommand: &str) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.arg(subcommand)
            .env_clear()
            .env("VOCAB_DATA_DIR", self.path("data"))
            .env("VOCAB_CACHE_DIR", self.path("cache"))
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
fn r85_check_creates_and_verifies_the_database_in_the_data_dir() {
    let env = Env::new(&[ORIGIN]);
    let output = env.run("check");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(files_in(&env.path("data")), ["vocabulary.sqlite"]);
    assert!(files_in(&env.path("cache")).is_empty());
    // Second run on the existing database succeeds as well.
    assert!(env.run("check").status.success());
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

    let env = Env::new(&[ORIGIN]);
    std::fs::remove_dir(env.path("data")).expect("rmdir");
    let output = env.run("check");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("database check failed"));
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

    let server = KillOnDrop(
        env.command("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn serve"),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while !env.run("healthcheck").status.success() {
        assert!(Instant::now() < deadline, "server did not become healthy");
        std::thread::sleep(Duration::from_millis(100));
    }

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

    // The API needs a session; logging in with the configured password gives one.
    let response = http(
        port,
        "GET /api/today HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    let body = format!(r#"{{"password":"{PASSWORD}"}}"#);
    let response = http(
        port,
        &format!(
            "POST /api/auth/login HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{port}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    );
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let cookie = response
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .and_then(|value| value.split(';').next())
        .expect("session cookie")
        .to_owned();
    let response = http(
        port,
        &format!(
            "GET /api/today HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"
        ),
    );
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");

    // WAL mode: while serving, the data dir holds exactly the SQLite file set.
    let files = files_in(&env.path("data"));
    assert!(
        files.iter().all(|f| f.starts_with("vocabulary.sqlite")),
        "{files:?}"
    );
    drop(server);
}

#[test]
fn r82_hash_password_prints_a_base64_argon2id_phc_string() {
    let encoded = password_hash_b64();
    assert!(
        encoded
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)),
        "plain base64 without `$`: {encoded}"
    );
    // A Windows line ending and no line ending at all are fine too.
    for input in [format!("{PASSWORD}\r\n"), PASSWORD.to_owned()] {
        let output = hash_password(&input);
        assert!(output.status.success());
        assert_ne!(
            String::from_utf8_lossy(&output.stdout).trim(),
            encoded,
            "fresh salt"
        );
    }
    // Too short (also: empty input) is refused.
    for input in ["", "\n", "short\n"] {
        let output = hash_password(input);
        assert_eq!(output.status.code(), Some(1), "{input:?}");
        assert!(output.stdout.is_empty());
    }
}

/// `serve` must exit with an error, without listening.
fn assert_refuses_to_start(mut cmd: Command, expected: &str) {
    let output = cmd.output().expect("run serve");
    assert_eq!(output.status.code(), Some(1));
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(log.contains(expected), "{log}");
}

#[test]
fn r82_serve_refuses_to_start_without_a_password_hash() {
    let port = free_port();
    let env = env_listening(port);
    let mut cmd = env.command_without_secrets("serve");
    cmd.env("VOCAB_IMPORT_TOKEN", IMPORT_TOKEN);
    assert_refuses_to_start(cmd, "VOCAB_PASSWORD_HASH_B64 is not set");

    let mut cmd = env.command_without_secrets("serve");
    cmd.env("VOCAB_PASSWORD_HASH_B64", "dGVzdC1ub3QtYS1oYXNo")
        .env("VOCAB_IMPORT_TOKEN", IMPORT_TOKEN);
    assert_refuses_to_start(cmd, "VOCAB_PASSWORD_HASH_B64 is not an argon2 PHC string");
}

#[test]
fn r82_serve_refuses_to_start_without_a_long_import_token() {
    let port = free_port();
    let env = env_listening(port);
    let mut cmd = env.command_without_secrets("serve");
    cmd.env("VOCAB_PASSWORD_HASH_B64", password_hash_b64());
    assert_refuses_to_start(cmd, "VOCAB_IMPORT_TOKEN is not set");

    let mut cmd = env.command_without_secrets("serve");
    cmd.env("VOCAB_PASSWORD_HASH_B64", password_hash_b64())
        .env("VOCAB_IMPORT_TOKEN", &IMPORT_TOKEN[..31]);
    assert_refuses_to_start(cmd, "VOCAB_IMPORT_TOKEN must be 32 to 512");
}
