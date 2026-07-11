use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PROTOCOL_MIN: u16 = 1;
pub const PROTOCOL_MAX: u16 = 2;
pub const MAX_PICKER_FRAME_BYTES: usize = 4 * 1024;
pub const MAX_OPEN_REQUEST_BYTES: usize = 23_553_408;
pub const MAX_CANDIDATES: usize = 64;
pub const MAX_METADATA_BYTES: usize = 1024;
pub const MAX_PREVIEW_BYTES: usize = 2048;
pub const MAX_THUMBNAIL_BASE64_BYTES: usize = 349_528;
pub const MAX_REALM_DISPLAY_ENTRIES: usize = 64;

const MAX_PROTOCOL_VERSION_TEXT_BYTES: usize = 128;
const MAX_REQUEST_ID_BYTES: usize = 256;
const MAX_ENTRY_ID_BYTES: usize = 256;
const MAX_MIME_TYPE_BYTES: usize = 256;
const MAX_COLOR_HINT_BYTES: usize = 128;
const MAX_PLACEMENT_COORDINATE: f64 = 1_000_000.0;
const MAX_PLACEMENT_DIMENSION: i32 = 1_000_000;
const REDACTED: &str = "<redacted>";
const IPC_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolVersionRange {
    pub min: u16,
    pub max: u16,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PickerFrame {
    ClientHello {
        protocol_version_range: ProtocolVersionRange,
        picker_version: String,
    },
    Select {
        selected_protocol_version: u16,
        request_id: String,
        entry_id: String,
    },
    Cancel {
        selected_protocol_version: u16,
        request_id: String,
    },
}

impl fmt::Debug for PickerFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClientHello {
                protocol_version_range,
                ..
            } => formatter
                .debug_struct("ClientHello")
                .field("protocol_version_range", protocol_version_range)
                .field("picker_version", &REDACTED)
                .finish(),
            Self::Select {
                selected_protocol_version,
                ..
            } => formatter
                .debug_struct("Select")
                .field("selected_protocol_version", selected_protocol_version)
                .field("request_id", &REDACTED)
                .field("entry_id", &REDACTED)
                .finish(),
            Self::Cancel {
                selected_protocol_version,
                ..
            } => formatter
                .debug_struct("Cancel")
                .field("selected_protocol_version", selected_protocol_version)
                .field("request_id", &REDACTED)
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClipdFrame {
    OpenRequest(Box<OpenRequest>),
    Error {
        selected_protocol_version: Option<u16>,
        request_id: Option<String>,
        code: String,
    },
    Close {
        selected_protocol_version: Option<u16>,
        request_id: Option<String>,
        code: String,
    },
}

impl fmt::Debug for ClipdFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenRequest(request) => {
                formatter.debug_tuple("OpenRequest").field(request).finish()
            }
            Self::Error {
                selected_protocol_version,
                request_id,
                ..
            } => formatter
                .debug_struct("Error")
                .field("selected_protocol_version", selected_protocol_version)
                .field("request_id", &request_id.as_ref().map(|_| REDACTED))
                .field("code", &REDACTED)
                .finish(),
            Self::Close {
                selected_protocol_version,
                request_id,
                ..
            } => formatter
                .debug_struct("Close")
                .field("selected_protocol_version", selected_protocol_version)
                .field("request_id", &request_id.as_ref().map(|_| REDACTED))
                .field("code", &REDACTED)
                .finish(),
        }
    }
}

