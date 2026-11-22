// SPDX-License-Identifier: MIT
//! `--config` through the real binary, where the environment can be set:
//! precedence of the command line, SPROCKET_BAUD and SPROCKET_PORT, the
//! file and defaults.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

fn sprocket() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sprocket"));
    command
        .env_remove("SPROCKET_BAUD")
        .env_remove("SPROCKET_PORT");
    command
}

/// The baud rate `sprocket serve` announces, from its first log line. The
/// port is a TCP address nothing listens on, so it only retries.
fn served_baud(config: Option<&str>, env: Option<&str>, args: &[&str]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let mut command = sprocket();
    command.args(["serve", "--port", "tcp://127.0.0.1:1"]);
    if let Some(toml) = config {
        let path = dir.path().join("sprocket.toml");
        let toml = format!("{toml}\nroot = {:?}\n", dir.path().to_str().unwrap());
        std::fs::write(&path, toml).unwrap();
        command.arg("--config").arg(&path);
    } else {
        command.arg("--root").arg(dir.path());
    }
    if let Some(baud) = env {
        command.env("SPROCKET_BAUD", baud);
    }
    let mut child = command.args(args).stderr(Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    let (_, rest) = line.split_once(" (").unwrap_or_else(|| panic!("{line}"));
    rest.split_once(' ').unwrap().0.to_string()
}

#[test]
fn baud_precedence() {
    let file = Some("[serve]\nbaud = 57600");
    assert_eq!(served_baud(None, None, &[]), "9600");
    assert_eq!(served_baud(None, Some("19200"), &[]), "19200");
    assert_eq!(served_baud(Some("[serve]"), None, &[]), "9600");
    assert_eq!(served_baud(file, None, &[]), "57600");
    assert_eq!(served_baud(file, Some("19200"), &[]), "19200");
    assert_eq!(
        served_baud(file, Some("19200"), &["--baud", "38400"]),
        "38400"
    );
    // The environment wins even when the file gives the default.
    assert_eq!(
        served_baud(Some("[serve]\nbaud = 9600"), Some("2400"), &[]),
        "2400"
    );
}

#[test]
fn exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.toml");
    let out = sprocket()
        .args(["hello", "--config"])
        .arg(&missing)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot read config"), "{stderr}");

    let empty = dir.path().join("empty.toml");
    std::fs::write(&empty, "").unwrap();
    let out = sprocket()
        .args(["serve", "--config"])
        .arg(&empty)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--root is required"), "{stderr}");

    let bad = dir.path().join("bad.toml");
    std::fs::write(&bad, "[serve]\nroot = \"r\"\nwritable = \"yes\"\n").unwrap();
    let out = sprocket()
        .args(["serve", "--config"])
        .arg(&bad)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("line 3"), "{stderr}");
}

/// The port `sprocket hello` failed to open: a path in a directory that
/// does not exist, so it fails at once and the message names it.
fn hello_port(config: Option<&str>, env: Option<&str>, args: &[&str]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let mut command = sprocket();
    command.args(["hello", "--wait", "0"]);
    if let Some(toml) = config {
        let path = dir.path().join("sprocket.toml");
        std::fs::write(&path, toml).unwrap();
        command.arg("--config").arg(&path);
    }
    if let Some(port) = env {
        command.env("SPROCKET_PORT", port);
    }
    let out = command.args(args).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr
        .lines()
        .find_map(|l| l.strip_prefix("Error: cannot open "))
        .unwrap_or_else(|| panic!("{stderr}"));
    line.to_string()
}

#[test]
fn port_precedence() {
    let file = Some("[hello]\nport = \"/no/such/file-port\"\n");
    let env = Some("/no/such/env-port");
    let cli = ["--port", "/no/such/cli-port"];
    assert_eq!(hello_port(None, env, &[]), "/no/such/env-port");
    assert_eq!(hello_port(None, env, &cli), "/no/such/cli-port");
    assert_eq!(hello_port(file, None, &[]), "/no/such/file-port");
    assert_eq!(hello_port(file, env, &[]), "/no/such/env-port");
    assert_eq!(hello_port(file, env, &cli), "/no/such/cli-port");
}

#[test]
fn empty_port_is_refused() {
    // As with an empty SPROCKET_BAUD, rather than falling back.
    let out = sprocket()
        .args(["hello", "--wait", "0"])
        .env("SPROCKET_PORT", "")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--port <PORT>"), "{stderr}");
}

#[test]
fn port_from_the_environment_for_serve() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = sprocket()
        .args(["serve", "--root"])
        .arg(dir.path())
        .env("SPROCKET_PORT", "tcp://127.0.0.1:1")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(line.contains(" on tcp://127.0.0.1:1 ("), "{line}");
}
