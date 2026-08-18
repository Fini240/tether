//! Tether's window — the same app on macOS and Windows.
//!
//! egui rather than a webview: the centrepiece is a drag-and-drop canvas of
//! screen rectangles, which is a drawing surface rather than a document, and
//! this way the whole app is one Rust binary with no Node toolchain and nothing
//! to bundle beside it.
//!
//! The daemon runs *in this process*, on its own thread with its own runtime,
//! and talks to the UI through `tether_daemon::control`. No child process, no
//! log scraping, no IPC — and on macOS it means one Accessibility grant covers
//! both, because there is only one binary to grant.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod canvas;

use std::path::PathBuf;

use eframe::egui;
use tether_core::config::{Config, Role};
use tether_core::layout::MachineId;
use tether_daemon::control::{self, Command, Status, UiControl};
use tether_daemon::{auto, clientmode, host};
use tether_net::Identity;
use tether_platform::BackendKind;

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,mdns_sd=off")),
        )
        .with_target(false)
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 700.0])
            .with_min_inner_size([760.0, 560.0])
            .with_title("Tether")
            .with_icon(load_icon())
            // Wayland ignores the embedded icon entirely: a compositor matches
            // a window to its icon, its name and its place in the launcher by
            // app_id, which must equal the basename of an installed .desktop
            // file. Without this, KDE shows a generic grey square and cannot
            // group the window with its launcher entry, however good the icon
            // baked into the binary is. X11 reads the same string as WM_CLASS.
            .with_app_id("dev.tether.Tether"),
        ..Default::default()
    };

    eframe::run_native(
        "Tether",
        options,
        Box::new(|cc| Ok(Box::new(TetherApp::new(cc)))),
    )
}

/// The app icon, decoded from the same PNG the macOS bundle uses. Gives the
/// Windows taskbar and title bar something other than a blank square.
fn load_icon() -> egui::IconData {
    const PNG: &[u8] = include_bytes!("../../../packaging/macos/Tether-1024.png");
    match tether_platform::clipboard::decode_png(PNG) {
        Ok((width, height, rgba)) => egui::IconData {
            rgba,
            width: width as u32,
            height: height as u32,
        },
        Err(_) => egui::IconData {
            rgba: vec![0; 4],
            width: 1,
            height: 1,
        },
    }
}

/// A daemon running on its own thread.
struct Session {
    role: Role,
    control: UiControl,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Set when Stop has been asked for and we are waiting for it to land.
    stopping_since: Option<std::time::Instant>,
}

/// How long to keep waiting for a session to wind down before giving up on it.
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(6);

struct TetherApp {
    config: Config,
    config_path: PathBuf,
    identity: Identity,
    session: Option<Session>,

    pairing: bool,
    status: Status,
    error: Option<String>,
    doctor: Option<String>,
    /// Machine being dragged on the canvas, and the layout as it looked when
    /// the drag started — so the whole gesture is one edit, not a hundred.
    drag: Option<canvas::Drag>,
}

