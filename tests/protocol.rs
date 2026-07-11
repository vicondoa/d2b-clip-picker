use d2b_clip_picker::protocol::{
    AttributionQuality, Candidate, ClipdFrame, DestinationMetadata, IpcPeer, MAX_CANDIDATES,
    OpenRequest, PROTOCOL_MAX, PROTOCOL_MIN, PickerFrame, PlacementHints,
    PresentationIsolationPosture, PresentationProviderKind, RealmDisplayMetadata, RealmKind,
    sanitize_preview,
};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

fn sample_request() -> OpenRequest {
    OpenRequest {
        selected_protocol_version: 2,
        clipd_version: "0.2.0".to_owned(),
        picker_version: "0.2.0".to_owned(),
        request_id: "req-1".to_owned(),
        destination: DestinationMetadata {
            realm: "personal".to_owned(),
            realm_kind: RealmKind::Vm,
            provider_kind: PresentationProviderKind::LocalVm,
            isolation_posture: PresentationIsolationPosture::VirtualMachine,
            application: Some("Firefox".to_owned()),
            app_id: Some("firefox".to_owned()),
            title: None,
            workspace: Some("1".to_owned()),
            output: Some("DP-1".to_owned()),
            attribution: Some(AttributionQuality::ExactClient),
        },
        requested_mime_type: "text/plain".to_owned(),
        expires_at_unix_ms: Some(1_800_000_000_000),
        placement_hints: Some(PlacementHints {
            pointer_x: Some(20.0),
            pointer_y: Some(30.0),
            output_width: Some(1920),
            output_height: Some(1080),
            overlay_width: Some(420),
            overlay_height: Some(520),
            output: Some("DP-1".to_owned()),
        }),
        candidates: vec![Candidate {
            entry_id: "entry-1".to_owned(),
            source_realm: "host".to_owned(),
            source_realm_kind: RealmKind::UnsafeLocal,
            source_provider_kind: PresentationProviderKind::UnsafeLocal,
            source_isolation_posture: PresentationIsolationPosture::UnsafeLocal,
            source_app: Some("Terminal".to_owned()),
            source_app_id: Some("org.example.Terminal".to_owned()),
            source_attribution: AttributionQuality::FocusedWindowGuess,
            preview_text: Some("hello".to_owned()),
            content_type: "text/plain".to_owned(),
            timestamp_unix_ms: Some(1_700_000_000_000),
            thumbnail_png_base64: None,
            byte_count: Some(5),
            confirmation_required: false,
        }],
        realm_display: Default::default(),
    }
}

#[test]
fn client_hello_contains_only_protocol_range_and_picker_version() {
    let (client, server) = UnixStream::pair().expect("socketpair");
    let peer = IpcPeer::new(client).expect("peer");
    peer.send_client_hello().expect("hello");

    let mut line = String::new();
    BufReader::new(server).read_line(&mut line).expect("line");
    let value: serde_json::Value = serde_json::from_str(line.trim_end()).expect("json");
    assert_eq!(value["type"], "client_hello");
    assert!(value.get("protocol_version_range").is_some());
    assert!(value.get("picker_version").is_some());
    assert!(value.get("request_id").is_none());
    assert_eq!(value["protocol_version_range"]["min"], PROTOCOL_MIN);
    assert_eq!(value["protocol_version_range"]["max"], PROTOCOL_MAX);
}

