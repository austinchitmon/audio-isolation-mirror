#![windows_subsystem = "windows"]

mod audio;
mod config;
mod dsp;
mod winaudio;

use cpal::traits::DeviceTrait;
use eframe::egui;

use audio::{list_input_devices, list_output_devices, AudioEngine, ModeHandle, MuteHandle};
use dsp::ChannelMode;
use winaudio::{AudioSession, EndpointOverride};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelSelection {
    Both,
    Left,
    Right,
}

fn effective_mode(sel: ChannelSelection, mirror: bool) -> ChannelMode {
    match sel {
        ChannelSelection::Both => ChannelMode::Both,
        ChannelSelection::Left => ChannelMode::Left { mirror },
        ChannelSelection::Right => ChannelMode::Right { mirror },
    }
}

fn decompose(mode: ChannelMode) -> (ChannelSelection, bool) {
    match mode {
        ChannelMode::Both => (ChannelSelection::Both, false),
        ChannelMode::Left { mirror } => (ChannelSelection::Left, mirror),
        ChannelMode::Right { mirror } => (ChannelSelection::Right, mirror),
    }
}

/// The app most recently routed to CABLE Input, and what its audio routing
/// looked like before we touched it -- so it can be put back the way it was
/// when the user picks a different app or closes this app.
struct ActiveRoute {
    pid: u32,
    exe_name: String,
    original: EndpointOverride,
}

/// Processes we assume the user wants isolated without having to ask --
/// checked in order, first match wins.
const AUTO_ROUTE_CANDIDATES: [&str; 2] = ["firefox.exe", "chrome.exe"];

const SETTINGS_SIZE: [f32; 2] = [360.0, 130.0];

/// Which frame of the settings window's life to un-hide it on, once its
/// requested size and position have actually been applied by the OS.
const REVEAL_FRAME: u32 = 3;

fn find_auto_route_candidate(sessions: &[AudioSession]) -> Option<usize> {
    sessions.iter().position(|s| {
        AUTO_ROUTE_CANDIDATES
            .iter()
            .any(|name| s.exe_name.eq_ignore_ascii_case(name))
    })
}

fn pick_index(devices: &[cpal::Device], preferred_name: Option<&str>) -> Option<usize> {
    if let Some(name) = preferred_name {
        let needle = name.to_lowercase();
        if let Some(i) = devices.iter().position(|d| {
            d.name()
                .map(|n| n.to_lowercase().contains(&needle))
                .unwrap_or(false)
        }) {
            return Some(i);
        }
    }
    if devices.is_empty() {
        None
    } else {
        Some(0)
    }
}

struct App {
    inputs: Vec<cpal::Device>,
    outputs: Vec<cpal::Device>,
    selected_input: Option<usize>,
    selected_output: Option<usize>,
    channel: ChannelSelection,
    mirror: bool,
    mode_handle: ModeHandle,
    muted: bool,
    mute_handle: MuteHandle,
    engine: Option<AudioEngine>,
    error: Option<String>,
    audio_sessions: Vec<AudioSession>,
    selected_session: Option<usize>,
    route_status: Option<String>,
    pending_route: Option<usize>,
    active_route: Option<ActiveRoute>,
    show_settings: bool,
    settings_pos: Option<egui::Pos2>,
    settings_frames: u32,
}

impl App {
    fn new() -> Self {
        let inputs = list_input_devices();
        let outputs = list_output_devices();
        let config = config::AppConfig::load();

        let selected_input = pick_index(&inputs, config.input_device_name.as_deref());
        let selected_output = pick_index(&outputs, config.output_device_name.as_deref());

        let initial_mode = config.mode_code.map(ChannelMode::from_code).unwrap_or(ChannelMode::Both);
        let (channel, mirror) = decompose(initial_mode);
        let mode_handle = ModeHandle::new(initial_mode);
        let muted = config.muted;
        let mute_handle = MuteHandle::new(muted);

        // Pre-fetch active audio sessions so a running browser can be
        // auto-routed without the user having to hit refresh first.
        let audio_sessions = winaudio::list_active_render_sessions().unwrap_or_default();
        let selected_session = find_auto_route_candidate(&audio_sessions);
        let pending_route = selected_session;

        let mut app = Self {
            inputs,
            outputs,
            selected_input,
            selected_output,
            channel,
            mirror,
            mode_handle,
            muted,
            mute_handle,
            engine: None,
            error: None,
            audio_sessions,
            selected_session,
            route_status: None,
            pending_route,
            active_route: None,
            show_settings: false,
            settings_pos: None,
            settings_frames: 0,
        };
        app.restart_engine();
        app
    }

    fn restart_engine(&mut self) {
        self.engine = None; // drop old streams before starting new ones
        self.error = None;

        let input = self.selected_input.and_then(|i| self.inputs.get(i));
        let output = self.selected_output.and_then(|i| self.outputs.get(i));

        if let (Some(input), Some(output)) = (input, output) {
            match AudioEngine::start(input, output, self.mode_handle.clone(), self.mute_handle.clone()) {
                Ok(engine) => self.engine = Some(engine),
                Err(e) => self.error = Some(e.to_string()),
            }
        }

        self.save_config();
    }

