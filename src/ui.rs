use self::foreign_toplevel_focus::{FocusObserverEvent, spawn_foreign_toplevel_focus_observer};
use crate::placement::{MovablePlacement, PickerPlacement, UsableArea, WorkAreaProbe};
use crate::protocol::{
    AttributionQuality, Candidate, ClipdFrame, DestinationMetadata, IpcPeer,
    MAX_OPEN_REQUEST_BYTES, OpenRequest, PickerTx, PlacementHints, PresentationIsolationPosture,
    PresentationProviderKind, RealmDisplayMetadata, RealmKind, display_content_kind,
    read_bounded_line, sanitize_preview,
};
use base64::Engine;
use gtk4::gdk::prelude::MonitorExt;
use gtk4::prelude::*;
use gtk4::{Align, Orientation, gdk};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use libadwaita::{self as adw, prelude::*};
use log::{info, warn};
use serde::Deserialize;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const UNSAFE_LOCAL_WARNING: &str = "unsafe-local · no isolation";
pub const PIN_WIDGET_NAME: &str = "d2b-pin-toggle";
pub const PIN_ICON_WIDGET_NAME: &str = "d2b-pin-icon";
pub const PIN_ICON_NAME: &str = "view-pin-symbolic";
pub const PIN_ICON_THEME: &str = "Adwaita";
pub const DRAG_HANDLE_WIDGET_NAME: &str = "d2b-drag-handle";
pub const RENDER_WIDTH: i32 = 420;
pub const RENDER_HEIGHT: i32 = 520;
pub const PICKER_BOOTSTRAP_KEYBOARD_MODE: KeyboardMode = KeyboardMode::Exclusive;
pub const PICKER_STEADY_KEYBOARD_MODE: KeyboardMode = KeyboardMode::OnDemand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTransition {
    None,
    Cancel,
}

mod foreign_toplevel_focus {
    use std::collections::HashMap;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::thread;

    use log::{debug, info};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::wl_registry;
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols_wlr::foreign_toplevel::v1::client::{
        zwlr_foreign_toplevel_handle_v1::{self as toplevel_handle, ZwlrForeignToplevelHandleV1},
        zwlr_foreign_toplevel_manager_v1::{
            self as toplevel_manager, ZwlrForeignToplevelManagerV1,
        },
    };

    #[derive(Debug)]
    pub(super) enum FocusObserverEvent {
        NormalToplevelActivated,
        Unavailable(String),
    }

    #[derive(Debug, Default)]
    struct HandleState {
        activated: bool,
        pending_activated: Option<bool>,
    }

    struct ObserverState {
        handles: HashMap<u32, HandleState>,
        baseline_complete: bool,
        stopped: bool,
        tx: Sender<FocusObserverEvent>,
    }

    impl ObserverState {
        fn new(tx: Sender<FocusObserverEvent>) -> Self {
            Self {
                handles: HashMap::new(),
                baseline_complete: false,
                stopped: false,
                tx,
            }
        }

        fn finish_baseline(&mut self) {
            self.baseline_complete = true;
        }

        fn register(&mut self, handle: &ZwlrForeignToplevelHandleV1) {
            debug!("foreign-toplevel observer registered a normal toplevel");
            self.handles.entry(handle.id().protocol_id()).or_default();
        }

        fn set_pending_state(&mut self, handle: &ZwlrForeignToplevelHandleV1, state: &[u8]) {
            let activated = is_activated_state(state);
            self.handles
                .entry(handle.id().protocol_id())
                .or_default()
                .pending_activated = Some(activated);
        }

        fn commit(&mut self, handle: &ZwlrForeignToplevelHandleV1) {
            let state = self.handles.entry(handle.id().protocol_id()).or_default();
            let activated = state.pending_activated.take().unwrap_or(state.activated);
            let became_activated =
                should_notify_activation(self.baseline_complete, state.activated, activated);
            state.activated = activated;
            debug!(
                "foreign-toplevel observer state committed: activated={activated} baseline={}",
                self.baseline_complete
            );

            if became_activated
                && self
                    .tx
                    .send(FocusObserverEvent::NormalToplevelActivated)
                    .is_err()
            {
                self.stopped = true;
            }
        }

        fn remove(&mut self, handle: &ZwlrForeignToplevelHandleV1) {
            self.handles.remove(&handle.id().protocol_id());
        }
    }

    fn is_activated_state(state: &[u8]) -> bool {
        state.chunks_exact(4).any(|value| {
            let value = u32::from_ne_bytes(value.try_into().expect("four-byte state"));
            value == toplevel_handle::State::Activated as u32
        })
    }

    fn should_notify_activation(
        baseline_complete: bool,
        was_active: bool,
        is_active: bool,
    ) -> bool {
        baseline_complete && !was_active && is_active
    }

    pub(super) fn spawn_foreign_toplevel_focus_observer() -> Receiver<FocusObserverEvent> {
        let (tx, rx) = mpsc::channel();
        let thread_tx = tx.clone();
        if let Err(error) = thread::Builder::new()
            .name("d2b-focus-observer".to_owned())
            .spawn(move || {
                if let Err(error) = run_observer(thread_tx.clone()) {
                    let _ = thread_tx.send(FocusObserverEvent::Unavailable(error.to_string()));
                }
            })
        {
            let _ = tx.send(FocusObserverEvent::Unavailable(format!(
                "could not start focus observer: {error}"
            )));
        }
        rx
    }