#[test]
fn fake_clipd_select_roundtrip() {
    let (client, mut server) = UnixStream::pair().expect("socketpair");
    let mut peer = IpcPeer::new(client).expect("peer");
    peer.send_client_hello().expect("hello");

    let mut hello = String::new();
    let mut server_reader = BufReader::new(server.try_clone().expect("clone"));
    server_reader.read_line(&mut hello).expect("hello line");

    let request = sample_request();
    let frame = ClipdFrame::OpenRequest(Box::new(request.clone()));
    writeln!(server, "{}", serde_json::to_string(&frame).expect("encode")).expect("write");
    let decoded = peer.read_clipd_frame().expect("open request");
    assert!(matches!(decoded, ClipdFrame::OpenRequest(_)));

    peer.tx_for_request(&request)
        .select("entry-1")
        .expect("select");
    let mut select = String::new();
    server_reader.read_line(&mut select).expect("select line");
    let value: serde_json::Value = serde_json::from_str(select.trim_end()).expect("json");
    assert_eq!(value["type"], "select");
    assert_eq!(value["request_id"], "req-1");
    assert_eq!(value["entry_id"], "entry-1");
    assert_eq!(value["selected_protocol_version"], 2);
}

#[test]
fn fake_clipd_cancel_roundtrip() {
    let (client, mut server) = UnixStream::pair().expect("socketpair");
    let mut peer = IpcPeer::new(client).expect("peer");
    peer.send_client_hello().expect("hello");

    let mut server_reader = BufReader::new(server.try_clone().expect("clone"));
    let mut hello = String::new();
    server_reader.read_line(&mut hello).expect("hello line");

    let request = sample_request();
    writeln!(
        server,
        "{}",
        serde_json::to_string(&ClipdFrame::OpenRequest(Box::new(request.clone()))).expect("encode")
    )
    .expect("write");
    peer.read_clipd_frame().expect("open request");
    peer.tx_for_request(&request).cancel().expect("cancel");

    let mut cancel = String::new();
    server_reader.read_line(&mut cancel).expect("cancel line");
    let value: serde_json::Value = serde_json::from_str(cancel.trim_end()).expect("json");
    assert_eq!(value["type"], "cancel");
    assert_eq!(value["request_id"], "req-1");
    assert!(value.get("entry_id").is_none());
}

#[test]
fn previews_are_inert_plain_text() {
    let sanitized = sanitize_preview("<b>x</b>\u{1b}[31m\nnext\u{0007}", 128);
    assert_eq!(sanitized, "<b>x</b> next�");
}

/// Older `d2b-clipd` versions do not emit `realm_display`; the field must
/// default to an empty map so the picker remains backward-compatible.
#[test]
fn open_request_without_realm_display_deserializes_to_empty_map() {
    let json = serde_json::json!({
        "type": "open_request",
        "selected_protocol_version": 1,
        "clipd_version": "0.1.0",
        "picker_version": "0.1.0",
        "request_id": "req-2",
        "destination": {
            "realm": "personal",
            "realm_kind": "vm"
        },
        "requested_mime_type": "text/plain",
        "candidates": []
    });
    let frame: ClipdFrame = serde_json::from_value(json).expect("parse without realm_display");
    let ClipdFrame::OpenRequest(req) = frame else {
        panic!("expected OpenRequest");
    };
    assert!(
        req.realm_display.is_empty(),
        "realm_display must default to empty map"
    );
    assert_eq!(
        req.destination.provider_kind,
        PresentationProviderKind::Unknown
    );
    assert_eq!(
        req.destination.isolation_posture,
        PresentationIsolationPosture::Unknown
    );
}

/// When `d2b-clipd` supplies `realm_display`, the picker parses the color
/// hint as presentation metadata only.
#[test]
fn open_request_with_realm_display_parses_color_hint() {
    let json = serde_json::json!({
        "type": "open_request",
        "selected_protocol_version": 1,
        "clipd_version": "0.1.0",
        "picker_version": "0.1.0",
        "request_id": "req-3",
        "destination": {
            "realm": "work",
            "realm_kind": "vm"
        },
        "requested_mime_type": "text/plain",
        "candidates": [],
        "realm_display": {
            "work": { "color": "#4a9eff" },
            "host": {}
        }
    });
    let frame: ClipdFrame = serde_json::from_value(json).expect("parse with realm_display");
    let ClipdFrame::OpenRequest(req) = frame else {
        panic!("expected OpenRequest");
    };
    assert_eq!(
        req.realm_display
            .get("work")
            .and_then(|m| m.color.as_deref()),
        Some("#4a9eff"),
        "work realm color must round-trip"
    );
    assert!(
        req.realm_display
            .get("host")
            .and_then(|m| m.color.as_deref())
            .is_none(),
        "host realm without color must produce None"
    );
}