impl ClipdFrame {
    pub fn validate(&self) -> Result<(), ProtocolViolation> {
        match self {
            Self::OpenRequest(request) => request.validate(),
            Self::Error {
                selected_protocol_version,
                request_id,
                code,
            }
            | Self::Close {
                selected_protocol_version,
                request_id,
                code,
            } => {
                if let Some(version) = selected_protocol_version {
                    validate_selected_protocol_version(*version)?;
                }
                validate_optional_text("request_id", request_id.as_deref(), MAX_REQUEST_ID_BYTES)?;
                validate_text("code", code, MAX_METADATA_BYTES, true)
            }
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRequest {
    pub selected_protocol_version: u16,
    pub clipd_version: String,
    pub picker_version: String,
    pub request_id: String,
    pub destination: DestinationMetadata,
    pub requested_mime_type: String,
    pub expires_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub placement_hints: Option<PlacementHints>,
    pub candidates: Vec<Candidate>,
    /// Presentation-only per-realm display metadata keyed by realm name.
    /// Absent realms fall back to the picker's theme palette defaults.
    /// This field is optional and defaults to an empty map when omitted by
    /// older `d2b-clipd` versions.
    #[serde(default)]
    pub realm_display: HashMap<String, RealmDisplayMetadata>,
}

impl fmt::Debug for OpenRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenRequest")
            .field("selected_protocol_version", &self.selected_protocol_version)
            .field("clipd_version", &REDACTED)
            .field("picker_version", &REDACTED)
            .field("request_id", &REDACTED)
            .field("destination", &self.destination)
            .field("requested_mime_type", &REDACTED)
            .field("expires_at_unix_ms", &self.expires_at_unix_ms)
            .field("placement_hints", &self.placement_hints)
            .field("candidate_count", &self.candidates.len())
            .field("realm_display_count", &self.realm_display.len())
            .finish()
    }
}

impl OpenRequest {
    pub fn validate(&self) -> Result<(), ProtocolViolation> {
        validate_selected_protocol_version(self.selected_protocol_version)?;
        validate_text(
            "clipd_version",
            &self.clipd_version,
            MAX_PROTOCOL_VERSION_TEXT_BYTES,
            true,
        )?;
        validate_text(
            "picker_version",
            &self.picker_version,
            MAX_PROTOCOL_VERSION_TEXT_BYTES,
            true,
        )?;
        validate_text("request_id", &self.request_id, MAX_REQUEST_ID_BYTES, true)?;
        validate_text(
            "requested_mime_type",
            &self.requested_mime_type,
            MAX_MIME_TYPE_BYTES,
            true,
        )?;
        self.destination.validate()?;
        if let Some(hints) = &self.placement_hints {
            hints.validate()?;
        }
        if self.candidates.len() > MAX_CANDIDATES {
            return Err(ProtocolViolation::new(format!(
                "candidate count exceeds {MAX_CANDIDATES}"
            )));
        }
        for candidate in &self.candidates {
            candidate.validate()?;
        }
        if self.realm_display.len() > MAX_REALM_DISPLAY_ENTRIES {
            return Err(ProtocolViolation::new(format!(
                "realm_display entry count exceeds {MAX_REALM_DISPLAY_ENTRIES}"
            )));
        }
        for (realm, metadata) in &self.realm_display {
            validate_text("realm_display realm", realm, MAX_METADATA_BYTES, true)?;
            validate_optional_text(
                "realm_display color",
                metadata.color.as_deref(),
                MAX_COLOR_HINT_BYTES,
            )?;
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct DestinationMetadata {
    pub realm: String,
    pub realm_kind: RealmKind,
    /// Presentation-only provider identity supplied by `d2b-clipd`.
    #[serde(default, skip_serializing_if = "PresentationProviderKind::is_unknown")]
    pub provider_kind: PresentationProviderKind,
    /// Presentation-only isolation identity supplied by `d2b-clipd`.
    #[serde(
        default,
        skip_serializing_if = "PresentationIsolationPosture::is_unknown"
    )]
    pub isolation_posture: PresentationIsolationPosture,
    #[serde(default)]
    pub application: Option<String>,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub attribution: Option<AttributionQuality>,
}

impl fmt::Debug for DestinationMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DestinationMetadata")
            .field("realm", &REDACTED)
            .field("realm_kind", &self.realm_kind)
            .field("provider_kind", &self.provider_kind)
            .field("isolation_posture", &self.isolation_posture)
            .field("application", &self.application.as_ref().map(|_| REDACTED))
            .field("app_id", &self.app_id.as_ref().map(|_| REDACTED))
            .field("title", &self.title.as_ref().map(|_| REDACTED))
            .field("workspace", &self.workspace.as_ref().map(|_| REDACTED))
            .field("output", &self.output.as_ref().map(|_| REDACTED))
            .field("attribution", &self.attribution)
            .finish()
    }
}

