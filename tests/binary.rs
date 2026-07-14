//! Binary integration tests for d2b-clip-picker.
//!
//! These tests spawn the compiled picker binary directly and exercise the
//! ADR 0042 protocol contract at the process boundary, verifying that the
//! binary exits non-zero on malformed or unexpected input rather than
//! silently succeeding.

use std::io::{BufRead, BufReader, Write};
use std::os::fd::BorrowedFd;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

fn spawn_picker() -> (UnixStream, Child) {
    spawn_picker_with_args(&[])
}

fn spawn_picker_with_args(extra_args: &[&str]) -> (UnixStream, Child) {
    let (parent, child) = UnixStream::pair().expect("socketpair");
    let child_fd = child.as_raw_fd();

    let mut command = Command::new(env!("CARGO_BIN_EXE_d2b-clip-picker"));
    command.arg("--ipc-fd").arg(child_fd.to_string());
    command.args(extra_args);
    unsafe {
        command.pre_exec(move || {
            let borrowed = BorrowedFd::borrow_raw(child_fd);
            let flags = rustix::io::fcntl_getfd(borrowed)
                .map_err(|err| std::io::Error::from_raw_os_error(err.raw_os_error()))?;
            rustix::io::fcntl_setfd(borrowed, flags - rustix::io::FdFlags::CLOEXEC)
                .map_err(|err| std::io::Error::from_raw_os_error(err.raw_os_error()))
        });
    }

    let child_proc = command.spawn().expect("spawn d2b-clip-picker");

    drop(child);
    (parent, child_proc)
}

fn read_client_hello(parent: &UnixStream) {
    parent
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .expect("set timeout");
    let mut reader = BufReader::new(parent.try_clone().expect("clone socket"));
    let mut hello = String::new();
    reader.read_line(&mut hello).expect("read client_hello");
    assert!(
        hello.contains("client_hello"),
        "picker must send client_hello first, got: {hello}"
    );
}

fn send_wayland_open_request(parent: &UnixStream, request_id: &str) {
    let request = serde_json::json!({
        "type": "open_request",
        "selected_protocol_version": 2,
        "clipd_version": "test",
        "picker_version": "test",
        "request_id": request_id,
        "destination": {
            "realm": "test",
            "realm_kind": "vm",
            "provider_kind": "local-vm",
            "isolation_posture": "virtual-machine"
        },
        "requested_mime_type": "text/plain",
        "expires_at_unix_ms": null,
        "placement_hints": {
            "pointer_x": 100.0,
            "pointer_y": 100.0,
            "output_width": 1280,
            "output_height": 720,
            "overlay_width": 420,
            "overlay_height": 520
        },
        "candidates": [{
            "entry_id": "wayland-entry",
            "source_realm": "host",
            "source_realm_kind": "host",
            "source_provider_kind": "unsafe-local",
            "source_isolation_posture": "unsafe-local",
            "source_attribution": "exact_client",
            "preview_text": "synthetic validation",
            "content_type": "text/plain"
        }],
        "realm_display": {}
    });
    writeln!(
        parent.try_clone().expect("clone socket for write"),
        "{request}"
    )
    .expect("write open request");
}

fn niri_json(command: &str) -> serde_json::Value {
    let output = Command::new("niri")
        .args(["msg", "-j", command])
        .output()
        .expect("run niri test harness query");
    assert!(
        output.status.success(),
        "niri {command} query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse niri JSON")
}

