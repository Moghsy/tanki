//! Serve lifecycle on synthetic data, including signals, lock and config overrides.
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn command(base: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tanki"));
    cmd.env("HOME", base.join("fake-home"))
        .env("XDG_CONFIG_HOME", base.join("config"))
        .env_remove("ANKI_BASE")
        .env_remove("XDG_DATA_HOME")
        .args(["--base", base.to_str().unwrap(), "--profile", "fixture"])
        .stdin(Stdio::null());
    cmd
}
#[test]
fn config_overrides_bind_key_cors_and_sigint_releases_collection() {
    let base = tempfile::tempdir_in("/tmp").unwrap();
    let profile = base.path().join("fixture");
    std::fs::create_dir(&profile).unwrap();
    let actor = tanki_core::CollectionActor::open(&profile).unwrap();
    actor.execute(tanki_core::Command::GenerateFixture).unwrap();
    actor.close().unwrap();
    let config = base.path().join("config.toml");
    std::fs::write(
        &config,
        "[connect]\nbind = '127.0.0.1:1'\napi_key = 'old'\n",
    )
    .unwrap();
    let mut child = Server(
        command(base.path())
            .args([
                "--config",
                config.to_str().unwrap(),
                "--json",
                "serve",
                "--bind",
                "127.0.0.1:0",
                "--api-key",
                "new",
                "--allow-origin",
                "https://mining.test",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let ready: Value = serde_json::from_str(&line).unwrap();
    let addr = ready["address"].as_str().unwrap();
    let mut socket = TcpStream::connect(addr).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let body = json!({"action":"version","version":6,"key":"new"}).to_string();
    write!(socket,"POST / HTTP/1.1\r\nHost: localhost\r\nOrigin: https://mining.test\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    assert!(response.contains("\"result\":6"), "{response}");
    let locked = command(base.path()).arg("decks").output().unwrap();
    assert_eq!(locked.status.code(), Some(20), "{locked:?}");
    assert!(Command::new("kill")
        .args(["-INT", &child.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "serve did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
    let actor = tanki_core::CollectionActor::open(profile).unwrap();
    actor.close().unwrap();
}
