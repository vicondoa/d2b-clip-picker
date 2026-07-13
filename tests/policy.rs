use std::fs;
use std::path::{Path, PathBuf};

fn source_files() -> Vec<PathBuf> {
    let mut files = vec![
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("flake.nix"),
    ];
    collect_rs(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    files.into_iter().filter(|path| path.exists()).collect()
}

fn collect_rs(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_rs(&path, files);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

fn assert_source_excludes(boundary: &str, forbidden: &[&str]) {
    for path in source_files() {
        let content = fs::read_to_string(&path).expect("read file");
        for needle in forbidden {
            assert!(
                !content.contains(needle),
                "{} violates the {boundary} boundary with marker {needle}",
                path.display()
            );
        }
    }
}

#[test]
fn no_clipboard_or_input_authority() {
    assert_source_excludes(
        "clipboard authority",
        &[
            concat!("data", "_control"),
            concat!("ext_", "data", "_control"),
            concat!("zwlr_", "data", "_control"),
            concat!("ext-", "data", "-control"),
            concat!("zwlr-", "data", "-control"),
            "primary_selection",
            "primary-selection",
            concat!("virtual", "_keyboard"),
            "ydotool",
            concat!("wl", "-copy"),
            concat!("wl", "-paste"),
            "SetClipboard",
            "ClearClipboard",
        ],
    );
}

#[test]
fn no_clipboard_payload_fd_transport() {
    assert_source_excludes(
        "payload fd",
        &[
            "recvmsg",
            "sendmsg",
            "SCM_RIGHTS",
            "SocketAncillary",
            "ControlMessage",
            "AncillaryData",
        ],
    );
}

#[test]
fn no_d2b_daemon_or_toolkit_client_coupling() {
    assert_source_excludes(
        "independent protocol",
        &[
            "d2b-client",
            "d2b_client",
            "d2b-toolkit",
            "d2b_toolkit",
            "d2b-core",
            "d2b_core",
            "d2b-contracts",
            "d2b_contracts",
            "d2bd-client",
            "d2bd_client",
            "/run/d2b/public.sock",
            "d2bd",
        ],
    );
}

#[test]
fn no_command_execution_surface() {
    assert_source_excludes(
        "command execution",
        &[
            "std::process::Command",
            "tokio::process",
            "Command::new(",
            "execve(",
            "execvp(",
            "posix_spawn",
            "libc::fork",
            "libc::system",
        ],
    );
}

#[test]
fn no_compositor_specific_ipc() {
    assert_source_excludes(
        "compositor IPC",
        &[
            concat!("NIRI", "_SOCKET"),
            "niri-ipc",
            "niri_ipc",
            "swaymsg",
            "hyprctl",
        ],
    );
}

#[test]
fn foreign_toplevel_observer_is_presentation_lifecycle_only() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/ui.rs");
    let source = fs::read_to_string(path).expect("read UI source");
    let observer_and_rest = source
        .split_once("mod foreign_toplevel_focus {")
        .expect("embedded focus observer")
        .1;
    let content = observer_and_rest
        .split_once("#[derive(Debug, Default)]\npub struct FocusLifecycle")
        .expect("end of embedded focus observer")
        .0;

    assert!(content.contains("ZwlrForeignToplevelManagerV1"));
    assert!(content.contains("State::Activated"));
    for forbidden in [
        "Event::Title",
        "Event::AppId",
        "Event::OutputEnter",
        "Event::OutputLeave",
        ".activate(",
        ".close(",
        ".set_rectangle(",
        ".set_fullscreen(",
        ".unset_fullscreen(",
        ".set_maximized(",
        ".unset_maximized(",
        ".set_minimized(",
    ] {
        assert!(
            !content.contains(forbidden),
            "foreign-toplevel observer exceeds lifecycle-only boundary with {forbidden}"
        );
    }
}

#[test]
fn no_persistence_or_history_store() {
    assert_source_excludes(
        "persistence",
        &[
            "rusqlite",
            "libsqlite",
            "sqlite",
            "stoolap",
            "redb",
            "sled",
            "keyring",
            "aes-gcm",
            "std::fs::write",
            "fs::write(",
            "File::create(",
            "OpenOptions",
            "create_dir",
            "ClipboardHistory",
            "clipboard_history",
            "HistoryStore",
        ],
    );
}

#[test]
fn no_policy_engine_or_app_id_identity_inference() {
    assert_source_excludes(
        "policy and identity",
        &[
            "PolicyDecision",
            "evaluate_policy",
            "authorize_transfer",
            "is_transfer_allowed",
            "app_id.starts_with",
            "app_id.strip_prefix",
            "app_id.split",
            ".starts_with(\"d2b.",
            ".strip_prefix(\"d2b.",
        ],
    );
}

#[test]
fn outbound_protocol_has_no_transfer_fulfillment_action() {
    for forbidden_type in [
        "set_clipboard",
        "write_payload",
        "receive_payload_fd",
        "publish_selection",
        "execute",
    ] {
        let frame = format!(r#"{{"type":"{forbidden_type}"}}"#);
        assert!(
            serde_json::from_str::<d2b_clip_picker::protocol::PickerFrame>(&frame).is_err(),
            "forbidden outbound action {forbidden_type} must not decode"
        );
    }
}