fn wait_for_niri_window(app_id: &str) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(id) = niri_json("windows")
            .as_array()
            .expect("niri windows array")
            .iter()
            .find(|window| window["app_id"] == app_id)
            .and_then(|window| window["id"].as_u64())
        {
            return id;
        }
        assert!(
            Instant::now() < deadline,
            "focus-stealing foot window never mapped"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_niri_window_focus(window_id: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let focused = niri_json("windows")
            .as_array()
            .expect("niri windows array")
            .iter()
            .any(|window| window["id"] == window_id && window["is_focused"] == true);
        if focused {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "fresh Niri toplevel {window_id} did not receive focus"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn niri_window_action(action: &str, window_id: u64) {
    let status = Command::new("niri")
        .args(["msg", "action", action, "--id"])
        .arg(window_id.to_string())
        .status()
        .expect("run niri test harness action");
    assert!(status.success(), "niri {action} failed");
}

/// Picker must exit non-zero when it receives a malformed / unrecognised
/// clipd frame type.
///
/// The picker uses `serde(deny_unknown_fields)` on `ClipdFrame` so any frame
/// that cannot be deserialized as a known variant propagates an error through
/// `run()` which calls `process::exit(1)`.
#[test]
fn picker_exits_nonzero_on_malformed_clipd_frame() {
    let (parent, mut child_proc) = spawn_picker();
    read_client_hello(&parent);

    // Send a frame with an unknown `type` field. ClipdFrame uses
    // `deny_unknown_fields` + `serde(tag = "type")`, so this will fail
    // deserialization and the picker must exit non-zero.
    let mut writer = parent.try_clone().expect("clone socket for write");
    writeln!(
        writer,
        r#"{{"type":"totally_bogus_type_that_does_not_exist_in_clipd_frame"}}"#
    )
    .expect("write malformed frame");
    drop(writer);
    drop(parent);

    let status = child_proc.wait().expect("wait for picker");
    assert!(
        !status.success(),
        "picker must exit non-zero on receiving an unknown/malformed clipd frame type; \
         exit code was: {:?}",
        status.code()
    );
}

#[test]
fn picker_exits_nonzero_on_clipd_error_frame() {
    let (parent, mut child_proc) = spawn_picker();
    read_client_hello(&parent);

    let mut writer = parent.try_clone().expect("clone socket for write");
    writeln!(
        writer,
        r#"{{"type":"error","selected_protocol_version":1,"request_id":"req","code":"picker_not_configured"}}"#
    )
    .expect("write error frame");
    drop(writer);
    drop(parent);

    let status = child_proc.wait().expect("wait for picker");
    assert!(
        !status.success(),
        "picker must exit non-zero when clipd rejects the request; exit code was: {:?}",
        status.code()
    );
}

#[test]
fn picker_exits_zero_on_clipd_close_frame() {
    let (parent, mut child_proc) = spawn_picker();
    read_client_hello(&parent);

    let mut writer = parent.try_clone().expect("clone socket for write");
    writeln!(
        writer,
        r#"{{"type":"close","selected_protocol_version":1,"request_id":"req","code":"request_expired"}}"#
    )
    .expect("write close frame");
    drop(writer);
    drop(parent);

    let status = child_proc.wait().expect("wait for picker");
    assert!(
        status.success(),
        "picker must exit cleanly on an explicit clipd close frame; exit code was: {:?}",
        status.code()
    );
}

/// Picker must exit non-zero when clipd closes the socket immediately without
/// sending an open_request (e.g., a too-old-version rejection path).
#[test]
fn picker_exits_nonzero_on_immediate_socket_close() {
    let (parent, mut child_proc) = spawn_picker();
    read_client_hello(&parent);

    // Close the parent socket immediately — simulates clipd hanging up.
    drop(parent);

    let status = child_proc.wait().expect("wait");
    assert!(
        !status.success(),
        "picker must exit non-zero when clipd disconnects without a Close frame; exit code was: {:?}",
        status.code()
    );
}

#[test]
fn wayland_picker_maps_and_preserves_legacy_select_roundtrip() {
    if std::env::var_os("D2B_RUN_WAYLAND_UI_TESTS").is_none() {
        return;
    }
    let (parent, mut child_proc) = spawn_picker_with_args(&["--test-select-first"]);
    read_client_hello(&parent);

    send_wayland_open_request(&parent, "wayland-request");

    parent
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .expect("set response timeout");
    let mut select = String::new();
    BufReader::new(parent.try_clone().expect("clone socket"))
        .read_line(&mut select)
        .expect("read select");
    let response: serde_json::Value = serde_json::from_str(select.trim_end()).expect("select json");
    assert_eq!(response["type"], "select");
    assert_eq!(response["request_id"], "wayland-request");
    assert_eq!(response["entry_id"], "wayland-entry");
    assert!(child_proc.wait().expect("wait for picker").success());
}

#[test]
fn niri_focus_bootstrap_transitions_then_cancels_once() {
    if std::env::var_os("D2B_RUN_NIRI_FOCUS_TESTS").is_none() {
        return;
    }
    assert!(
        std::env::var_os("NIRI_SOCKET").is_some(),
        "NIRI_SOCKET is required only by this test harness"
    );

    let (parent, mut picker) = spawn_picker();
    read_client_hello(&parent);
    send_wayland_open_request(&parent, "niri-focus-request");
    std::thread::sleep(Duration::from_millis(1500));

    let app_id = format!("d2b-clip-picker-focus-test-{}", std::process::id());
    let mut foot = Command::new("foot")
        .args(["--app-id", &app_id, "--title", &app_id, "sleep", "30"])
        .spawn()
        .expect("spawn focus-stealing foot window");
    let window_id = wait_for_niri_window(&app_id);
    niri_window_action("focus-window", window_id);
    wait_for_niri_window_focus(window_id);

    parent
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set cancel timeout");
    let mut reader = BufReader::new(parent.try_clone().expect("clone picker socket"));
    let mut cancel = String::new();
    assert_ne!(reader.read_line(&mut cancel).expect("read cancel"), 0);
    let response: serde_json::Value = serde_json::from_str(cancel.trim_end()).expect("cancel JSON");
    assert_eq!(response["type"], "cancel");
    assert_eq!(response["request_id"], "niri-focus-request");

    let mut duplicate = String::new();
    assert_eq!(
        reader
            .read_line(&mut duplicate)
            .expect("picker socket closes after cancel"),
        0,
        "picker must send exactly one terminal frame"
    );
    assert!(picker.wait().expect("wait for picker").success());

    niri_window_action("close-window", window_id);
    let deadline = Instant::now() + Duration::from_secs(2);
    while foot.try_wait().expect("poll foot").is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    if foot.try_wait().expect("poll closed foot").is_none() {
        foot.kill().expect("stop focus test foot");
        foot.wait().expect("reap focus test foot");
    }
}

#[test]
fn wayland_render_mode_writes_valid_bounded_png() {
    if std::env::var_os("D2B_RUN_WAYLAND_UI_TESTS").is_none() {
        return;
    }
    let output = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("render-contract-{}.png", std::process::id()));
    let status = Command::new(env!("CARGO_BIN_EXE_d2b-clip-picker"))
        .arg("--render-sample")
        .arg(&output)
        .status()
        .expect("run render mode");
    assert!(status.success(), "render mode must exit successfully");

    let bytes = std::fs::read(&output).expect("read rendered PNG");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 420);
    assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 520);
    assert!(bytes.len() < 5 * 1024 * 1024);
    std::fs::remove_file(output).expect("remove rendered PNG");
}
