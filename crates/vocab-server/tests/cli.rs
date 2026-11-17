//! The binary's subcommands: `check`, `serve`, `healthcheck`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_vocab-server");

struct Env {
    root: tempfile::TempDir,
}

impl Env {
    fn new(config: &str) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        for dir in ["data", "cache", "config"] {
            std::fs::create_dir(root.path().join(dir)).expect("mkdir");
        }
        std::fs::write(root.path().join("config/config.json"), config).expect("config");
        Self { root }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn command(&self, subcommand: &str) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.arg(subcommand)
            .env_clear()
            .env("VOCAB_CONFIG", self.path("config/config.json"))
            .env("VOCAB_DATA_DIR", self.path("data"))
            .env("VOCAB_CACHE_DIR", self.path("cache"))
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

fn config_with_listen(port: u16) -> String {
    format!(
        r#"{{"public_origin": "http://localhost:{port}", "listen": "127.0.0.1:{port}", "log_level": "warn"}}"#
    )
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
    let env = Env::new(r#"{"public_origin": "https://vocab.example.home"}"#);
    let output = env.run("check");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(files_in(&env.path("data")), ["vocabulary.sqlite"]);
    assert!(files_in(&env.path("cache")).is_empty());
    assert_eq!(files_in(&env.path("config")), ["config.json"]);
    // Second run on the existing database succeeds as well.
    assert!(env.run("check").status.success());
}

#[test]
fn r85_invalid_config_or_data_dir_stops_the_app() {
    let env = Env::new(r#"{"public_origin": "https://vocab.example.home", "backup": true}"#);
    let output = env.run("check");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown field"));

    let env = Env::new(r#"{"public_origin": "https://vocab.example.home"}"#);
    std::fs::remove_dir(env.path("data")).expect("rmdir");
    let output = env.run("check");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("database check failed"));
}

#[test]
fn r85_unknown_subcommand_prints_usage() {
    let env = Env::new("{}");
    let output = env.run("frobnicate");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage: vocab-server"));
}

#[test]
fn r83_serve_answers_and_healthcheck_reports_its_state() {
    let port = free_port();
    let env = Env::new(&config_with_listen(port));

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
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .expect("send");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response
            .to_ascii_lowercase()
            .contains("content-security-policy: default-src 'self'")
    );

    // WAL mode: while serving, the data dir holds exactly the SQLite file set.
    let files = files_in(&env.path("data"));
    assert!(
        files.iter().all(|f| f.starts_with("vocabulary.sqlite")),
        "{files:?}"
    );
    drop(server);
}