    fn save_config(&self) {
        let input_device_name = self
            .selected_input
            .and_then(|i| self.inputs.get(i))
            .and_then(|d| d.name().ok());
        let output_device_name = self
            .selected_output
            .and_then(|i| self.outputs.get(i))
            .and_then(|d| d.name().ok());

        config::AppConfig {
            input_device_name,
            output_device_name,
            mode_code: Some(effective_mode(self.channel, self.mirror).to_code()),
            muted: self.muted,
        }
        .save();
    }

    /// Actually performs the routing for `pending_route`, if any was queued
    /// last frame. Run at the start of the frame *after* the one that queued
    /// it, so the "Routing..." status has a chance to actually be painted
    /// and presented before this (synchronous, blocking) work runs.
    fn process_pending_route(&mut self) {
        let Some(idx) = self.pending_route.take() else { return };
        let Some(session) = self.audio_sessions.get(idx) else { return };
        let pid = session.pid;
        let exe_name = session.exe_name.clone();

        // Put whichever app we previously routed back the way it was before
        // routing this newly-selected one.
        self.restore_active_route();

        let original = match winaudio::get_endpoint_override(pid) {
            Ok(original) => original,
            Err(e) => {
                self.error = Some(format!(
                    "Couldn't read {exe_name}'s current audio routing: {e}"
                ));
                return;
            }
        };

        match winaudio::find_render_endpoint_id_by_name("CABLE Input") {
            Ok(Some(device_id)) => match winaudio::route_process_to_endpoint(pid, &device_id) {
                Ok(()) => {
                    self.error = None;
                    self.route_status = Some(format!(
                        "{exe_name} is now routed to CABLE Input. If you don't hear it \
                         come through yet, refresh/restart playback in that app."
                    ));
                    self.active_route = Some(ActiveRoute { pid, exe_name, original });
                }
                Err(e) => {
                    self.error = Some(format!(
                        "Automatic app routing isn't available on this Windows version ({e}). \
                         Route manually instead: Settings -> System -> Sound -> Volume mixer \
                         -> set your browser's output to 'CABLE Input'."
                    ));
                }
            },
            Ok(None) => {
                self.error = Some(
                    "Couldn't find a 'CABLE Input' playback device -- is \
                     VB-Audio Virtual Cable installed?"
                        .to_string(),
                );
            }
            Err(e) => {
                self.error = Some(format!("Couldn't look up the CABLE Input device: {e}"));
            }
        }
    }