impl DestinationMetadata {
    fn validate(&self) -> Result<(), ProtocolViolation> {
        validate_text("destination realm", &self.realm, MAX_METADATA_BYTES, true)?;
        for (field, value) in [
            ("destination application", self.application.as_deref()),
            ("destination app_id", self.app_id.as_deref()),
            ("destination title", self.title.as_deref()),
            ("destination workspace", self.workspace.as_deref()),
            ("destination output", self.output.as_deref()),
        ] {
            validate_optional_text(field, value, MAX_METADATA_BYTES)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RealmKind {
    Host,
    Vm,
    UnsafeLocal,
    #[default]
    Unknown,
}

/// Independent, presentation-only copy of the provider labels used on the
/// clipd→picker boundary. These values never participate in authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PresentationProviderKind {
    LocalVm,
    QemuMedia,
    ProviderManaged,
    UnsafeLocal,
    #[default]
    Unknown,
}

impl PresentationProviderKind {
    pub fn is_unknown(&self) -> bool {
        *self == Self::Unknown
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::LocalVm => "local-vm",
            Self::QemuMedia => "qemu-media",
            Self::ProviderManaged => "provider-managed",
            Self::UnsafeLocal => "unsafe-local",
            Self::Unknown => "unknown",
        }
    }
}

/// Independent, presentation-only copy of the isolation labels used on the
/// clipd→picker boundary. These values never participate in authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PresentationIsolationPosture {
    VirtualMachine,
    ProviderManaged,
    UnsafeLocal,
    #[default]
    Unknown,
}

impl PresentationIsolationPosture {
    pub fn is_unknown(&self) -> bool {
        *self == Self::Unknown
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::VirtualMachine => "virtual-machine",
            Self::ProviderManaged => "provider-managed",
            Self::UnsafeLocal => "unsafe-local",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionQuality {
    ExactClient,
    FocusedWindowGuess,
    CacheStaleFocusedWindowGuess,
    BrokerInjectedDebug,
}

/// Presentation-only realm display metadata supplied by `d2b-clipd`.
///
/// This is used solely for grouping and coloring the realm headers in the
/// picker UI. It carries no authorization weight; trust decisions remain
/// inside `d2b-clipd`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RealmDisplayMetadata {
    /// Optional CSS-safe color hint (`#rrggbb` or `alpha(#rrggbb, opacity)`)
    /// for the realm group header. Unused if absent or if the value fails the
    /// picker's safe-color validation.
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementHints {
    #[serde(default)]
    pub pointer_x: Option<f64>,
    #[serde(default)]
    pub pointer_y: Option<f64>,
    #[serde(default)]
    pub output_width: Option<i32>,
    #[serde(default)]
    pub output_height: Option<i32>,
    #[serde(default)]
    pub overlay_width: Option<i32>,
    #[serde(default)]
    pub overlay_height: Option<i32>,
    #[serde(default)]
    pub output: Option<String>,
}

impl fmt::Debug for PlacementHints {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlacementHints")
            .field("pointer_x", &self.pointer_x)
            .field("pointer_y", &self.pointer_y)
            .field("output_width", &self.output_width)
            .field("output_height", &self.output_height)
            .field("overlay_width", &self.overlay_width)
            .field("overlay_height", &self.overlay_height)
            .field("output", &self.output.as_ref().map(|_| REDACTED))
            .finish()
    }
}

impl PlacementHints {
    fn validate(&self) -> Result<(), ProtocolViolation> {
        for (field, value) in [("pointer_x", self.pointer_x), ("pointer_y", self.pointer_y)] {
            if value.is_some_and(|coordinate| {
                !coordinate.is_finite() || coordinate.abs() > MAX_PLACEMENT_COORDINATE
            }) {
                return Err(ProtocolViolation::new(format!(
                    "{field} is outside the supported placement range"
                )));
            }
        }
        for (field, value) in [
            ("output_width", self.output_width),
            ("output_height", self.output_height),
            ("overlay_width", self.overlay_width),
            ("overlay_height", self.overlay_height),
        ] {
            if value.is_some_and(|dimension| dimension <= 0 || dimension > MAX_PLACEMENT_DIMENSION)
            {
                return Err(ProtocolViolation::new(format!(
                    "{field} is outside the supported placement range"
                )));
            }
        }
        validate_optional_text(
            "placement output",
            self.output.as_deref(),
            MAX_METADATA_BYTES,
        )
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub entry_id: String,
    pub source_realm: String,
    pub source_realm_kind: RealmKind,
    /// Presentation-only provider identity supplied by `d2b-clipd`.
    #[serde(default, skip_serializing_if = "PresentationProviderKind::is_unknown")]
    pub source_provider_kind: PresentationProviderKind,
    /// Presentation-only isolation identity supplied by `d2b-clipd`.
    #[serde(
        default,
        skip_serializing_if = "PresentationIsolationPosture::is_unknown"
    )]
    pub source_isolation_posture: PresentationIsolationPosture,
    #[serde(default)]
    pub source_app: Option<String>,
    #[serde(default)]
    pub source_app_id: Option<String>,
    pub source_attribution: AttributionQuality,
    #[serde(default)]
    pub preview_text: Option<String>,
    pub content_type: String,
    #[serde(default)]
    pub timestamp_unix_ms: Option<u64>,
    #[serde(default)]
    pub thumbnail_png_base64: Option<String>,
    #[serde(default)]
    pub byte_count: Option<u64>,
    #[serde(default)]
    pub confirmation_required: bool,
}

