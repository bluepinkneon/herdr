use super::harness::*;
use std::os::unix::fs::PermissionsExt;

// Real PTY + public JSON API + observer socket. The fixture never starts an agent.
#[test]
fn scroll_intent_repaints_observer_and_preserves_pty_geometry() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let shell = base.join("fixture.sh");
    fs::write(&shell, "#!/bin/sh\ni=0\nwhile [ $i -lt 100 ]; do printf 'row-%03d\\r\\n' $i; i=$((i+1)); done\nstty size > \"$SIZE_FILE\"\nprintf 'fixture-ready\\r\\n'\nexec cat\n").unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
    let config = base.join("config");
    let runtime = base.join("runtime");
    let socket = runtime.join("herdr.sock");
    let server = spawn_herdr_with_config(&config, &runtime, &socket, None, &format!(
        "onboarding = false\n[server]\nheadless_cols = 44\nheadless_rows = 46\n[ui]\npane_scrollbars = false\n[terminal]\ndefault_shell = {:?}\n", shell.to_str().unwrap()
    ));
    wait_for_socket(&socket, Duration::from_secs(5));
    let size_file = base.join("size");
    let created = send_request(&socket, &serde_json::json!({"id":"create","method":"workspace.create","params":{"cwd":base,"env":{"SIZE_FILE":size_file}}}).to_string());
    let pane = created["result"]["root_pane"].clone();
    let pane_id = pane["pane_id"].as_str().unwrap();
    assert!(wait_until(
        Duration::from_secs(5),
        Duration::from_millis(20),
        || pane_read_recent_contains(&socket, pane_id, "fixture-ready")
    ));
    assert_eq!(fs::read_to_string(size_file).unwrap().trim(), "46 44");
    let observer = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args([
            "terminal", "session", "observe", pane_id, "--cols", "44", "--rows", "46",
        ])
        .env("HERDR_SOCKET_PATH", &socket)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_RUNTIME_DIR", &runtime)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut observer = Observer(observer);
    let stdout = observer.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if tx.send(value).is_err() {
                break;
            }
        }
    });
    let first = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(first["type"], "terminal.frame");
    assert_eq!(first["width"], 44);
    assert_eq!(first["height"], 46);
    for (lines, offset) in [(-7, 7), (7, 0)] {
        let request = serde_json::json!({"id":"scroll","method":"pane.scroll_intent","params":{
            "pane_id":pane_id,"terminal_id":pane["terminal_id"],"cols":44,"rows":46,
            "column":21,"row":32,"lines":lines,
            "expires_at_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64 + 200
        }});
        let response = send_request(&socket, &request.to_string());
        assert_eq!(response["result"]["type"], "ok", "{response}");
        let frame = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(frame["type"], "terminal.frame");
        assert_eq!(frame["width"], 44);
        assert_eq!(frame["height"], 46);
        let got = send_request(
            &socket,
            &serde_json::json!({"id":"get","method":"pane.get","params":{"pane_id":pane_id}})
                .to_string(),
        );
        assert_eq!(
            got["result"]["pane"]["scroll"]["offset_from_bottom"],
            offset
        );
        assert_eq!(got["result"]["pane"]["terminal_id"], pane["terminal_id"]);
    }
    observer.0.kill().unwrap();
    observer.0.wait().unwrap();
    reader.join().unwrap();
    cleanup_spawned_herdr(server, base);
}

struct Observer(std::process::Child);
impl Drop for Observer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