impl TetherApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        // `--config <path>`, parsed by hand rather than pulling an argument
        // parser into a GUI that has exactly one option.
        let mut args = std::env::args().skip(1);
        let mut override_path = None;
        while let Some(arg) = args.next() {
            if arg == "--config" {
                override_path = args.next().map(PathBuf::from);
            }
        }

        let config_path = override_path
            .or_else(|| Config::default_path().ok())
            .unwrap_or_else(|| PathBuf::from("config.json"));
        let config = Config::load(&config_path).unwrap_or_default();

        let state_dir = config_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));

        // A missing identity is fatal — without a certificate this machine
        // cannot authenticate to anything — but it must not take the window
        // down before it can say so.
        let (identity, error) = match Identity::load_or_generate(&state_dir) {
            Ok(identity) => (Some(identity), None),
            Err(err) => (
                None,
                Some(format!("could not load this machine's identity: {err}")),
            ),
        };

        let identity = identity.unwrap_or_else(|| {
            Identity::generate().expect("generating an in-memory identity cannot fail")
        });

        Self {
            config,
            config_path,
            identity,
            session: None,
            pairing: false,
            status: Status::default(),
            error,
            doctor: None,
            drag: None,
        }
    }

    fn running(&self) -> bool {
        self.session.is_some() && self.status.running
    }

    fn start(&mut self, role: Role) {
        self.stop();
        self.error = None;

        let (daemon, ui) = control::channel();
        let config = self.config.clone();
        let identity = self.identity.clone();
        let config_path = self.config_path.clone();
        let pairing = self.pairing;
        let port = self.config.port;
        let address = self.config.address.clone();

        let thread = std::thread::Builder::new()
            .name("tether-session".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        daemon.status.update(|s| {
                            s.running = false;
                            s.error = Some(format!("could not start the runtime: {err}"));
                        });
                        return;
                    }
                };

                let status = daemon.status.clone();
                let result = runtime.block_on(async move {
                    let mut backend = tether_platform::Backend::new(BackendKind::Native)?;
                    match role {
                        Role::Auto => {
                            auto::run(
                                auto::Options {
                                    bind: "0.0.0.0".to_string(),
                                    port,
                                    pairing,
                                    config_path,
                                    control: Some(daemon),
                                },
                                config,
                                identity,
                                &mut backend,
                            )
                            .await
                        }
                        Role::Host => host::run(
                            host::Options {
                                bind: format!("0.0.0.0:{port}"),
                                pairing,
                                config_path,
                                advertise: true,
                                ready: None,
                                control: Some(daemon),
                                supersede: None,
                            },
                            config,
                            identity,
                            &mut backend,
                        )
                        .await
                        .map(|_| ()),
                        Role::Client => clientmode::run(
                            clientmode::Options {
                                address,
                                pairing,
                                config_path,
                                control: Some(daemon),
                                auto: false,
                            },
                            config,
                            identity,
                            &mut backend,
                        )
                        .await
                        .map(|_| ()),
                    }
                });

                if let Err(err) = result {
                    status.update(|s| {
                        s.running = false;
                        s.error = Some(format!("{err:#}"));
                        s.detail = "stopped".into();
                    });
                }
            });

        match thread {
            Ok(thread) => {
                self.session = Some(Session {
                    role,
                    control: ui,
                    thread: Some(thread),
                    stopping_since: None,
                });
                self.pairing = false;
            }
            Err(err) => self.error = Some(format!("could not start a session: {err}")),
        }
    }

    /// Ask the session to stop. Never blocks.
    ///
    /// Blocking the UI thread on a join here froze the whole window, and a
    /// frozen window says nothing at all about what is wrong. The session is
    /// kept and reaped in `poll` instead, so the window keeps drawing and can
    /// say "stopping…" while it happens.
    fn stop(&mut self) {
        if let Some(session) = &mut self.session {
            if session.stopping_since.is_none() {
                session.control.send(Command::Stop);
                session.stopping_since = Some(std::time::Instant::now());
            }
        }
    }

    /// Stop and wait, for shutdown only.
    ///
    /// Worth blocking for here: on macOS the session holds an event tap, and
    /// leaving one installed by a process that has gone away is how you end up
    /// with a keyboard that does nothing.
    fn stop_and_wait(&mut self) {
        self.stop();
        if let Some(mut session) = self.session.take() {
            if let Some(thread) = session.thread.take() {
                let deadline = std::time::Instant::now() + STOP_GRACE;
                while !thread.is_finished() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                if thread.is_finished() {
                    let _ = thread.join();
                }
            }
        }
        self.status = Status::default();
    }

    /// Clear a stopping session once its thread has actually finished.
    fn reap(&mut self) {
        let finished = match &self.session {
            Some(session) => match (&session.stopping_since, &session.thread) {
                (Some(since), Some(thread)) => thread.is_finished() || since.elapsed() > STOP_GRACE,
                (Some(_), None) => true,
                _ => false,
            },
            None => false,
        };
        if !finished {
            return;
        }

        if let Some(mut session) = self.session.take() {
            match session.thread.take() {
                Some(thread) if thread.is_finished() => {
                    let _ = thread.join();
                }
                Some(_) => {
                    // Past the grace period and still running. Letting go of
                    // the handle is the lesser evil: the alternative is a
                    // window that never responds again.
                    self.error = Some(
                        "the session did not stop cleanly. Quit and reopen Tether                          before starting another one."
                            .into(),
                    );
                }
                None => {}
            }
        }
        self.status = Status::default();
    }

    fn poll(&mut self) {
        self.reap();
        if let Some(session) = &self.session {
            self.status = session.control.status();
            if let Some(err) = self.status.error.clone() {
                self.error = Some(err);
            }
        }
    }

    fn send(&self, command: Command) {
        if let Some(session) = &self.session {
            session.control.send(command);
        }
    }
}