    fn run_observer(
        tx: Sender<FocusObserverEvent>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let connection = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<ObserverState>(&connection)?;
        let _manager =
            globals.bind::<ZwlrForeignToplevelManagerV1, _, _>(&queue.handle(), 1..=3, ())?;
        let mut state = ObserverState::new(tx);

        queue.roundtrip(&mut state)?;
        state.finish_baseline();
        info!("foreign-toplevel focus observer baseline complete");

        while !state.stopped {
            queue.blocking_dispatch(&mut state)?;
        }
        Ok(())
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ObserverState {
        fn event(
            _state: &mut Self,
            _registry: &wl_registry::WlRegistry,
            _event: wl_registry::Event,
            _data: &GlobalListContents,
            _connection: &Connection,
            _queue: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for ObserverState {
        fn event(
            state: &mut Self,
            _manager: &ZwlrForeignToplevelManagerV1,
            event: toplevel_manager::Event,
            _data: &(),
            _connection: &Connection,
            _queue: &QueueHandle<Self>,
        ) {
            match event {
                toplevel_manager::Event::Toplevel { toplevel } => state.register(&toplevel),
                toplevel_manager::Event::Finished => {
                    let _ = state.tx.send(FocusObserverEvent::Unavailable(
                        "foreign-toplevel focus observer was stopped by the compositor".to_owned(),
                    ));
                    state.stopped = true;
                }
                _ => {}
            }
        }

        wayland_client::event_created_child!(ObserverState, ZwlrForeignToplevelManagerV1, [
            toplevel_manager::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ())
        ]);
    }

    impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for ObserverState {
        fn event(
            state: &mut Self,
            handle: &ZwlrForeignToplevelHandleV1,
            event: toplevel_handle::Event,
            _data: &(),
            _connection: &Connection,
            _queue: &QueueHandle<Self>,
        ) {
            match event {
                toplevel_handle::Event::State { state: next } => {
                    state.set_pending_state(handle, &next);
                }
                toplevel_handle::Event::Done => state.commit(handle),
                toplevel_handle::Event::Closed => state.remove(handle),
                _ => {}
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn state_bytes(states: &[toplevel_handle::State]) -> Vec<u8> {
            states
                .iter()
                .flat_map(|state| (*state as u32).to_ne_bytes())
                .collect()
        }

        #[test]
        fn activated_state_parser_ignores_other_presentation_states() {
            let inactive = state_bytes(&[
                toplevel_handle::State::Maximized,
                toplevel_handle::State::Fullscreen,
            ]);
            let active = state_bytes(&[
                toplevel_handle::State::Maximized,
                toplevel_handle::State::Activated,
            ]);

            assert!(!is_activated_state(&inactive));
            assert!(is_activated_state(&active));
        }

        #[test]
        fn baseline_and_repeated_active_states_do_not_notify() {
            assert!(!should_notify_activation(false, false, true));
            assert!(should_notify_activation(true, false, true));
            assert!(!should_notify_activation(true, true, true));
            assert!(!should_notify_activation(true, true, false));
        }
    }
}

#[derive(Debug, Default)]
pub struct FocusLifecycle {
    had_focus: bool,
    focused: bool,
    pinned: bool,
    terminal_sent: bool,
}

impl FocusLifecycle {
    pub fn focus_changed(&mut self, focused: bool) -> FocusTransition {
        self.focused = focused;
        if focused {
            self.had_focus = true;
            return FocusTransition::None;
        }
        self.cancel_if_transient()
    }

    pub fn set_pinned(&mut self, pinned: bool) -> FocusTransition {
        self.pinned = pinned;
        self.cancel_if_transient()
    }

    pub fn is_pinned(&self) -> bool {
        self.pinned
    }

    pub fn claim_terminal(&mut self) -> bool {
        if self.terminal_sent {
            false
        } else {
            self.terminal_sent = true;
            true
        }
    }

    fn cancel_if_transient(&mut self) -> FocusTransition {
        if self.had_focus && !self.focused && !self.pinned && self.claim_terminal() {
            FocusTransition::Cancel
        } else {
            FocusTransition::None
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct EndpointPresentation {
    pub realm: String,
    pub provider: Option<&'static str>,
    pub isolation: Option<&'static str>,
    pub warning: Option<&'static str>,
}

impl EndpointPresentation {
    pub fn has_identity_metadata(&self) -> bool {
        self.provider.is_some() || self.isolation.is_some()
    }

    pub fn provenance_label(&self) -> String {
        let mut parts = vec![format!("Realm {}", self.realm)];
        if let Some(provider) = self.provider {
            parts.push(format!("provider {provider}"));
        }
        if let Some(isolation) = self.isolation {
            parts.push(format!("isolation {isolation}"));
        }
        parts.join(" · ")
    }

    pub fn identity_label(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(provider) = self.provider {
            parts.push(format!("provider {provider}"));
        }
        if let Some(isolation) = self.isolation {
            parts.push(format!("isolation {isolation}"));
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

pub fn destination_presentation(
    destination: &crate::protocol::DestinationMetadata,
) -> EndpointPresentation {
    endpoint_presentation(
        &destination.realm,
        destination.provider_kind,
        destination.isolation_posture,
    )
}

pub fn candidate_presentation(candidate: &Candidate) -> EndpointPresentation {
    endpoint_presentation(
        &candidate.source_realm,
        candidate.source_provider_kind,
        candidate.source_isolation_posture,
    )
}

fn endpoint_presentation(
    realm: &str,
    provider: PresentationProviderKind,
    isolation: PresentationIsolationPosture,
) -> EndpointPresentation {
    let provider_label = (!provider.is_unknown()).then(|| provider.label());
    let isolation_label = (!isolation.is_unknown()).then(|| isolation.label());
    let unsafe_local = provider == PresentationProviderKind::UnsafeLocal
        || isolation == PresentationIsolationPosture::UnsafeLocal;
    EndpointPresentation {
        realm: sanitize_preview(realm, 80),
        provider: provider_label,
        isolation: isolation_label,
        warning: unsafe_local.then_some(UNSAFE_LOCAL_WARNING),
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemePalette {
    pub background: String,
    pub foreground: String,
    pub border: String,
    pub accent: String,
    pub selected_background: String,
    pub realm_background: String,
    /// Background for realm group-header rows. Applies when `d2b-clipd` does
    /// not supply a per-realm color in `realm_display`.
    pub realm_header_background: String,
    pub search_background: String,
    pub warning_background: String,
}

impl Default for ThemePalette {
    fn default() -> Self {
        Self {
            background: "#1e1e2e".to_owned(),
            foreground: "#f8f8f2".to_owned(),
            border: "#2a2d35".to_owned(),
            accent: "#3584e4".to_owned(),
            selected_background: "alpha(#3584e4, 0.14)".to_owned(),
            realm_background: "alpha(#3584e4, 0.14)".to_owned(),
            realm_header_background: "alpha(#89b4fa, 0.10)".to_owned(),
            search_background: "alpha(currentColor, 0.07)".to_owned(),
            warning_background: "alpha(#f5c211, 0.22)".to_owned(),
        }
    }
}

impl ThemePalette {
    pub fn from_json_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let text = std::fs::read_to_string(path)?;
        let palette: Self = serde_json::from_str(&text)?;
        palette.validate()?;
        Ok(palette)
    }

    fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        for (name, value) in [
            ("background", &self.background),
            ("foreground", &self.foreground),
            ("border", &self.border),
            ("accent", &self.accent),
            ("selected_background", &self.selected_background),
            ("realm_background", &self.realm_background),
            ("realm_header_background", &self.realm_header_background),
            ("search_background", &self.search_background),
            ("warning_background", &self.warning_background),
        ] {
            if !is_safe_css_color(value) {
                return Err(format!(
                    "theme field {name} must be a #rrggbb color or alpha(#rrggbb|currentColor, opacity)"
                )
                .into());
            }
        }
        Ok(())
    }

    fn css(&self) -> String {
        format!(
            "
        window.d2b-clip-picker {{
            background-color: {background};
            color: {foreground};
            border: 2px solid {border};
            border-radius: 12px;
        }}
        .d2b-clip-picker-root {{
            background-color: {background};
            color: {foreground};
            border: 2px solid {border};
            border-radius: 12px;
        }}
        headerbar {{ background: transparent; box-shadow: none; }}
        .drag-handle {{ padding: 8px 12px; }}
        .pin-toggle {{ min-width: 24px; padding: 4px 6px; }}
        .pin-toggle:checked {{
            background: {accent};
            color: {foreground};
        }}
        .clipboard-list {{ background: transparent; }}
        .clipboard-item {{
            border: 1px solid {border};
            border-left-width: 5px;
            border-radius: 10px;
            padding: 4px;
            margin: 6px 12px;
            transition: border-color 150ms ease, background 150ms ease;
        }}
        .clipboard-item:hover {{ background: {search_background}; }}
        .clipboard-item:selected {{ background: {selected_background}; }}
        .clipboard-preview {{ opacity: 0.94; }}
        .realm-pill, .search-pill, .warning-pill {{
            border-radius: 999px;
            padding: 4px 8px;
        }}
        .realm-pill {{ background: {realm_background}; }}
        .realm-label {{ color: {foreground}; }}
        .search-pill {{ background: {search_background}; }}
        .warning-pill {{ background: {warning_background}; }}
        ",
            background = self.background,
            foreground = self.foreground,
            border = self.border,
            accent = self.accent,
            selected_background = self.selected_background,
            realm_background = self.realm_background,
            search_background = self.search_background,
            warning_background = self.warning_background,
        )
    }
}

pub fn run_picker(
    request: OpenRequest,
    peer: IpcPeer,
    placement: PickerPlacement,
    test_select_first: bool,
    theme: ThemePalette,
) -> Result<(), Box<dyn std::error::Error>> {
    adw::init()?;
    let tx = peer.tx_for_request(&request);
    let (socket_closed_tx, socket_closed_rx) = mpsc::channel();
    let mut reader = peer.into_reader()?;
    std::thread::spawn(
        move || match read_bounded_line(&mut reader, MAX_OPEN_REQUEST_BYTES) {
            Err(_) => {
                let _ = socket_closed_tx.send(());
            }
            Ok(line) => {
                let _ = serde_json::from_str::<ClipdFrame>(line.trim_end());
                let _ = socket_closed_tx.send(());
            }
        },
    );

    let app: gtk4::Application = adw::Application::builder()
        .application_id("io.github.vicondoa.d2b_clip_picker")
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build()
        .upcast();
    let lifecycle = Rc::new(RefCell::new(FocusLifecycle::default()));

    let request_for_activate = request.clone();
    let placement_for_activate = placement.clone();
    let theme_for_activate = theme.clone();
    let lifecycle_for_activate = lifecycle.clone();
    app.connect_activate(move |app| {
        let runtime = PickerRuntime {
            tx: Some(tx.clone()),
            app: app.clone(),
            lifecycle: lifecycle_for_activate.clone(),
            dismiss_on_focus_loss: true,
        };
        let built = create_window(
            app,
            request_for_activate.clone(),
            runtime,
            placement_for_activate.clone(),
            test_select_first,
            theme_for_activate.clone(),
        );
        built.window.present();
    });

    let app_for_socket = app.clone();
    let lifecycle_for_socket = lifecycle.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        if socket_closed_rx.try_recv().is_ok() {
            lifecycle_for_socket.borrow_mut().claim_terminal();
            app_for_socket.quit();
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });

    app.run_with_args::<String>(&[]);
    Ok(())
}

pub fn render_sample(output: &Path, theme: ThemePalette) -> Result<(), Box<dyn std::error::Error>> {
    if output.extension().and_then(|extension| extension.to_str()) != Some("png") {
        return Err("--render-sample output must use the .png extension".into());
    }
    if !output.parent().is_none_or(Path::exists) {
        return Err("--render-sample output parent directory does not exist".into());
    }

    adw::init()?;
    let app: gtk4::Application = adw::Application::builder()
        .application_id("io.github.vicondoa.d2b_clip_picker.render")
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build()
        .upcast();
    let result = Rc::new(RefCell::new(None::<Result<(), String>>));
    let request = render_sample_request();
    let placement = PickerPlacement::from_hints(
        request
            .placement_hints
            .as_ref()
            .expect("render sample placement"),
    );
    let output = output.to_path_buf();

    let result_for_activate = result.clone();
    app.connect_activate(move |app| {
        let runtime = PickerRuntime {
            tx: None,
            app: app.clone(),
            lifecycle: Rc::new(RefCell::new(FocusLifecycle::default())),
            dismiss_on_focus_loss: false,
        };
        let built = create_window(
            app,
            request.clone(),
            runtime,
            placement.clone(),
            false,
            theme.clone(),
        );
        if let Err(error) = assert_render_structure(&built, &request) {
            *result_for_activate.borrow_mut() = Some(Err(error));
            app.quit();
            return;
        }

        let pin_button = built.pin_button.clone();
        let output = output.clone();
        let result_for_map = result_for_activate.clone();
        let app_for_map = app.clone();
        built.window.connect_map(move |window| {
            let pin_button = pin_button.clone();
            let output = output.clone();
            let result_for_map = result_for_map.clone();
            let app_for_map = app_for_map.clone();
            let window = window.clone();
            glib::timeout_add_local_once(Duration::from_millis(300), move || {
                let capture = if !pin_button.is_visible() {
                    Err("production pin control is not visible".to_owned())
                } else {
                    capture_widget_png(&window, &output).map_err(|error| error.to_string())
                };
                *result_for_map.borrow_mut() = Some(capture);
                app_for_map.quit();
            });
        });
        built.window.present();
    });

    let result_for_timeout = result.clone();
    let app_for_timeout = app.clone();
    glib::timeout_add_local_once(Duration::from_secs(10), move || {
        if result_for_timeout.borrow().is_none() {
            *result_for_timeout.borrow_mut() =
                Some(Err("timed out rendering picker sample".to_owned()));
            app_for_timeout.quit();
        }
    });

    app.run_with_args::<String>(&[]);
    result
        .borrow_mut()
        .take()
        .unwrap_or_else(|| Err("picker render exited without a result".to_owned()))
        .map_err(Into::into)
}

pub fn render_sample_request() -> OpenRequest {
    let candidate = |entry_id: &str,
                     realm: &str,
                     realm_kind: RealmKind,
                     provider: PresentationProviderKind,
                     isolation: PresentationIsolationPosture,
                     app: &str,
                     preview: Option<&str>,
                     content_type: &str,
                     byte_count: Option<u64>,
                     confirmation_required: bool| {
        Candidate {
            entry_id: entry_id.to_owned(),
            source_realm: realm.to_owned(),
            source_realm_kind: realm_kind,
            source_canonical_target: None,
            source_provider_kind: provider,
            source_isolation_posture: isolation,
            source_app: Some(app.to_owned()),
            source_app_id: None,
            source_attribution: AttributionQuality::ExactClient,
            preview_text: preview.map(str::to_owned),
            content_type: content_type.to_owned(),
            timestamp_unix_ms: None,
            thumbnail_png_base64: None,
            byte_count,
            confirmation_required,
            capability_preflight: None,
        }
    };
    OpenRequest {
        selected_protocol_version: 2,
        clipd_version: "render-sample".to_owned(),
        picker_version: crate::VERSION.to_owned(),
        request_id: "synthetic-render-request".to_owned(),
        destination: DestinationMetadata {
            realm: "personal-dev".to_owned(),
            realm_kind: RealmKind::Vm,
            canonical_target: None,
            provider_kind: PresentationProviderKind::LocalVm,
            isolation_posture: PresentationIsolationPosture::VirtualMachine,
            application: Some("Browser".to_owned()),
            app_id: None,
            title: None,
            workspace: Some("development".to_owned()),
            output: None,
            attribution: Some(AttributionQuality::ExactClient),
            capability_preflight: None,
        },
        requested_mime_type: "text/plain".to_owned(),
        expires_at_unix_ms: None,
        placement_hints: Some(PlacementHints {
            pointer_x: Some(760.0),
            pointer_y: Some(96.0),
            output_width: Some(1280),
            output_height: Some(720),
            overlay_width: Some(RENDER_WIDTH),
            overlay_height: Some(RENDER_HEIGHT),
            output: None,
        }),
        candidates: vec![
            candidate(
                "sample-work",
                "work",
                RealmKind::Vm,
                PresentationProviderKind::LocalVm,
                PresentationIsolationPosture::VirtualMachine,
                "Editor",
                Some("Release checklist with deterministic sample text"),
                "text/plain",
                Some(49),
                false,
            ),
            candidate(
                "sample-research",
                "research",
                RealmKind::Vm,
                PresentationProviderKind::QemuMedia,
                PresentationIsolationPosture::VirtualMachine,
                "Reader",
                Some("Architecture notes and test evidence"),
                "text/plain",
                Some(36),
                false,
            ),
            candidate(
                "sample-provider",
                "managed",
                RealmKind::Vm,
                PresentationProviderKind::ProviderManaged,
                PresentationIsolationPosture::ProviderManaged,
                "Console",
                Some("Provider-managed clipboard sample"),
                "text/plain",
                Some(33),
                true,
            ),
            candidate(
                "sample-image",
                "host",
                RealmKind::Host,
                PresentationProviderKind::UnsafeLocal,
                PresentationIsolationPosture::UnsafeLocal,
                "Image viewer",
                None,
                "image/png",
                Some(18_432),
                false,
            ),
            candidate(
                "sample-host-text",
                "host",
                RealmKind::UnsafeLocal,
                PresentationProviderKind::UnsafeLocal,
                PresentationIsolationPosture::UnsafeLocal,
                "Terminal",
                Some("Host-side text placeholder"),
                "text/plain",
                Some(26),
                false,
            ),
        ],
        realm_display: HashMap::from([
            (
                "work".to_owned(),
                RealmDisplayMetadata {
                    color: Some("#7fc8ff".to_owned()),
                },
            ),
            (
                "research".to_owned(),
                RealmDisplayMetadata {
                    color: Some("#90d090".to_owned()),
                },
            ),
            (
                "managed".to_owned(),
                RealmDisplayMetadata {
                    color: Some("#c8a0e0".to_owned()),
                },
            ),
            (
                "host".to_owned(),
                RealmDisplayMetadata {
                    color: Some("#ffb347".to_owned()),
                },
            ),
        ]),
    }
}

fn assert_render_structure(built: &BuiltPickerWindow, request: &OpenRequest) -> Result<(), String> {
    if !configured_icon_theme().has_icon(PIN_ICON_NAME) {
        return Err(format!(
            "render mode requires packaged GTK icon {PIN_ICON_NAME}"
        ));
    }
    let distinct_realms = request
        .candidates
        .iter()
        .map(|candidate| candidate.source_realm.as_str())
        .collect::<std::collections::HashSet<_>>();
    if request.candidates.len() < 4 || distinct_realms.len() < 3 {
        return Err("render sample must contain multiple rows and realms".to_owned());
    }
    if !request.candidates.iter().any(|candidate| {
        candidate.source_isolation_posture == PresentationIsolationPosture::UnsafeLocal
    }) || !request.candidates.iter().any(|candidate| {
        candidate.source_provider_kind == PresentationProviderKind::ProviderManaged
    }) {
        return Err("render sample must exercise multiple provider postures".to_owned());
    }
    if built.pin_button.widget_name() != PIN_WIDGET_NAME
        || built.pin_button.label().is_some()
        || built.pin_button.child().is_none()
        || built.pin_button.is_active()
    {
        return Err(
            "production pin control must use icon content and be unpinned by default".to_owned(),
        );
    }
    if built.pin_icon.widget_name() != PIN_ICON_WIDGET_NAME {
        return Err("production pin control is missing its monochrome icon".to_owned());
    }
    match built.pin_icon_source {
        PinIconSource::ThemeSymbolic if built.pin_icon.paintable().is_none() => {
            return Err("theme symbolic pin icon has no image content".to_owned());
        }
        PinIconSource::GeneratedMonochrome => {
            return Err("render mode must exercise the packaged symbolic pin icon".to_owned());
        }
        _ => {}
    }
    if !widget_tree_contains(&built.root, PIN_WIDGET_NAME)
        || !widget_tree_contains(&built.root, PIN_ICON_WIDGET_NAME)
        || !widget_tree_contains(&built.root, DRAG_HANDLE_WIDGET_NAME)
    {
        return Err("production picker structure is missing pin or drag chrome".to_owned());
    }
    Ok(())
}

fn widget_tree_contains(root: &impl IsA<gtk4::Widget>, widget_name: &str) -> bool {
    let root = root.as_ref();
    if root.widget_name() == widget_name {
        return true;
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if widget_tree_contains(&widget, widget_name) {
            return true;
        }
        child = widget.next_sibling();
    }
    false
}

fn capture_widget_png(
    window: &adw::ApplicationWindow,
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let width = window.allocated_width();
    let height = window.allocated_height();
    if (width, height) != (RENDER_WIDTH, RENDER_HEIGHT) {
        return Err(format!(
            "production picker allocated {width}x{height}; expected {RENDER_WIDTH}x{RENDER_HEIGHT}"
        )
        .into());
    }

    let paintable = gtk4::WidgetPaintable::new(Some(window));
    let snapshot = gtk4::Snapshot::new();
    paintable.snapshot(&snapshot, width as f64, height as f64);
    let node = snapshot
        .to_node()
        .ok_or("production widget snapshot was empty")?;
    let surface =
        gtk4::prelude::NativeExt::surface(window).ok_or("picker surface is unavailable")?;
    let renderer =
        gtk4::gsk::Renderer::for_surface(&surface).ok_or("picker renderer is unavailable")?;
    let viewport = gtk4::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let texture = renderer.render_texture(&node, Some(&viewport));
    renderer.unrealize();
    if (texture.width(), texture.height()) != (RENDER_WIDTH, RENDER_HEIGHT) {
        return Err(format!(
            "rendered texture is {}x{}; expected {RENDER_WIDTH}x{RENDER_HEIGHT}",
            texture.width(),
            texture.height()
        )
        .into());
    }

    let stride = texture.width() as usize * 4;
    let mut pixels = vec![0_u8; stride * texture.height() as usize];
    texture.download(&mut pixels, stride);
    let first = pixels
        .first_chunk::<4>()
        .ok_or("rendered texture is empty")?;
    if pixels.chunks_exact(4).all(|pixel| pixel == first) {
        return Err("rendered texture is uniform".into());
    }

    texture.save_to_png(output)?;
    let bytes = std::fs::read(output)?;
    if bytes.len() >= 5 * 1024 * 1024 {
        return Err("rendered PNG must remain below 5 MB".into());
    }
    if png_dimensions(&bytes) != Some((RENDER_WIDTH as u32, RENDER_HEIGHT as u32)) {
        return Err("rendered PNG signature or logical dimensions are invalid".into());
    }
    Ok(())
}

#[derive(Clone)]
struct PickerRuntime {
    tx: Option<PickerTx>,
    app: gtk4::Application,
    lifecycle: Rc<RefCell<FocusLifecycle>>,
    dismiss_on_focus_loss: bool,
}

struct BuiltPickerWindow {
    window: adw::ApplicationWindow,
    root: gtk4::Box,
    pin_button: gtk4::ToggleButton,
    pin_icon: gtk4::Image,
    pin_icon_source: PinIconSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinIconSource {
    ThemeSymbolic,
    GeneratedMonochrome,
}

fn create_window(
    app: &gtk4::Application,
    request: OpenRequest,
    runtime: PickerRuntime,
    mut placement: PickerPlacement,
    test_select_first: bool,
    theme: ThemePalette,
) -> BuiltPickerWindow {
    configure_color_scheme();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("d2b clipboard picker")
        .decorated(false)
        .default_width(placement.geometry.overlay_width)
        .default_height(placement.geometry.overlay_height)
        .resizable(false)
        .build();
    window.add_css_class("d2b-clip-picker");
    window.init_layer_shell();
    window.set_layer(Layer::Top);
    window.set_namespace(Some("d2b-clip-picker"));
    let monitor = placement
        .output
        .as_deref()
        .and_then(find_monitor)
        .or_else(default_monitor);
    if let Some(monitor) = monitor.as_ref() {
        if placement.geometry.output_width <= 0 || placement.geometry.output_height <= 0 {
            let geometry = monitor.geometry();
            placement.geometry.output_width = geometry.width();
            placement.geometry.output_height = geometry.height();
            placement.geometry.x =
                ((geometry.width() - placement.geometry.overlay_width).max(16) / 2) as f64;
            placement.geometry.y =
                ((geometry.height() - placement.geometry.overlay_height).max(16) / 2) as f64;
        }
        window.set_monitor(Some(monitor));
    }
    info!(
        "picker window placement x={} y={} overlay_width={} overlay_height={} output={:?}",
        placement.geometry.x,
        placement.geometry.y,
        placement.geometry.overlay_width,
        placement.geometry.overlay_height,
        placement.output
    );
    window.set_exclusive_zone(0);
    window.set_keyboard_mode(PICKER_BOOTSTRAP_KEYBOARD_MODE);
    let output_for_refresh = placement.output.clone();
    let placement = placement.geometry;
    let usable_area = UsableArea::new(placement.output_width, placement.output_height);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Left, true);
    let movable = Rc::new(RefCell::new(MovablePlacement::new(placement)));
    apply_panel_position(&window, movable.borrow().position());
    let movable_for_map = movable.clone();
    window.connect_map(move |mapped| {
        let mapped = mapped.clone();
        let movable_for_map = movable_for_map.clone();
        glib::idle_add_local_once(move || {
            let mut placement = movable_for_map.borrow_mut();
            if let Some(area) = usable_area {
                placement.update_bounds(
                    area,
                    mapped.allocated_width().max(1),
                    mapped.allocated_height().max(1),
                );
            }
            apply_panel_position(&mapped, placement.position());
        });
    });
    if let Some(monitor) = monitor {
        let output =
            output_for_refresh.or_else(|| monitor.connector().map(|name| name.to_string()));
        let window_for_geometry = window.clone();
        let movable_for_geometry = movable.clone();
        let output_for_geometry = output.clone();
        monitor.connect_geometry_notify(move |_| {
            refresh_usable_area(
                &window_for_geometry,
                &movable_for_geometry,
                output_for_geometry.as_deref(),
            );
        });
        let window_for_scale = window.clone();
        let movable_for_scale = movable.clone();
        monitor.connect_scale_factor_notify(move |_| {
            refresh_usable_area(&window_for_scale, &movable_for_scale, output.as_deref());
        });
    }

    apply_css(&window, &theme);

    // Build the stable realm→CSS-class mapping for this request's candidates.
    // The CSS-class names are presentation-only and carry no authz meaning.
    let realm_css_classes = Rc::new(build_realm_css_classes(&request.candidates));
    apply_realm_colors_css(
        &window,
        &request.destination.realm,
        &request.realm_display,
        &realm_css_classes,
    );

    let confirm_entry = Rc::new(RefCell::new(None::<String>));
    let search = Rc::new(RefCell::new(String::new()));
    let displayed = Rc::new(RefCell::new(Vec::<Candidate>::new()));

    let main_box = gtk4::Box::new(Orientation::Vertical, 0);
    main_box.add_css_class("d2b-clip-picker-root");
    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(false);
    header.set_show_start_title_buttons(false);

    let drag_handle = gtk4::Box::new(Orientation::Horizontal, 0);
    drag_handle.set_widget_name(DRAG_HANDLE_WIDGET_NAME);
    drag_handle.add_css_class("drag-handle");
    drag_handle.set_hexpand(true);
    drag_handle.set_cursor_from_name(Some("move"));
    let header_title = gtk4::Label::new(Some("d2b clipboard picker"));
    header_title.set_hexpand(true);
    drag_handle.append(&header_title);
    header.set_title_widget(Some(&drag_handle));

    let drag_origin = Rc::new(Cell::new((0, 0)));
    let drag = gtk4::GestureDrag::new();
    drag.set_button(gdk::BUTTON_PRIMARY);
    let movable_for_drag_begin = movable.clone();
    let drag_origin_for_begin = drag_origin.clone();
    drag.connect_drag_begin(move |_, _, _| {
        drag_origin_for_begin.set(movable_for_drag_begin.borrow().position());
    });
    let movable_for_drag = movable.clone();
    let drag_origin_for_update = drag_origin.clone();
    let window_for_drag = window.clone();
    drag.connect_drag_update(move |_, offset_x, offset_y| {
        let mut placement = movable_for_drag.borrow_mut();
        placement.drag_from(drag_origin_for_update.get(), offset_x, offset_y);
        apply_panel_position(&window_for_drag, placement.position());
    });
    drag_handle.add_controller(drag);

    let close_button = gtk4::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Cancel")
        .build();
    close_button.add_css_class("flat");

    let (pin_icon, pin_icon_source) = build_pin_icon(&theme.foreground);
    let pin_button = gtk4::ToggleButton::builder()
        .child(&pin_icon)
        .tooltip_text("Pin picker so focus loss does not close it")
        .build();
    pin_button.set_widget_name(PIN_WIDGET_NAME);
    pin_button.add_css_class("flat");
    pin_button.add_css_class("pin-toggle");
    pin_button.update_property(&[
        gtk4::accessible::Property::Label("Pin picker"),
        gtk4::accessible::Property::Description(
            "Keep this picker open when another window receives focus",
        ),
    ]);
    header.pack_end(&close_button);
    header.pack_end(&pin_button);
    main_box.append(&header);

    let destination = gtk4::Label::new(Some(&destination_label(&request)));
    destination.add_css_class("title-4");
    destination.set_halign(Align::Start);
    destination.set_margin_start(16);
    destination.set_margin_end(16);
    destination.set_margin_top(8);
    destination.set_wrap(true);
    main_box.append(&destination);

    let destination_presentation = destination_presentation(&request.destination);
    if destination_presentation.has_identity_metadata() {
        let provenance = gtk4::Label::new(Some(&destination_presentation.provenance_label()));
        provenance.add_css_class("caption");
        provenance.add_css_class("endpoint-identity");
        provenance.set_halign(Align::Start);
        provenance.set_margin_start(16);
        provenance.set_margin_end(16);
        provenance.set_wrap(true);
        main_box.append(&provenance);
    }
    if let Some(warning) = destination_presentation.warning {
        let posture_warning = gtk4::Label::new(Some(warning));
        posture_warning.add_css_class("warning-pill");
        posture_warning.set_halign(Align::Start);
        posture_warning.set_margin_start(16);
        posture_warning.set_margin_end(16);
        posture_warning.set_margin_top(6);
        main_box.append(&posture_warning);
    }

    let requested = gtk4::Label::new(Some(&format!(
        "Requested {} · type to filter",
        request.requested_mime_type
    )));
    requested.add_css_class("caption");
    requested.add_css_class("dim-label");
    requested.set_halign(Align::Start);
    requested.set_margin_start(16);
    requested.set_margin_end(16);
    main_box.append(&requested);

    let search_label = gtk4::Label::new(Some("Search…"));
    search_label.add_css_class("search-pill");
    search_label.set_halign(Align::Fill);
    search_label.set_margin_top(10);
    search_label.set_margin_bottom(6);
    search_label.set_margin_start(16);
    search_label.set_margin_end(16);
    main_box.append(&search_label);

    let banner = gtk4::Label::new(None);
    banner.add_css_class("warning-pill");
    banner.set_visible(false);
    banner.set_margin_start(16);
    banner.set_margin_end(16);
    main_box.append(&banner);

    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scrolled.set_vexpand(true);
    scrolled.set_propagate_natural_width(false);
    scrolled.set_propagate_natural_height(false);
    scrolled.set_min_content_width(1);
    scrolled.set_min_content_height(1);

    let list_box = gtk4::ListBox::new();
    list_box.add_css_class("clipboard-list");
    list_box.set_selection_mode(gtk4::SelectionMode::Single);
    scrolled.set_child(Some(&list_box));
    main_box.append(&scrolled);
    window.set_content(Some(&main_box));

    rebuild_grouped_rows(
        &list_box,
        &request.candidates,
        &search.borrow(),
        &displayed,
        &realm_css_classes,
    );

    let runtime_for_pin = runtime.clone();
    pin_button.connect_toggled(move |button| {
        let pinned = button.is_active();
        if pinned {
            button.set_tooltip_text(Some("Unpin picker"));
            button.update_property(&[
                gtk4::accessible::Property::Label("Unpin picker"),
                gtk4::accessible::Property::Description(
                    "Allow this picker to close when another window receives focus",
                ),
            ]);
        } else {
            button.set_tooltip_text(Some("Pin picker so focus loss does not close it"));
            button.update_property(&[
                gtk4::accessible::Property::Label("Pin picker"),
                gtk4::accessible::Property::Description(
                    "Keep this picker open when another window receives focus",
                ),
            ]);
        }
        let transition = runtime_for_pin.lifecycle.borrow_mut().set_pinned(pinned);
        if transition == FocusTransition::Cancel {
            send_claimed_cancel_and_quit(&runtime_for_pin);
        }
    });

    let keyboard_release_scheduled = Rc::new(Cell::new(false));
    let focus_observer_started = Rc::new(Cell::new(false));
    let focus_controller = gtk4::EventControllerFocus::new();
    let window_for_focus_enter = window.clone();
    let runtime_for_focus_enter = runtime.clone();
    let keyboard_release_for_focus_enter = keyboard_release_scheduled.clone();
    let focus_observer_for_enter = focus_observer_started.clone();
    focus_controller.connect_enter(move |controller| {
        if controller.contains_focus() {
            info!("picker widget focus entered");
            observe_picker_focus(
                &window_for_focus_enter,
                &runtime_for_focus_enter,
                &keyboard_release_for_focus_enter,
                true,
            );
            if runtime_for_focus_enter.dismiss_on_focus_loss
                && !focus_observer_for_enter.replace(true)
            {
                monitor_normal_toplevel_activation(
                    spawn_foreign_toplevel_focus_observer(),
                    &window_for_focus_enter,
                    &runtime_for_focus_enter,
                    keyboard_release_for_focus_enter.clone(),
                );
            }
        }
    });
    window.add_controller(focus_controller);

    let runtime_for_close = runtime.clone();
    close_button.connect_clicked(move |_| {
        send_cancel_once_and_quit(&runtime_for_close);
    });

    let runtime_for_close_request = runtime.clone();
    window.connect_close_request(move |_| {
        send_cancel_once_and_quit(&runtime_for_close_request);
        glib::Propagation::Stop
    });

    let activation = ActivationContext {
        runtime: runtime.clone(),
        confirm_entry: confirm_entry.clone(),
        banner: banner.clone(),
    };
    if test_select_first {
        let displayed_for_test = displayed.clone();
        let activation_for_test = activation.clone();
        window.connect_map(move |_| {
            let displayed_for_test = displayed_for_test.clone();
            let activation_for_test = activation_for_test.clone();
            glib::idle_add_local_once(move || {
                if let Some(candidate) = displayed_for_test.borrow().first() {
                    info!(
                        "test-select-first mapped; selecting entry {}",
                        candidate.entry_id
                    );
                    activate_candidate(candidate, &activation_for_test);
                }
            });
        });
    }
    let displayed_for_activation = displayed.clone();
    let activation_for_row_activated = activation.clone();
    list_box.connect_row_activated(move |_, row| {
        let entry_id = row.widget_name();
        if let Some(candidate) = displayed_for_activation
            .borrow()
            .iter()
            .find(|c| c.entry_id.as_str() == entry_id.as_str())
        {
            activate_candidate(candidate, &activation_for_row_activated);
        }
    });
    let click_controller = gtk4::GestureClick::new();
    let list_for_click = list_box.clone();
    let displayed_for_click_select = displayed.clone();
    let activation_for_single_click = activation.clone();
    click_controller.connect_released(move |_, _, _, _| {
        if let Some(row) = list_for_click.selected_row() {
            let entry_id = row.widget_name();
            if let Some(candidate) = displayed_for_click_select
                .borrow()
                .iter()
                .find(|c| c.entry_id.as_str() == entry_id.as_str())
            {
                activate_candidate(candidate, &activation_for_single_click);
            }
        }
    });
    list_box.add_controller(click_controller);

    let key_controller = gtk4::EventControllerKey::new();
    let list_for_keys = list_box.clone();
    let request_for_keys = request.clone();
    let displayed_for_keys = displayed.clone();
    let search_for_keys = search.clone();
    let search_label_for_keys = search_label.clone();
    let activation_for_keys = activation.clone();
    let realm_css_classes_for_keys = realm_css_classes.clone();
    key_controller.connect_key_pressed(move |_, key, _keycode, modifiers| {
        use gdk::Key;
        if modifiers.contains(gdk::ModifierType::CONTROL_MASK) && key == Key::v {
            return glib::Propagation::Stop;
        }
        match key {
            Key::Escape => {
                send_cancel_once_and_quit(&activation_for_keys.runtime);
                glib::Propagation::Stop
            }
            Key::Down | Key::j | Key::J => {
                select_relative(&list_for_keys, 1);
                glib::Propagation::Stop
            }
            Key::Up | Key::k | Key::K => {
                select_relative(&list_for_keys, -1);
                glib::Propagation::Stop
            }
            Key::Return | Key::KP_Enter => {
                if let Some(row) = list_for_keys.selected_row() {
                    let entry_id = row.widget_name();
                    if let Some(candidate) = displayed_for_keys
                        .borrow()
                        .iter()
                        .find(|c| c.entry_id.as_str() == entry_id.as_str())
                    {
                        activate_candidate(candidate, &activation_for_keys);
                        return glib::Propagation::Stop;
                    }
                }
                glib::Propagation::Proceed
            }
            Key::BackSpace => {
                search_for_keys.borrow_mut().pop();
                update_search(
                    &search_for_keys,
                    &search_label_for_keys,
                    &list_for_keys,
                    &request_for_keys.candidates,
                    &displayed_for_keys,
                    &realm_css_classes_for_keys,
                );
                glib::Propagation::Stop
            }
            Key::Delete => glib::Propagation::Stop,
            _ => {
                if let Some(ch) = key.to_unicode()
                    && !ch.is_control()
                {
                    search_for_keys.borrow_mut().push(ch);
                    update_search(
                        &search_for_keys,
                        &search_label_for_keys,
                        &list_for_keys,
                        &request_for_keys.candidates,
                        &displayed_for_keys,
                        &realm_css_classes_for_keys,
                    );
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        }
    });
    window.add_controller(key_controller);

    BuiltPickerWindow {
        window,
        root: main_box,
        pin_button,
        pin_icon,
        pin_icon_source,
    }
}

fn build_pin_icon(foreground: &str) -> (gtk4::Image, PinIconSource) {
    let icon_theme = configured_icon_theme();
    let (image, source) = if icon_theme.has_icon(PIN_ICON_NAME) {
        info!("using GTK symbolic pin icon {PIN_ICON_NAME}");
        let icon = icon_theme.lookup_icon(
            PIN_ICON_NAME,
            &[],
            16,
            1,
            gtk4::TextDirection::None,
            gtk4::IconLookupFlags::FORCE_SYMBOLIC,
        );
        (
            gtk4::Image::from_paintable(Some(&icon)),
            PinIconSource::ThemeSymbolic,
        )
    } else {
        warn!(
            "GTK icon theme does not provide {PIN_ICON_NAME}; using generated monochrome pin icon"
        );
        (
            gtk4::Image::from_paintable(Some(&generated_pin_texture(foreground))),
            PinIconSource::GeneratedMonochrome,
        )
    };
    image.set_widget_name(PIN_ICON_WIDGET_NAME);
    image.set_pixel_size(16);
    image.set_can_target(false);
    (image, source)
}

fn configured_icon_theme() -> gtk4::IconTheme {
    let icon_theme = gtk4::IconTheme::new();
    icon_theme.set_theme_name(Some(PIN_ICON_THEME));
    icon_theme
}

fn generated_pin_texture(foreground: &str) -> gdk::Texture {
    const WIDTH: usize = 16;
    const MASK: [&str; 16] = [
        "................",
        "....########....",
        ".....######.....",
        ".....######.....",
        ".....######.....",
        "....########....",
        "...##########...",
        ".......##.......",
        ".......##.......",
        ".......##.......",
        ".......##.......",
        ".......##.......",
        ".......##.......",
        ".......##.......",
        "................",
        "................",
    ];
    let color = parse_pin_color(foreground);
    let pixels = generated_pin_pixels(&MASK, color);
    let bytes = glib::Bytes::from_owned(pixels);
    gdk::MemoryTexture::new(
        WIDTH as i32,
        MASK.len() as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        WIDTH * 4,
    )
    .upcast()
}

fn generated_pin_pixels(mask: &[&str], color: [u8; 3]) -> Vec<u8> {
    let width = mask.first().map_or(0, |row| row.len());
    let mut pixels = Vec::with_capacity(width * mask.len() * 4);
    for row in mask {
        for pixel in row.bytes() {
            if pixel == b'#' {
                pixels.extend_from_slice(&[color[0], color[1], color[2], 0xff]);
            } else {
                pixels.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    pixels
}

fn parse_pin_color(foreground: &str) -> [u8; 3] {
    if foreground.len() == 7 && foreground.starts_with('#') {
        let red = u8::from_str_radix(&foreground[1..3], 16);
        let green = u8::from_str_radix(&foreground[3..5], 16);
        let blue = u8::from_str_radix(&foreground[5..7], 16);
        if let (Ok(red), Ok(green), Ok(blue)) = (red, green, blue) {
            return [red, green, blue];
        }
    }
    [0xf8, 0xf8, 0xf2]
}

#[derive(Clone)]
struct ActivationContext {
    runtime: PickerRuntime,
    confirm_entry: Rc<RefCell<Option<String>>>,
    banner: gtk4::Label,
}

fn activate_candidate(candidate: &Candidate, ctx: &ActivationContext) {
    if ctx.runtime.lifecycle.borrow().terminal_sent {
        return;
    }
    if candidate.confirmation_required {
        let mut pending = ctx.confirm_entry.borrow_mut();
        if pending.as_deref() != Some(candidate.entry_id.as_str()) {
            *pending = Some(candidate.entry_id.clone());
            ctx.banner.set_text("Select this item again to confirm.");
            ctx.banner.set_visible(true);
            return;
        }
    }
    if ctx.runtime.lifecycle.borrow_mut().claim_terminal()
        && let Some(tx) = &ctx.runtime.tx
        && let Err(_err) = tx.select(&candidate.entry_id)
    {
        warn!("failed to send selection to d2b-clipd");
    }
    ctx.runtime.app.quit();
}

fn send_cancel_once_and_quit(runtime: &PickerRuntime) {
    if runtime.lifecycle.borrow_mut().claim_terminal()
        && let Some(tx) = &runtime.tx
        && let Err(_err) = tx.cancel()
    {
        warn!("failed to send cancellation to d2b-clipd");
    }
    runtime.app.quit();
}

fn observe_picker_focus(
    window: &adw::ApplicationWindow,
    runtime: &PickerRuntime,
    keyboard_release_scheduled: &Cell<bool>,
    focused: bool,
) {
    let transition = if runtime.dismiss_on_focus_loss {
        runtime.lifecycle.borrow_mut().focus_changed(focused)
    } else {
        FocusTransition::None
    };
    if focused && !keyboard_release_scheduled.replace(true) {
        let window = window.clone();
        glib::idle_add_local_once(move || {
            window.set_keyboard_mode(PICKER_STEADY_KEYBOARD_MODE);
        });
    }
    if transition == FocusTransition::Cancel {
        send_claimed_cancel_and_quit(runtime);
    }
}

fn monitor_normal_toplevel_activation(
    receiver: mpsc::Receiver<FocusObserverEvent>,
    window: &adw::ApplicationWindow,
    runtime: &PickerRuntime,
    keyboard_release_scheduled: Rc<Cell<bool>>,
) {
    let window = window.clone();
    let runtime = runtime.clone();
    glib::timeout_add_local(Duration::from_millis(25), move || {
        match receiver.try_recv() {
            Ok(FocusObserverEvent::NormalToplevelActivated) => {
                info!("normal Wayland toplevel activated after picker bootstrap");
                observe_picker_focus(&window, &runtime, &keyboard_release_scheduled, false);
                if runtime.lifecycle.borrow().terminal_sent {
                    return glib::ControlFlow::Break;
                }
            }
            Ok(FocusObserverEvent::Unavailable(error)) => {
                warn!("normal-toplevel focus observer unavailable: {error}");
                return glib::ControlFlow::Break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                warn!("normal-toplevel focus observer stopped unexpectedly");
                return glib::ControlFlow::Break;
            }
        }
        glib::ControlFlow::Continue
    });
}

fn send_claimed_cancel_and_quit(runtime: &PickerRuntime) {
    if let Some(tx) = &runtime.tx
        && let Err(_err) = tx.cancel()
    {
        warn!("failed to send cancellation to d2b-clipd");
    }
    runtime.app.quit();
}

fn update_search(
    search: &Rc<RefCell<String>>,
    label: &gtk4::Label,
    list_box: &gtk4::ListBox,
    candidates: &[Candidate],
    displayed: &Rc<RefCell<Vec<Candidate>>>,
    realm_css_classes: &HashMap<String, String>,
) {
    let query = search.borrow();
    if query.is_empty() {
        label.set_text("Search…");
    } else {
        label.set_text(&format!("Search: {query}"));
    }
    rebuild_grouped_rows(list_box, candidates, &query, displayed, realm_css_classes);
}

fn apply_panel_position(window: &impl IsA<gtk4::Window>, position: (i32, i32)) {
    window.set_margin(Edge::Left, position.0);
    window.set_margin(Edge::Top, position.1);
}

fn refresh_usable_area(
    window: &adw::ApplicationWindow,
    movable: &Rc<RefCell<MovablePlacement>>,
    output: Option<&str>,
) {
    match WorkAreaProbe::capture_timeout(output, Duration::from_millis(500)) {
        Ok(area) => {
            let mut placement = movable.borrow_mut();
            placement.update_bounds(
                area,
                window.allocated_width().max(1),
                window.allocated_height().max(1),
            );
            apply_panel_position(window, placement.position());
        }
        Err(error) => warn!("failed to refresh compositor usable area: {error}"),
    }
}

fn find_monitor(output: &str) -> Option<gdk::Monitor> {
    let display = gdk::Display::default()?;
    let monitors = display.monitors();
    for index in 0..monitors.n_items() {
        let Some(item) = monitors.item(index) else {
            continue;
        };
        let Ok(monitor) = item.downcast::<gdk::Monitor>() else {
            continue;
        };
        let connector = monitor.connector();
        let model = monitor.model();
        let matches = connector
            .as_ref()
            .is_some_and(|connector| connector == output)
            || model.as_ref().is_some_and(|model| model.contains(output));
        if matches {
            return Some(monitor);
        }
    }
    None
}

fn default_monitor() -> Option<gdk::Monitor> {
    let display = gdk::Display::default()?;
    let monitors = display.monitors();
    monitors.item(0)?.downcast::<gdk::Monitor>().ok()
}

/// Rebuild the list box rows grouped by realm, with a non-selectable realm
/// header row before each group. Selects the first selectable row after
/// rebuilding. `displayed` is updated to the filtered selectable candidates.
fn rebuild_grouped_rows(
    list_box: &gtk4::ListBox,
    candidates: &[Candidate],
    query: &str,
    displayed: &Rc<RefCell<Vec<Candidate>>>,
    realm_css_classes: &HashMap<String, String>,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    let query_lower = query.to_lowercase();
    let visible: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| candidate_matches(c, &query_lower))
        .collect();

    if visible.is_empty() {
        let row = gtk4::ListBoxRow::new();
        row.set_selectable(false);
        row.set_activatable(false);
        let label = gtk4::Label::new(Some("No matching clipboard entries"));
        label.add_css_class("dim-label");
        label.set_margin_top(24);
        label.set_margin_bottom(24);
        row.set_child(Some(&label));
        list_box.append(&row);
        *displayed.borrow_mut() = Vec::new();
        return;
    }

    // Group candidates by realm, preserving the order of first appearance.
    let mut realm_order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<&Candidate>> = HashMap::new();
    for candidate in &visible {
        let realm = candidate.source_realm.clone();
        if !groups.contains_key(&realm) {
            realm_order.push(realm.clone());
            groups.insert(realm.clone(), Vec::new());
        }
        groups.get_mut(&realm).unwrap().push(candidate);
    }

    let mut all_visible: Vec<Candidate> = Vec::new();

    for realm in &realm_order {
        let css_class = realm_css_classes.get(realm).map(String::as_str);
        for candidate in &groups[realm] {
            let row = candidate_row(candidate, css_class);
            row.set_widget_name(&candidate.entry_id);
            list_box.append(&row);
            all_visible.push((*candidate).clone());
        }
    }

    *displayed.borrow_mut() = all_visible;

    // Select the first selectable row (skips any leading realm header rows).
    let mut idx = 0i32;
    while let Some(row) = list_box.row_at_index(idx) {
        if row.is_selectable() {
            list_box.select_row(Some(&row));
            break;
        }
        idx += 1;
    }
}

fn candidate_matches(candidate: &Candidate, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {} {} {}",
        candidate.source_realm,
        candidate.source_app.as_deref().unwrap_or_default(),
        candidate.source_app_id.as_deref().unwrap_or_default(),
        candidate.content_type,
        candidate.preview_text.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    haystack.contains(query)
}

fn candidate_row(candidate: &Candidate, css_class: Option<&str>) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.add_css_class("clipboard-item");
    if let Some(class) = css_class {
        row.add_css_class(class);
    }
    let main = gtk4::Box::new(Orientation::Vertical, 6);
    main.set_margin_top(10);
    main.set_margin_bottom(10);
    main.set_margin_start(12);
    main.set_margin_end(12);
    main.set_hexpand(true);

    let header = gtk4::Box::new(Orientation::Horizontal, 8);
    let (kind, icon) = display_content_kind(&candidate.content_type);
    let icon_label = gtk4::Label::new(Some(icon));
    let realm = gtk4::Label::new(Some(&source_label(candidate)));
    realm.add_css_class("caption");
    realm.add_css_class("realm-label");
    let kind_label = gtk4::Label::new(Some(kind));
    kind_label.add_css_class("caption");
    kind_label.set_halign(Align::Start);
    kind_label.set_hexpand(true);
    let time = gtk4::Label::new(Some(&format_timestamp(candidate.timestamp_unix_ms)));
    time.add_css_class("caption");
    time.add_css_class("dim-label");
    header.append(&icon_label);
    header.append(&realm);
    header.append(&kind_label);
    header.append(&time);
    main.append(&header);

    let presentation = candidate_presentation(candidate);
    if let Some(identity) = presentation.identity_label() {
        let identity_label = gtk4::Label::new(Some(&identity));
        identity_label.add_css_class("caption");
        identity_label.add_css_class("endpoint-identity");
        identity_label.set_halign(Align::Start);
        identity_label.set_wrap(true);
        main.append(&identity_label);
    }
    if let Some(warning) = presentation.warning {
        let warning_label = gtk4::Label::new(Some(warning));
        warning_label.add_css_class("warning-pill");
        warning_label.set_halign(Align::Start);
        main.append(&warning_label);
    }

    let app_label = gtk4::Label::new(Some(&app_source_label(candidate)));
    app_label.add_css_class("caption");
    app_label.add_css_class("dim-label");
    app_label.set_halign(Align::Start);
    app_label.set_wrap(true);
    main.append(&app_label);

    if let Some(texture) = safe_host_thumbnail(candidate) {
        let picture = gtk4::Picture::for_paintable(&texture);
        picture.set_can_shrink(true);
        picture.set_hexpand(true);
        picture.set_height_request(160);
        picture.set_halign(Align::Center);
        picture.add_css_class("clipboard-preview");
        main.append(&picture);
    } else {
        let preview = preview_text(candidate);
        let preview_label = gtk4::Label::new(Some(&preview));
        preview_label.add_css_class("clipboard-preview");
        preview_label.set_halign(Align::Start);
        preview_label.set_wrap(true);
        preview_label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        preview_label.set_max_width_chars(58);
        preview_label.set_lines(4);
        preview_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        main.append(&preview_label);
    }

    if candidate.confirmation_required {
        let confirm = gtk4::Label::new(Some("Confirmation required"));
        confirm.add_css_class("warning-pill");
        confirm.set_halign(Align::Start);
        main.append(&confirm);
    }

    row.set_child(Some(&main));
    row
}

fn destination_label(request: &OpenRequest) -> String {
    let realm = sanitize_preview(&request.destination.realm, 80);
    let app = request
        .destination
        .application
        .as_deref()
        .or(request.destination.app_id.as_deref())
        .map(|value| sanitize_preview(value, 80));
    match app {
        Some(app) if !app.is_empty() => format!("Paste into {realm} · {app}"),
        _ => format!("Paste into {realm}"),
    }
}

fn source_label(candidate: &Candidate) -> String {
    sanitize_preview(&candidate.source_realm, 48)
}

fn app_source_label(candidate: &Candidate) -> String {
    let app = candidate
        .source_app
        .as_deref()
        .or(candidate.source_app_id.as_deref())
        .unwrap_or("Unknown app");
    let app = sanitize_preview(app, 80);
    match candidate.source_attribution {
        AttributionQuality::ExactClient => format!("Exact source · {app}"),
        AttributionQuality::FocusedWindowGuess => format!("Focused-window guess · {app}"),
        AttributionQuality::CacheStaleFocusedWindowGuess => {
            format!("Focused-window guess (stale) · {app}")
        }
        AttributionQuality::BrokerInjectedDebug => "Debug-injected source".to_owned(),
    }
}

fn preview_text(candidate: &Candidate) -> String {
    if candidate.content_type.split(';').next() == Some("image/png") {
        let bytes = candidate
            .byte_count
            .map(|count| format!(" · {count} bytes"))
            .unwrap_or_default();
        return format!("Image entry{bytes}");
    }
    sanitize_preview(
        candidate.preview_text.as_deref().unwrap_or("No preview"),
        300,
    )
}

fn safe_host_thumbnail(candidate: &Candidate) -> Option<gdk::Texture> {
    if candidate.source_realm_kind != RealmKind::Host {
        return None;
    }
    if candidate.content_type.split(';').next()? != "image/png" {
        return None;
    }
    let encoded = candidate.thumbnail_png_base64.as_ref()?;
    if encoded.len() > 256 * 1024 {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    if decoded.len() > 192 * 1024 {
        return None;
    }
    let (width, height) = png_dimensions(&decoded)?;
    if width > 4096 || height > 4096 || width.saturating_mul(height) > 8_000_000 {
        return None;
    }
    let bytes = glib::Bytes::from_owned(decoded);
    gdk::Texture::from_bytes(&bytes).ok()
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != PNG_SIG {
        return None;
    }
    if &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

fn select_relative(list_box: &gtk4::ListBox, delta: i32) {
    let current = list_box.selected_row().map(|row| row.index()).unwrap_or(0);
    let mut next = current + delta;
    loop {
        match list_box.row_at_index(next) {
            None => break,
            Some(row) if row.is_selectable() => {
                list_box.select_row(Some(&row));
                row.grab_focus();
                break;
            }
            Some(_) => next += delta,
        }
    }
}

fn format_timestamp(timestamp_ms: Option<u64>) -> String {
    let Some(timestamp_ms) = timestamp_ms else {
        return "Unknown time".to_owned();
    };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(timestamp_ms);
    let diff = now_ms.saturating_sub(timestamp_ms) / 1000;
    if diff < 30 {
        "Just now".to_owned()
    } else if diff < 3600 {
        let minutes = diff / 60;
        format!("{minutes}m ago")
    } else if diff < 86_400 {
        let hours = diff / 3600;
        format!("{hours}h ago")
    } else {
        let days = diff / 86_400;
        format!("{days}d ago")
    }
}

fn configure_color_scheme() {
    let style_manager = adw::StyleManager::default();
    if let Some(settings) = gtk4::Settings::default() {
        if settings.is_gtk_application_prefer_dark_theme() {
            style_manager.set_color_scheme(adw::ColorScheme::PreferDark);
        } else {
            style_manager.set_color_scheme(adw::ColorScheme::Default);
        }
    }
}

fn apply_css(window: &adw::ApplicationWindow, theme: &ThemePalette) {
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(&theme.css());
    gtk4::style_context_add_provider_for_display(
        &gtk4::prelude::WidgetExt::display(window),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Build a stable mapping from realm name to CSS class name for this session.
/// The CSS class names are `d2b-realm-header-<idx>` and are used solely for
/// per-realm color overrides on group header rows.
fn build_realm_css_classes(candidates: &[Candidate]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut idx = 0usize;
    for candidate in candidates {
        let realm = &candidate.source_realm;
        if !map.contains_key(realm) {
            map.insert(realm.clone(), format!("d2b-realm-header-{idx}"));
            idx += 1;
        }
    }
    map
}

/// Inject per-realm color CSS for group headers, using colors from
/// `realm_display` supplied by `d2b-clipd`. Colors are presentation-only and
/// carry no authorization weight. Colors that fail safe-CSS validation are
/// silently ignored, falling back to the palette's `realm_header_background`.
fn apply_realm_colors_css(
    window: &adw::ApplicationWindow,
    destination_realm: &str,
    realm_display: &HashMap<String, RealmDisplayMetadata>,
    realm_css_classes: &HashMap<String, String>,
) {
    let mut css = String::new();
    let _ = destination_realm;
    for (realm, class) in realm_css_classes {
        let configured = realm_display
            .get(realm)
            .and_then(|meta| meta.color.as_deref())
            .filter(|color| is_safe_css_color(color));
        let fallback;
        let color = match configured {
            Some(color) => color,
            None => {
                fallback = fallback_realm_color(realm);
                fallback.as_str()
            }
        };
        if is_safe_css_color(color) {
            css += &format!(".{class} {{ border-left-color: {color}; }}\n");
        }
    }
    if !css.is_empty() {
        let provider = gtk4::CssProvider::new();
        provider.load_from_data(&css);
        gtk4::style_context_add_provider_for_display(
            &gtk4::prelude::WidgetExt::display(window),
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn fallback_realm_color(realm: &str) -> String {
    const PALETTE: [&str; 12] = [
        "#7fc8ff", "#90d090", "#ffb347", "#c8a0e0", "#ff8080", "#40e0d0", "#ffd700", "#ff69b4",
        "#a0c8a0", "#d4a0ff", "#ffa07a", "#87ceeb",
    ];
    let mut hash = 0_u64;
    for byte in format!("d2b-env-accent-{realm}").bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(byte as u64);
    }
    PALETTE[(hash as usize) % PALETTE.len()].to_owned()
}

fn is_safe_css_color(value: &str) -> bool {
    is_hex_color(value) || is_alpha_color(value)
}

fn is_hex_color(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 7
        && bytes[0] == b'#'
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn is_alpha_color(value: &str) -> bool {
    let Some(inner) = value
        .strip_prefix("alpha(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };
    let Some((color, opacity)) = inner.split_once(',') else {
        return false;
    };
    let color = color.trim();
    let opacity = opacity.trim();
    (color == "currentColor" || is_hex_color(color))
        && opacity
            .parse::<f32>()
            .is_ok_and(|parsed| (0.0..=1.0).contains(&parsed))
}

#[cfg(test)]
mod theme_tests {
    use super::*;

    #[test]
    fn default_palette_uses_neutral_border_and_realm_rail() {
        let css = ThemePalette::default().css();
        assert!(css.contains("background-color: #1e1e2e;"));
        assert!(css.contains("border: 2px solid #2a2d35;"));
        assert!(css.contains("background: alpha(#3584e4, 0.14);"));
        assert!(css.contains("border-left-width: 5px;"));
        assert!(css.contains(".realm-label"));
        assert!(css.contains(".pin-toggle:checked"));
    }

    #[test]
    fn fallback_realm_color_is_deterministic_and_safe() {
        assert_eq!(fallback_realm_color("work"), fallback_realm_color("work"));
        assert!(is_safe_css_color(&fallback_realm_color("work")));
        assert_ne!(fallback_realm_color("work"), fallback_realm_color("dev"));
    }

    #[test]
    fn source_label_keeps_realm_identity_pure() {
        let candidate = Candidate {
            entry_id: "entry".to_owned(),
            source_realm: "dev".to_owned(),
            source_realm_kind: RealmKind::Vm,
            source_canonical_target: None,
            source_provider_kind: PresentationProviderKind::LocalVm,
            source_isolation_posture: PresentationIsolationPosture::VirtualMachine,
            source_app: None,
            source_app_id: None,
            source_attribution: AttributionQuality::ExactClient,
            preview_text: None,
            content_type: "text/plain".to_owned(),
            timestamp_unix_ms: None,
            thumbnail_png_base64: None,
            byte_count: None,
            confirmation_required: false,
            capability_preflight: None,
        };
        assert_eq!(source_label(&candidate), "dev");
    }

    #[test]
    fn unsafe_local_view_model_shows_provider_posture_realm_and_warning() {
        let destination = crate::protocol::DestinationMetadata {
            realm: "host-tools".to_owned(),
            realm_kind: RealmKind::UnsafeLocal,
            canonical_target: None,
            provider_kind: PresentationProviderKind::UnsafeLocal,
            isolation_posture: PresentationIsolationPosture::UnsafeLocal,
            application: Some("Browser".to_owned()),
            app_id: Some("org.example.Browser".to_owned()),
            title: None,
            workspace: None,
            output: None,
            attribution: Some(AttributionQuality::ExactClient),
            capability_preflight: None,
        };

        let presentation = destination_presentation(&destination);
        assert_eq!(
            presentation.provenance_label(),
            "Realm host-tools · provider unsafe-local · isolation unsafe-local"
        );
        assert_eq!(presentation.warning, Some(UNSAFE_LOCAL_WARNING));
    }

    #[test]
    fn normal_vm_title_and_realm_label_remain_unchanged() {
        let destination = crate::protocol::DestinationMetadata {
            realm: "dev".to_owned(),
            realm_kind: RealmKind::Vm,
            canonical_target: None,
            provider_kind: PresentationProviderKind::LocalVm,
            isolation_posture: PresentationIsolationPosture::VirtualMachine,
            application: Some("Firefox".to_owned()),
            app_id: Some("d2b.looks-unsafe.firefox".to_owned()),
            title: None,
            workspace: None,
            output: None,
            attribution: Some(AttributionQuality::ExactClient),
            capability_preflight: None,
        };
        let request = OpenRequest {
            selected_protocol_version: 2,
            clipd_version: "test".to_owned(),
            picker_version: "test".to_owned(),
            request_id: "request".to_owned(),
            destination: destination.clone(),
            requested_mime_type: "text/plain".to_owned(),
            expires_at_unix_ms: None,
            placement_hints: None,
            candidates: Vec::new(),
            realm_display: HashMap::new(),
        };

        assert_eq!(destination_label(&request), "Paste into dev · Firefox");
        let presentation = destination_presentation(&destination);
        assert_eq!(
            presentation.provenance_label(),
            "Realm dev · provider local-vm · isolation virtual-machine"
        );
        assert_eq!(presentation.warning, None);
    }

    #[test]
    fn app_id_never_infers_unsafe_local_identity() {
        let destination = crate::protocol::DestinationMetadata {
            realm: "dev".to_owned(),
            realm_kind: RealmKind::Vm,
            canonical_target: None,
            provider_kind: PresentationProviderKind::LocalVm,
            isolation_posture: PresentationIsolationPosture::VirtualMachine,
            application: None,
            app_id: Some("unsafe-local.fabricated".to_owned()),
            title: None,
            workspace: None,
            output: None,
            attribution: Some(AttributionQuality::ExactClient),
            capability_preflight: None,
        };

        assert_eq!(destination_presentation(&destination).warning, None);
    }

    #[test]
    fn theme_palette_accepts_safe_colors() {
        let palette = ThemePalette {
            background: "#010203".to_owned(),
            foreground: "#111213".to_owned(),
            border: "#212223".to_owned(),
            accent: "#313233".to_owned(),
            selected_background: "alpha(#313233, 0.14)".to_owned(),
            realm_background: "alpha(#313233, 0.14)".to_owned(),
            realm_header_background: "alpha(#313233, 0.10)".to_owned(),
            search_background: "alpha(currentColor, 0.07)".to_owned(),
            warning_background: "alpha(#414243, 1)".to_owned(),
        };
        palette.validate().expect("palette should validate");
    }

    #[test]
    fn theme_palette_rejects_uppercase_hex_and_arbitrary_css() {
        let mut palette = ThemePalette {
            background: "#ABCDEF".to_owned(),
            ..ThemePalette::default()
        };
        assert!(palette.validate().is_err());

        palette.background = "url(file:///tmp/x)".to_owned();
        assert!(palette.validate().is_err());
    }

    #[test]
    fn startup_inactive_transition_is_ignored_then_focus_loss_cancels_once() {
        let mut lifecycle = FocusLifecycle::default();

        assert_eq!(
            lifecycle.focus_changed(false),
            FocusTransition::None,
            "startup inactivity must not dismiss the picker"
        );
        assert_eq!(lifecycle.focus_changed(true), FocusTransition::None);
        assert_eq!(lifecycle.focus_changed(false), FocusTransition::Cancel);
        assert_eq!(lifecycle.focus_changed(false), FocusTransition::None);
        assert!(!lifecycle.claim_terminal(), "cancel must be claimed once");
    }

    #[test]
    fn picker_bootstraps_focus_then_uses_on_demand_keyboard_interactivity() {
        assert_eq!(PICKER_BOOTSTRAP_KEYBOARD_MODE, KeyboardMode::Exclusive);
        assert_eq!(PICKER_STEADY_KEYBOARD_MODE, KeyboardMode::OnDemand);
        assert_eq!(PIN_ICON_NAME, "view-pin-symbolic");
        assert_eq!(PIN_ICON_THEME, "Adwaita");
        assert_eq!(parse_pin_color("#f8f8f2"), [0xf8, 0xf8, 0xf2]);
        assert_eq!(parse_pin_color("currentColor"), [0xf8, 0xf8, 0xf2]);
    }

    #[test]
    fn generated_pin_fallback_is_deterministic_and_monochrome() {
        let mask = ["#.", ".#"];
        let pixels = generated_pin_pixels(&mask, [10, 20, 30]);

        assert_eq!(pixels.len(), 16);
        assert_eq!(&pixels[0..4], &[10, 20, 30, 255]);
        assert_eq!(&pixels[4..8], &[0, 0, 0, 0]);
        assert_eq!(&pixels[8..12], &[0, 0, 0, 0]);
        assert_eq!(&pixels[12..16], &[10, 20, 30, 255]);
    }

    #[test]
    fn pin_suppresses_focus_loss_and_unpinning_while_inactive_cancels() {
        let mut lifecycle = FocusLifecycle::default();

        lifecycle.focus_changed(true);
        assert_eq!(lifecycle.set_pinned(true), FocusTransition::None);
        assert!(lifecycle.is_pinned());
        assert_eq!(lifecycle.focus_changed(false), FocusTransition::None);
        assert_eq!(lifecycle.set_pinned(false), FocusTransition::Cancel);
        assert_eq!(lifecycle.set_pinned(false), FocusTransition::None);
    }

    #[test]
    fn explicit_terminal_action_prevents_focus_loss_cancel() {
        let mut lifecycle = FocusLifecycle::default();

        lifecycle.focus_changed(true);
        assert!(lifecycle.claim_terminal());
        assert_eq!(lifecycle.focus_changed(false), FocusTransition::None);
        assert!(!lifecycle.claim_terminal());
    }

    #[test]
    fn render_sample_contract_is_synthetic_and_visually_varied() {
        let request = render_sample_request();
        let realms = request
            .candidates
            .iter()
            .map(|candidate| candidate.source_realm.as_str())
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(request.request_id, "synthetic-render-request");
        request.validate().expect("synthetic request must be valid");
        assert!(request.placement_hints.is_some());
        assert!(realms.len() >= 3);
        assert!(
            request
                .candidates
                .iter()
                .any(|candidate| candidate.content_type == "image/png")
        );
        assert!(request.candidates.iter().any(|candidate| {
            candidate.source_isolation_posture == PresentationIsolationPosture::UnsafeLocal
        }));
    }
}