impl fmt::Debug for Candidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Candidate")
            .field("entry_id", &REDACTED)
            .field("source_realm", &REDACTED)
            .field("source_realm_kind", &self.source_realm_kind)
            .field("source_provider_kind", &self.source_provider_kind)
            .field("source_isolation_posture", &self.source_isolation_posture)
            .field("source_app", &self.source_app.as_ref().map(|_| REDACTED))
            .field(
                "source_app_id",
                &self.source_app_id.as_ref().map(|_| REDACTED),
            )
            .field("source_attribution", &self.source_attribution)
            .field(
                "preview_text",
                &self.preview_text.as_ref().map(|_| REDACTED),
            )
            .field("content_type", &REDACTED)
            .field("timestamp_unix_ms", &self.timestamp_unix_ms)
            .field(
                "thumbnail_png_base64",
                &self.thumbnail_png_base64.as_ref().map(|_| REDACTED),
            )
            .field("byte_count", &self.byte_count)
            .field("confirmation_required", &self.confirmation_required)
            .finish()
    }
}

impl Candidate {
    fn validate(&self) -> Result<(), ProtocolViolation> {
        validate_text(
            "candidate entry_id",
            &self.entry_id,
            MAX_ENTRY_ID_BYTES,
            true,
        )?;
        validate_text(
            "candidate source_realm",
            &self.source_realm,
            MAX_METADATA_BYTES,
            true,
        )?;
        for (field, value) in [
            ("candidate source_app", self.source_app.as_deref()),
            ("candidate source_app_id", self.source_app_id.as_deref()),
        ] {
            validate_optional_text(field, value, MAX_METADATA_BYTES)?;
        }
        validate_optional_text(
            "candidate preview_text",
            self.preview_text.as_deref(),
            MAX_PREVIEW_BYTES,
        )?;
        validate_text(
            "candidate content_type",
            &self.content_type,
            MAX_MIME_TYPE_BYTES,
            true,
        )?;
        validate_optional_text(
            "candidate thumbnail_png_base64",
            self.thumbnail_png_base64.as_deref(),
            MAX_THUMBNAIL_BASE64_BYTES,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolViolation {
    message: String,
}

impl ProtocolViolation {
    fn new(message: String) -> Self {
        Self { message }
    }
}

impl fmt::Display for ProtocolViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProtocolViolation {}

fn validate_selected_protocol_version(version: u16) -> Result<(), ProtocolViolation> {
    if (PROTOCOL_MIN..=PROTOCOL_MAX).contains(&version) {
        Ok(())
    } else {
        Err(ProtocolViolation::new(format!(
            "d2b-clipd selected incompatible picker protocol version {version}; supported range is {PROTOCOL_MIN}..={PROTOCOL_MAX}"
        )))
    }
}

fn validate_text(
    field: &str,
    value: &str,
    max_bytes: usize,
    require_nonempty: bool,
) -> Result<(), ProtocolViolation> {
    if require_nonempty && value.is_empty() {
        return Err(ProtocolViolation::new(format!("{field} must not be empty")));
    }
    if value.len() > max_bytes {
        return Err(ProtocolViolation::new(format!(
            "{field} exceeds {max_bytes} bytes"
        )));
    }
    if value.contains('\0') {
        return Err(ProtocolViolation::new(format!(
            "{field} must not contain NUL"
        )));
    }
    Ok(())
}

fn validate_optional_text(
    field: &str,
    value: Option<&str>,
    max_bytes: usize,
) -> Result<(), ProtocolViolation> {
    value.map_or(Ok(()), |value| {
        validate_text(field, value, max_bytes, false)
    })
}

#[derive(Clone)]
pub struct PickerTx {
    stream: Arc<Mutex<UnixStream>>,
    selected_protocol_version: u16,
    request_id: String,
}

impl PickerTx {
    pub fn select(&self, entry_id: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.send(PickerFrame::Select {
            selected_protocol_version: self.selected_protocol_version,
            request_id: self.request_id.clone(),
            entry_id: entry_id.to_owned(),
        })
    }

    pub fn cancel(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.send(PickerFrame::Cancel {
            selected_protocol_version: self.selected_protocol_version,
            request_id: self.request_id.clone(),
        })
    }

    fn send(&self, frame: PickerFrame) -> Result<(), Box<dyn std::error::Error>> {
        let encoded = serde_json::to_vec(&frame)?;
        if encoded.len() > MAX_PICKER_FRAME_BYTES {
            return Err("picker frame exceeded size cap".into());
        }
        let mut stream = self
            .stream
            .lock()
            .map_err(|_| "picker socket lock poisoned")?;
        stream.write_all(&encoded)?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        Ok(())
    }
}

pub struct IpcPeer {
    reader: BufReader<UnixStream>,
    writer: Arc<Mutex<UnixStream>>,
    handshake_deadline: Instant,
}

impl IpcPeer {
    pub fn new(stream: UnixStream) -> Result<Self, Box<dyn std::error::Error>> {
        Self::new_with_timeout(stream, IPC_HANDSHAKE_TIMEOUT)
    }

    fn new_with_timeout(
        stream: UnixStream,
        timeout: Duration,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let writer_stream = stream.try_clone()?;
        let handshake_deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "picker handshake timeout exceeds the supported range",
            )
        })?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer: Arc::new(Mutex::new(writer_stream)),
            handshake_deadline,
        })
    }

    pub fn send_client_hello(&self) -> Result<(), Box<dyn std::error::Error>> {
        PickerTx {
            stream: self.writer.clone(),
            selected_protocol_version: PROTOCOL_MAX,
            request_id: String::new(),
        }
        .send(PickerFrame::ClientHello {
            protocol_version_range: ProtocolVersionRange {
                min: PROTOCOL_MIN,
                max: PROTOCOL_MAX,
            },
            picker_version: crate::VERSION.to_owned(),
        })
    }

    pub fn read_clipd_frame(&mut self) -> Result<ClipdFrame, Box<dyn std::error::Error>> {
        let line = read_bounded_line_until(
            &mut self.reader,
            MAX_OPEN_REQUEST_BYTES,
            self.handshake_deadline,
        )
        .map_err(|error| {
            if error.downcast_ref::<io::Error>().is_some_and(|error| {
                matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                )
            }) {
                Box::new(ProtocolViolation::new(
                    "timed out waiting for d2b-clipd picker protocol frame".to_owned(),
                )) as Box<dyn std::error::Error>
            } else {
                error
            }
        })?;
        let frame: ClipdFrame = serde_json::from_str(line.trim_end()).map_err(|_| {
            ProtocolViolation::new(
                "d2b-clipd sent a malformed or unsupported picker protocol frame".to_owned(),
            )
        })?;
        frame.validate()?;
        Ok(frame)
    }

    pub fn tx_for_request(&self, request: &OpenRequest) -> PickerTx {
        PickerTx {
            stream: self.writer.clone(),
            selected_protocol_version: request.selected_protocol_version,
            request_id: request.request_id.clone(),
        }
    }

    pub fn into_reader(mut self) -> io::Result<BufReader<UnixStream>> {
        self.reader.get_mut().set_read_timeout(None)?;
        self.reader.get_mut().set_write_timeout(None)?;
        Ok(self.reader)
    }
}