/// `RealmDisplayMetadata` with no fields must accept an empty JSON object.
#[test]
fn realm_display_metadata_accepts_empty_object() {
    let meta: RealmDisplayMetadata = serde_json::from_str("{}").expect("empty object");
    assert!(meta.color.is_none());
}

/// `RealmDisplayMetadata` with `color: null` must produce `None`.
#[test]
fn realm_display_metadata_accepts_null_color() {
    let meta: RealmDisplayMetadata = serde_json::from_str(r#"{"color":null}"#).expect("null color");
    assert!(meta.color.is_none());
}

#[test]
fn v1_omission_defaults_endpoint_presentation_to_legacy_unknown() {
    let json = serde_json::json!({
        "type": "open_request",
        "selected_protocol_version": 1,
        "clipd_version": "0.1.0",
        "picker_version": "0.2.0",
        "request_id": "legacy",
        "destination": {
            "realm": "work",
            "realm_kind": "vm"
        },
        "requested_mime_type": "text/plain",
        "candidates": [{
            "entry_id": "entry",
            "source_realm": "personal",
            "source_realm_kind": "vm",
            "source_attribution": "exact_client",
            "content_type": "text/plain"
        }]
    });

    let frame: ClipdFrame = serde_json::from_value(json).expect("v1 frame");
    frame.validate().expect("valid v1 frame");
    let ClipdFrame::OpenRequest(request) = frame else {
        panic!("expected open request");
    };
    assert_eq!(
        request.destination.provider_kind,
        PresentationProviderKind::Unknown
    );
    assert_eq!(
        request.destination.isolation_posture,
        PresentationIsolationPosture::Unknown
    );
    assert_eq!(
        request.candidates[0].source_provider_kind,
        PresentationProviderKind::Unknown
    );
    assert_eq!(
        request.candidates[0].source_isolation_posture,
        PresentationIsolationPosture::Unknown
    );
}

#[test]
fn v2_roundtrip_preserves_bounded_provider_and_posture_values() {
    let request = sample_request();
    let encoded = serde_json::to_string(&ClipdFrame::OpenRequest(Box::new(request)))
        .expect("encode v2 request");
    let decoded: ClipdFrame = serde_json::from_str(&encoded).expect("decode v2 request");
    decoded.validate().expect("validate v2 request");
    let ClipdFrame::OpenRequest(request) = decoded else {
        panic!("expected open request");
    };

    assert_eq!(request.selected_protocol_version, 2);
    assert_eq!(
        request.destination.provider_kind,
        PresentationProviderKind::LocalVm
    );
    assert_eq!(
        request.destination.isolation_posture,
        PresentationIsolationPosture::VirtualMachine
    );
    assert_eq!(
        request.candidates[0].source_provider_kind,
        PresentationProviderKind::UnsafeLocal
    );
    assert_eq!(
        request.candidates[0].source_isolation_posture,
        PresentationIsolationPosture::UnsafeLocal
    );
}

#[test]
fn unknown_fields_and_future_closed_values_fail_visibly() {
    let unknown_field = r#"{
        "realm":"work",
        "realm_kind":"vm",
        "provider_kind":"local-vm",
        "isolation_posture":"virtual-machine",
        "future_authority":true
    }"#;
    let error = serde_json::from_str::<DestinationMetadata>(unknown_field)
        .expect_err("unknown fields must fail");
    assert!(error.to_string().contains("unknown field"));

    let unknown_provider = r#"{"realm":"work","realm_kind":"vm","provider_kind":"future-runtime"}"#;
    let error = serde_json::from_str::<DestinationMetadata>(unknown_provider)
        .expect_err("future closed value must fail");
    assert!(error.to_string().contains("unknown variant"));
}