impl eframe::App for TetherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        let busy = self.running()
            || self
                .session
                .as_ref()
                .is_some_and(|s| s.stopping_since.is_some());
        if busy {
            // The daemon changes state on its own schedule; without this the
            // window would only refresh when the mouse moves over it — and a
            // window that stops redrawing is indistinguishable from one that
            // has hung.
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }

        egui::TopBottomPanel::top("header").show(ctx, |ui| self.header(ui));
        egui::SidePanel::right("controls")
            .resizable(false)
            .exact_width(280.0)
            .show(ctx, |ui| self.controls(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.arrangement(ui));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Closing the window must not leave a daemon holding an event tap and
        // suppressing the keyboard, so this one does wait.
        self.stop_and_wait();
    }
}

impl TetherApp {
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.heading("Tether");
            ui.add_space(8.0);

            let (dot, label) = match (&self.session, self.status.running) {
                (Some(session), _) if session.stopping_since.is_some() => (
                    egui::Color32::from_rgb(255, 179, 64),
                    "Stopping…".to_string(),
                ),
                (Some(session), true) => (
                    egui::Color32::from_rgb(52, 199, 89),
                    match session.role {
                        Role::Auto => "Connected — any keyboard drives".to_string(),
                        Role::Host => "Host — this keyboard drives".to_string(),
                        Role::Client => "Client".to_string(),
                    },
                ),
                (Some(_), false) => (egui::Color32::from_rgb(255, 179, 64), "starting…".into()),
                (None, _) => (egui::Color32::GRAY, "Stopped".into()),
            };

            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.0, dot);
            ui.label(egui::RichText::new(label).strong());

            if !self.status.detail.is_empty() {
                ui.label(
                    egui::RichText::new(format!("· {}", self.status.detail))
                        .color(ui.visuals().weak_text_color()),
                );
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(&self.config.name).color(ui.visuals().weak_text_color()),
                );
            });
        });
        ui.add_space(8.0);
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);

        self.permission_banner(ui);

        if let Some(error) = self.error.clone() {
            ui.colored_label(egui::Color32::from_rgb(255, 105, 97), error);
            if ui.button("Dismiss").clicked() {
                self.error = None;
            }
            ui.add_space(8.0);
        }

        ui.heading("Session");
        ui.add_space(4.0);

        if let Some(session) = &self.session {
            let stopping = session.stopping_since.is_some();
            let label = if stopping { "Stopping…" } else { "Stop" };
            if ui
                .add_sized(
                    [ui.available_width(), 32.0],
                    egui::Button::new(label).sense(if stopping {
                        egui::Sense::hover()
                    } else {
                        egui::Sense::click()
                    }),
                )
                .clicked()
            {
                self.stop();
            }
        } else {
            ui.horizontal(|ui| {
                ui.label("Host");
                let mut address = self.config.address.clone().unwrap_or_default();
                let response = ui.add(
                    egui::TextEdit::singleline(&mut address)
                        .hint_text("found automatically")
                        .desired_width(f32::INFINITY),
                );
                if response.changed() {
                    let trimmed = address.trim().to_string();
                    self.config.address = if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed)
                    };
                }
                if response.lost_focus() {
                    if let Err(err) = self.config.save(&self.config_path) {
                        self.error = Some(format!("could not save: {err}"));
                    }
                }
            })
            .response
            .on_hover_text(
                "Leave empty to find the host over the network. Fill in \
                 192.168.1.50:24800 to connect directly — useful when discovery \
                 finds the machine but hands out an address it cannot be \
                 reached on.",
            );
            ui.add_space(4.0);

            ui.checkbox(&mut self.pairing, "Pair with a new machine")
                .on_hover_text(
                    "Accepts a machine that has never connected before. Leave it off \
                     once yours are set up — while it is on, anything on this network \
                     can connect.",
                );
            ui.add_space(4.0);

            // One button, because there is one thing anybody wants: turn it
            // on and have the machines find each other. Which of them ends up
            // arbitrating the shared pointer is not a decision worth putting
            // in front of somebody — it is worked out from the network, and it
            // changes by itself when the network does.
            if ui
                .add_sized([ui.available_width(), 36.0], egui::Button::new("Connect"))
                .on_hover_text(
                    "Find the other machines and be reachable from them. Whichever \
                     keyboard you touch drives, and the pointer crosses either way.",
                )
                .clicked()
            {
                self.start(Role::Auto);
            }

            ui.add_space(8.0);
            egui::CollapsingHeader::new("Pin the roles by hand")
                .default_open(false)
                .show(ui, |ui| {
                    ui.label(
                        "Only needed when discovery cannot work — machines on \
                         different subnets, or mDNS blocked.",
                    );
                    ui.add_space(4.0);
                    if ui
                        .add_sized(
                            [ui.available_width(), 28.0],
                            egui::Button::new("Start as Host"),
                        )
                        .on_hover_text("This machine's keyboard and mouse drive the others.")
                        .clicked()
                    {
                        self.start(Role::Host);
                    }
                    ui.add_space(4.0);
                    if ui
                        .add_sized(
                            [ui.available_width(), 28.0],
                            egui::Button::new("Start as Client"),
                        )
                        .on_hover_text("Receive input from a host on this network.")
                        .clicked()
                    {
                        self.start(Role::Client);
                    }
                });
        }

        ui.add_space(14.0);
        ui.heading("Behaviour");
        ui.add_space(4.0);

        let mut changed = false;
        changed |= ui
            .checkbox(
                &mut self.config.options.auto_input_handoff,
                "Follow the keyboard I touch",
            )
            .on_hover_text("Touch this machine's keyboard or trackpad and it takes over driving.")
            .changed();
        changed |= ui
            .checkbox(
                &mut self.config.options.cursor_follows_input,
                "Bring the pointer with it",
            )
            .changed();
        changed |= ui
            .checkbox(
                &mut self.config.options.sync_clipboard,
                "Share the clipboard",
            )
            .changed();
        changed |= ui
            .checkbox(
                &mut self.config.options.lock_screen_on_leave,
                "Lock this screen when I leave it",
            )
            .changed();

        ui.add_space(10.0);
        ui.label(
            egui::RichText::new("Scrolling from other machines")
                .small()
                .color(ui.visuals().weak_text_color()),
        );
        changed |= ui
            .checkbox(
                &mut self.config.options.scroll_invert,
                "Reverse the direction",
            )
            .on_hover_text(
                "For when a wheel on another machine scrolls this one the wrong way — \
                 macOS scrolls naturally by default and Windows does not. Only affects \
                 input arriving from elsewhere; this machine's own trackpad is untouched.",
            )
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(
                    &mut self.config.options.scroll_scale,
                    tether_core::config::SCROLL_SCALE_RANGE,
                )
                .logarithmic(true)
                .text("Speed"),
            )
            .on_hover_text("How far one turn of a remote wheel scrolls here. 1.0 leaves it alone.")
            .changed();

        if changed {
            if let Err(err) = self.config.save(&self.config_path) {
                self.error = Some(format!("could not save settings: {err}"));
            }
            if self.session.is_some() {
                ui.label(
                    egui::RichText::new("Restart the session to apply")
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            }
        }

        if self.running() {
            ui.add_space(6.0);
            let locked = self.status.cursor_locked;
            if ui
                .button(if locked {
                    "Unlock the pointer"
                } else {
                    "Lock the pointer here"
                })
                .clicked()
            {
                self.send(Command::ToggleCursorLock);
            }
        }

        ui.add_space(14.0);
        ui.heading("Machines");
        ui.add_space(4.0);

        if self.status.peers.is_empty() {
            ui.label(
                egui::RichText::new(if self.running() {
                    "None connected yet."
                } else {
                    "Start a session to see connected machines."
                })
                .color(ui.visuals().weak_text_color()),
            );
        }

        let peers = self.status.peers.clone();
        for peer in &peers {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&peer.name).strong());
                ui.label(
                    egui::RichText::new(peer.platform.to_string())
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            });
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&peer.address)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
                if ui.small_button("Go to").clicked() {
                    self.send(Command::JumpTo(peer.machine));
                }
            });
            ui.add_space(4.0);
        }

        ui.add_space(14.0);
        if ui.button("Run a check").clicked() {
            self.doctor = Some(run_doctor());
        }
        if let Some(report) = self.doctor.clone() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(report).monospace().small());
            if ui.small_button("Hide").clicked() {
                self.doctor = None;
            }
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!("fingerprint {}", &self.identity.fingerprint[..16]))
                    .small()
                    .color(ui.visuals().weak_text_color()),
            )
            .on_hover_text(self.identity.display_fingerprint());
        });
    }

    #[cfg(target_os = "macos")]
    fn permission_banner(&mut self, ui: &mut egui::Ui) {
        if tether_platform::check_capture_permission().is_ok() {
            return;
        }

        egui::Frame::default()
            .fill(egui::Color32::from_rgb(70, 50, 20))
            .inner_margin(8.0)
            .corner_radius(6.0)
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Accessibility not granted").strong());
                ui.label(
                    egui::RichText::new(
                        "Tether cannot read or send input until macOS allows it.",
                    )
                    .small(),
                );
                ui.add_space(4.0);
                if ui.button("Open Settings").clicked() {
                    let _ = std::process::Command::new("open")
                        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
                        .spawn();
                }
                if ui
                    .button("Already on? Reset it")
                    .on_hover_text(
                        "After an update the stored permission still points at the \
                         previous build, so macOS keeps denying while showing it as \
                         granted. This clears it so it can be granted again.",
                    )
                    .clicked()
                {
                    let _ = std::process::Command::new("tccutil")
                        .args(["reset", "Accessibility", "dev.tether.Tether"])
                        .status();
                }
            });
        ui.add_space(10.0);
    }

    #[cfg(not(target_os = "macos"))]
    fn permission_banner(&mut self, _ui: &mut egui::Ui) {
        // Windows needs no grant: low-level hooks work for any process at the
        // same integrity level as the one it is driving.
    }

    fn arrangement(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("Screen arrangement");
            ui.label(
                egui::RichText::new("drag a screen to say where it really sits")
                    .color(ui.visuals().weak_text_color()),
            );
        });
        ui.add_space(6.0);

        // Prefer the live layout: a machine that just connected is on the
        // canvas before anything has been saved.
        let layout = if self.status.layout.machines.is_empty() {
            self.config.layout.clone()
        } else {
            self.status.layout.clone()
        };

        if layout.machines.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new(
                        "No machines yet.\n\nStart a session on both machines with \
                         \"Pair with a new machine\" ticked.\nOnce they connect, their \
                         screens appear here and you can drag them into place.",
                    )
                    .color(ui.visuals().weak_text_color()),
                );
            });
            return;
        }

        let this = self.status.this_machine.unwrap_or(self.identity.machine_id);
        let outcome = canvas::show(
            ui,
            &layout,
            this,
            self.status.cursor_on,
            self.status.cursor_position,
            self.status.input_owner,
            &mut self.drag,
        );

        if let Some(new_layout) = outcome {
            self.config.layout = new_layout.clone();
            if let Err(err) = self.config.save(&self.config_path) {
                self.error = Some(format!("could not save the arrangement: {err}"));
            }
            self.send(Command::SetLayout(new_layout));
        }
    }
}