    /// Puts the currently-routed app's audio back the way it was before this
    /// app touched it. Called when the user switches to routing a different
    /// app, and on shutdown so the system doesn't stay silently repointed at
    /// CABLE Input after this app closes.
    fn restore_active_route(&mut self) {
        if let Some(prev) = self.active_route.take() {
            if let Err(e) = winaudio::restore_endpoint(prev.pid, &prev.original) {
                eprintln!("failed to restore {}'s audio routing: {e}", prev.exe_name);
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_pending_route();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Audio Channel Isolator");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙").on_hover_text("Settings").clicked() {
                        self.show_settings = !self.show_settings;
                        if self.show_settings {
                            self.settings_frames = 0;
                            // Centered over the main window, rather than
                            // wherever the OS would otherwise place it.
                            self.settings_pos =
                                ctx.input(|i| i.viewport().outer_rect).map(|r| {
                                    r.center()
                                        - egui::vec2(
                                            SETTINGS_SIZE[0] / 2.0,
                                            SETTINGS_SIZE[1] / 2.0,
                                        )
                                });
                        }
                    }
                });
            });
            ui.separator();

            let mut device_changed = false;

            ui.label("Output (your real headphones/speakers)");
            egui::ComboBox::from_id_salt("output_device")
                .selected_text(
                    self.selected_output
                        .and_then(|i| self.outputs.get(i))
                        .and_then(|d| d.name().ok())
                        .unwrap_or_else(|| "<none>".to_string()),
                )
                .show_ui(ui, |ui| {
                    for i in 0..self.outputs.len() {
                        let name = self.outputs[i].name().unwrap_or_default();
                        if ui
                            .selectable_value(&mut self.selected_output, Some(i), name)
                            .changed()
                        {
                            device_changed = true;
                        }
                    }
                });

            if device_changed {
                self.restart_engine();
            }

            ui.separator();
            ui.label("Send your livestream into the cable");
            ui.horizontal(|ui| {
                if ui
                    .button("🔄")
                    .on_hover_text("Detect apps with audio playing")
                    .clicked()
                {
                    self.route_status = None;
                    match winaudio::list_active_render_sessions() {
                        Ok(sessions) => {
                            self.selected_session = None;
                            self.error = None;
                            if sessions.is_empty() {
                                self.error = Some(
                                    "No apps are currently playing audio. Start playback in \
                                     your browser tab, then refresh again."
                                        .to_string(),
                                );
                            }
                            self.audio_sessions = sessions;
                        }
                        Err(e) => self.error = Some(format!("Couldn't detect apps: {e}")),
                    }
                }

                egui::ComboBox::from_id_salt("audio_session")
                    .selected_text(
                        self.selected_session
                            .and_then(|i| self.audio_sessions.get(i))
                            .map(|s| format!("{} (pid {})", s.exe_name, s.pid))
                            .unwrap_or_else(|| "<none detected>".to_string()),
                    )
                    .show_ui(ui, |ui| {
                        for i in 0..self.audio_sessions.len() {
                            let label = format!(
                                "{} (pid {})",
                                self.audio_sessions[i].exe_name, self.audio_sessions[i].pid
                            );
                            if ui
                                .selectable_value(&mut self.selected_session, Some(i), label)
                                .changed()
                            {
                                self.route_status = None;
                                self.error = None;
                                self.pending_route = Some(i);
                                ctx.request_repaint();
                            }
                        }
                    });
            });

            if self.pending_route.is_some() {
                ui.colored_label(egui::Color32::YELLOW, "Routing to CABLE Input...");
            } else if let Some(status) = &self.route_status {
                ui.colored_label(egui::Color32::GREEN, status);
            }

            ui.separator();
            ui.label("Channel mode");

            let mut mode_changed = false;
            ui.horizontal(|ui| {
                mode_changed |= ui
                    .radio_value(&mut self.channel, ChannelSelection::Both, "Both")
                    .changed();
                mode_changed |= ui
                    .radio_value(&mut self.channel, ChannelSelection::Left, "Left")
                    .changed();
                mode_changed |= ui
                    .radio_value(&mut self.channel, ChannelSelection::Right, "Right")
                    .changed();
            });

            ui.add_enabled_ui(self.channel != ChannelSelection::Both, |ui| {
                if ui.checkbox(&mut self.mirror, "Mirror to both channels").changed() {
                    mode_changed = true;
                }
            });

            if mode_changed {
                self.mode_handle.set(effective_mode(self.channel, self.mirror));
                self.save_config();
            }

            ui.separator();
            if ui
                .checkbox(&mut self.muted, "Mute")
                .on_hover_text(
                    "Silences only this app's forwarded audio. Does not mute the output device for other apps.",
                )
                .changed()
            {
                self.mute_handle.set(self.muted);
                self.save_config();
            }

            // Pick up stream errors from the audio callback thread (e.g. a device
            // was unplugged mid-stream) so they surface here instead of only stderr.
            if let Some(engine) = &self.engine {
                if let Some(err) = engine.take_error() {
                    self.error = Some(err);
                    self.engine = None;
                }
            }

            ui.separator();
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, format!("Error: {err}"));
            } else if self.engine.is_some() {
                ui.colored_label(egui::Color32::GREEN, "Running");
            } else {
                ui.label("Select an input and output device to start.");
            }
        });

        if self.show_settings {
            let mut input_changed = false;
            let mut close_requested = false;

            // Stays hidden for the first few frames: the OS briefly shows the
            // window near fullscreen at its default spot before our size and
            // position take effect, which flashes on screen.
            let reveal = self.settings_frames >= REVEAL_FRAME;

            // The main window is always-on-top; Settings must be too, or
            // Windows' topmost z-order band forces it underneath.
            let mut builder = egui::ViewportBuilder::default()
                .with_title("Settings")
                .with_inner_size(SETTINGS_SIZE)
                .with_always_on_top()
                // No .with_active(): egui 0.29's ViewportBuilder::patch diffs
                // `visible` against `active`, so setting both means the reveal
                // below never reaches the OS and the window stays hidden.
                .with_visible(reveal);
            if let Some(pos) = self.settings_pos {
                builder = builder.with_position(pos);
            }

            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("settings_viewport"),
                builder,
                |ctx, _class| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.label("Input (capture from virtual cable)");
                        egui::ComboBox::from_id_salt("input_device")
                            .selected_text(
                                self.selected_input
                                    .and_then(|i| self.inputs.get(i))
                                    .and_then(|d| d.name().ok())
                                    .unwrap_or_else(|| "<none>".to_string()),
                            )
                            .show_ui(ui, |ui| {
                                for i in 0..self.inputs.len() {
                                    let name = self.inputs[i].name().unwrap_or_default();
                                    if ui
                                        .selectable_value(&mut self.selected_input, Some(i), name)
                                        .changed()
                                    {
                                        input_changed = true;
                                    }
                                }
                            });
                    });

                    if ctx.input(|i| i.viewport().close_requested()) {
                        close_requested = true;
                    }
                },
            );

            if !reveal {
                self.settings_frames += 1;
                ctx.request_repaint();
            }

            if input_changed {
                self.restart_engine();
            }
            if close_requested {
                self.show_settings = false;
            }
        }

        // Stream errors arrive on a background audio thread with no repaint of
        // their own; poll periodically so a mid-stream disconnect shows up promptly.
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.restore_active_route();
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 320.0])
            .with_always_on_top(),
        ..Default::default()
    };
    eframe::run_native(
        "Audio Channel Isolator",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}