pub fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut buf = Vec::new();
    let read = reader
        .take(max_bytes.saturating_add(1) as u64)
        .read_until(b'\n', &mut buf)?;
    if read == 0 {
        return Err("d2b-clipd closed picker socket".into());
    }

    if !buf.ends_with(b"\n") {
        return Err("clipd frame exceeded size cap".into());
    }
    buf.pop();
    Ok(String::from_utf8(buf)?)
}

fn read_bounded_line_until(
    reader: &mut BufReader<UnixStream>,
    max_bytes: usize,
    deadline: Instant,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut buf = Vec::with_capacity(max_bytes.min(8192));
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "picker protocol frame deadline elapsed",
            )
            .into());
        }
        reader.get_ref().set_read_timeout(Some(remaining))?;
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err("d2b-clipd closed picker socket".into());
        }
        let chunk_len = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if buf.len().saturating_add(chunk_len) > max_bytes {
            return Err("clipd frame exceeded size cap".into());
        }
        let complete = available[chunk_len - 1] == b'\n';
        buf.extend_from_slice(&available[..chunk_len]);
        reader.consume(chunk_len);
        if complete {
            return Ok(String::from_utf8(buf)?);
        }
    }
}

pub fn sanitize_preview(input: &str, max_chars: usize) -> String {
    let mut output = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if output.chars().count() >= max_chars {
            output.push('…');
            break;
        }
        if ch == '\u{1b}' {
            while let Some(next) = chars.peek().copied() {
                chars.next();
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        match ch {
            '\n' | '\r' | '\t' => output.push(' '),
            c if c.is_control() => output.push('�'),
            c => output.push(c),
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn display_content_kind(content_type: &str) -> (&'static str, &'static str) {
    match content_type.split(';').next().unwrap_or(content_type) {
        "text/html" => ("HTML text", "📝"),
        "text/plain" => ("Text", "📝"),
        "image/png" => ("Image", "🖼️"),
        _ => ("Item", "📄"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reader_rejects_overlong_line() {
        let input = vec![b'a'; MAX_OPEN_REQUEST_BYTES + 1];
        let mut cursor = std::io::Cursor::new(input);
        let err = read_bounded_line(&mut cursor, MAX_OPEN_REQUEST_BYTES).unwrap_err();
        assert!(err.to_string().contains("size cap"));
    }

    #[test]
    fn ipc_peer_times_out_on_a_partial_handshake_frame() {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.write_all(b"{").unwrap();
        let mut peer = IpcPeer::new_with_timeout(client, Duration::from_millis(20)).unwrap();
        let started = std::time::Instant::now();
        let error = peer.read_clipd_frame().unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn ipc_peer_uses_one_deadline_against_slow_trickle_frames() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let producer = std::thread::spawn(move || {
            for _ in 0..50 {
                if server.write_all(b"{").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let mut peer = IpcPeer::new_with_timeout(client, Duration::from_millis(30)).unwrap();
        let started = Instant::now();
        let error = peer.read_clipd_frame().unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_millis(200));
        drop(peer);
        producer.join().unwrap();
    }

    #[test]
    fn ipc_peer_clears_read_timeout_for_interactive_lifetime() {
        let (client, _server) = UnixStream::pair().unwrap();
        let peer = IpcPeer::new_with_timeout(client, Duration::from_millis(20)).unwrap();
        let reader = peer.into_reader().unwrap();
        assert_eq!(reader.get_ref().read_timeout().unwrap(), None);
        assert_eq!(reader.get_ref().write_timeout().unwrap(), None);
    }

    #[test]
    fn sanitize_preview_strips_ansi_and_controls() {
        assert_eq!(
            sanitize_preview("hello\u{1b}[31m red\nworld", 80),
            "hello red world"
        );
    }
}