/// Same checks as `tether doctor`, rendered into the panel.
fn run_doctor() -> String {
    use std::fmt::Write;
    let mut out = String::new();

    let _ = writeln!(out, "platform    {}", tether_proto::Platform::current());
    match tether_platform::check_capture_permission() {
        Ok(()) => {
            let _ = writeln!(out, "permission  granted");
        }
        Err(err) => {
            let _ = writeln!(out, "permission  DENIED\n            {err}");
        }
    }

    match tether_platform::Backend::new(BackendKind::Native) {
        Ok(mut backend) => {
            match backend.monitors.enumerate() {
                Ok(monitors) => {
                    let _ = writeln!(out, "displays    {}", monitors.len());
                    for monitor in monitors {
                        let _ = writeln!(
                            out,
                            "            {}x{} at {},{}",
                            monitor.bounds.width,
                            monitor.bounds.height,
                            monitor.bounds.x,
                            monitor.bounds.y
                        );
                    }
                }
                Err(err) => {
                    let _ = writeln!(out, "displays    FAILED: {err}");
                }
            }

            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            match backend.capture.start(tx) {
                Ok(()) => {
                    let before = backend.capture.injected_filtered();
                    let at = backend
                        .pointer
                        .position()
                        .unwrap_or(tether_proto::Point::new(400, 400));
                    for i in 0..5 {
                        let _ = backend.inject.inject(&tether_proto::InputEvent::MouseMove {
                            x: at.x + i,
                            y: at.y,
                        });
                    }
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let after = backend.capture.injected_filtered();
                    backend.capture.stop();

                    // `None` from either end means this backend has no filter
                    // to count, because it injects through devices it never
                    // reads — which is a stronger separation, not a missing
                    // one. Only a backend that *does* count gets judged on the
                    // number.
                    match (before, after) {
                        (Some(before), Some(after)) => {
                            let filtered = after.saturating_sub(before);
                            if filtered >= 5 {
                                let _ = writeln!(out, "handoff     safe ({filtered} filtered)");
                            } else {
                                let _ = writeln!(
                                    out,
                                    "handoff     BROKEN ({filtered} filtered)\n\
                                                 turn off \"follow the keyboard I touch\""
                                );
                            }
                        }
                        _ => {
                            let _ = writeln!(out, "handoff     safe (separate input devices)");
                        }
                    }
                }
                Err(err) => {
                    let _ = writeln!(out, "capture     FAILED: {err}");
                }
            }
        }
        Err(err) => {
            let _ = writeln!(out, "backend     FAILED: {err}");
        }
    }

    out
}

/// Kept so `MachineId` stays in scope for the canvas module's signature.
#[allow(dead_code)]
type _Machine = MachineId;