#[test]
fn boundary_protocol_errors_do_not_echo_unknown_values() {
    let (client, mut server) = UnixStream::pair().expect("socketpair");
    let mut peer = IpcPeer::new(client).expect("peer");
    let frame = serde_json::json!({
        "type": "open_request",
        "selected_protocol_version": 2,
        "clipd_version": "0.2.0",
        "picker_version": "0.2.0",
        "request_id": "request",
        "destination": {
            "realm": "work",
            "realm_kind": "vm",
            "provider_kind": "future-provider-secret-sentinel"
        },
        "requested_mime_type": "text/plain",
        "candidates": []
    });
    writeln!(server, "{frame}").expect("write");

    let error = peer.read_clipd_frame().expect_err("unknown provider");
    assert_eq!(
        error.to_string(),
        "d2b-clipd sent a malformed or unsupported picker protocol frame"
    );
    assert!(
        !error
            .to_string()
            .contains("future-provider-secret-sentinel")
    );
}

#[test]
fn request_validation_enforces_candidate_and_metadata_bounds() {
    let mut too_many = sample_request();
    too_many.candidates = vec![too_many.candidates[0].clone(); MAX_CANDIDATES + 1];
    let error = too_many.validate().expect_err("candidate cap");
    assert!(error.to_string().contains("candidate count exceeds"));

    let mut oversized_realm = sample_request();
    oversized_realm.destination.realm = "r".repeat(1025);
    let error = oversized_realm.validate().expect_err("realm cap");
    assert!(error.to_string().contains("destination realm exceeds"));
    assert!(
        !error
            .to_string()
            .contains(&oversized_realm.destination.realm)
    );
}

#[test]
fn incompatible_selected_version_is_rejected_clearly() {
    let (client, mut server) = UnixStream::pair().expect("socketpair");
    let mut peer = IpcPeer::new(client).expect("peer");
    let mut request = sample_request();
    request.selected_protocol_version = PROTOCOL_MAX + 1;
    writeln!(
        server,
        "{}",
        serde_json::to_string(&ClipdFrame::OpenRequest(Box::new(request))).expect("encode")
    )
    .expect("write");

    let error = peer
        .read_clipd_frame()
        .expect_err("incompatible selected version");
    let message = error.to_string();
    assert!(message.contains("incompatible picker protocol version 3"));
    assert!(message.contains("1..=2"));
}

#[test]
fn protocol_debug_output_redacts_dynamic_metadata_and_payload_previews() {
    let mut request = sample_request();
    request.request_id = "request-secret-sentinel".to_owned();
    request.destination.realm = "realm-secret-sentinel".to_owned();
    request.destination.application = Some("application-secret-sentinel".to_owned());
    request.candidates[0].entry_id = "entry-secret-sentinel".to_owned();
    request.candidates[0].preview_text = Some("preview-secret-sentinel".to_owned());
    request.candidates[0].thumbnail_png_base64 = Some("thumbnail-secret-sentinel".to_owned());
    let debug = format!("{:?}", ClipdFrame::OpenRequest(Box::new(request)));

    for secret in [
        "request-secret-sentinel",
        "realm-secret-sentinel",
        "application-secret-sentinel",
        "entry-secret-sentinel",
        "preview-secret-sentinel",
        "thumbnail-secret-sentinel",
    ] {
        assert!(!debug.contains(secret), "Debug leaked {secret}");
    }
    assert!(debug.contains("<redacted>"));

    let outbound = PickerFrame::Select {
        selected_protocol_version: 2,
        request_id: "outbound-request-sentinel".to_owned(),
        entry_id: "outbound-entry-sentinel".to_owned(),
    };
    let debug = format!("{outbound:?}");
    assert!(!debug.contains("outbound-request-sentinel"));
    assert!(!debug.contains("outbound-entry-sentinel"));
}
