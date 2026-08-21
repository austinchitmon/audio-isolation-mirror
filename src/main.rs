#![windows_subsystem = "windows"]

use std::collections::{HashMap, VecDeque};

mod audio;
mod config;
mod dsp;
mod winaudio;

use cpal::traits::DeviceTrait;
use eframe::egui;

use audio::{
    default_output_device_name, list_input_devices, list_output_devices, AudioEngine, ModeHandle,
    MuteHandle,
};
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

/// Common browser process names -- used both to auto-route on launch without
/// asking, and (when Simple Browser Selection is on) as the fixed set of
/// icons shown instead of the full running-process dropdown.
const KNOWN_BROWSERS: [&str; 5] = ["chrome.exe", "firefox.exe", "msedge.exe", "brave.exe", "opera.exe"];

const SETTINGS_SIZE: [f32; 2] = [360.0, 280.0];

/// Which frame of the settings window's life to un-hide it on, once its
/// requested size and position have actually been applied by the OS.
const REVEAL_FRAME: u32 = 3;

/// Substring VB-CABLE's recording device registers under, e.g.
/// "CABLE Output (VB-Audio Virtual Cable)".
const VB_CABLE_NAME_HINT: &str = "VB-Audio Virtual Cable";

const VB_CABLE_DOWNLOAD_URL: &str = "https://vb-audio.com/Cable/";

/// How many recent lines the developer console keeps before dropping the
/// oldest -- unbounded growth over a long-running session isn't worth it.
const DEV_LOG_MAX_LINES: usize = 200;

/// Lists active audio sessions, excluding this app's own process -- routing
/// yourself into CABLE Input is never something a user would want to pick.
fn list_audio_sessions() -> anyhow::Result<Vec<AudioSession>> {
    let self_pid = std::process::id();
    Ok(winaudio::list_active_render_sessions()?
        .into_iter()
        .filter(|s| s.pid != self_pid)
        .collect())
}

fn find_auto_route_candidate(sessions: &[AudioSession]) -> Option<usize> {
    sessions.iter().position(|s| {
        KNOWN_BROWSERS
            .iter()
            .any(|name| s.exe_name.eq_ignore_ascii_case(name))
    })
}

fn find_by_name(devices: &[cpal::Device], name: &str) -> Option<usize> {
    let needle = name.to_lowercase();
    devices.iter().position(|d| {
        d.name()
            .map(|n| n.to_lowercase().contains(&needle))
            .unwrap_or(false)
    })
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
    show_vb_cable_warning: bool,
    dev_console: bool,
    dev_log: VecDeque<String>,
    simple_browser_selection: bool,
    /// Icon textures for `KNOWN_BROWSERS`, loaded lazily and cached by exe
    /// name the first time each browser actually shows up in the session
    /// list -- keeps this out of the hot path for users who never enable
    /// Simple Browser Selection.
    browser_icons: HashMap<String, Option<egui::TextureHandle>>,
}

impl App {
    fn new() -> Self {
        // Buffers startup log points; self doesn't exist yet for `self.log`
        // to write into, so this gets folded into `dev_log` once it does.
        let mut dev_log: VecDeque<String> = VecDeque::new();
        let mut startup_log = |msg: String| {
            eprintln!("{msg}");
            dev_log.push_back(msg);
        };

        let inputs = list_input_devices();
        let outputs = list_output_devices();
        let config = config::AppConfig::load();

        // Prefer whatever the user explicitly picked last time; otherwise
        // assume VB-Audio Virtual Cable is installed and find it ourselves,
        // so non-technical users never have to open Settings at all.
        startup_log("searching for VB-Audio Virtual Cable...".to_string());
        let vb_cable_present = find_by_name(&inputs, VB_CABLE_NAME_HINT).is_some();
        startup_log(if vb_cable_present {
            "found VB-Audio Virtual Cable".to_string()
        } else {
            "VB-Audio Virtual Cable not found".to_string()
        });
        let selected_input = config
            .input_device_name
            .as_deref()
            .and_then(|name| find_by_name(&inputs, name))
            .or_else(|| find_by_name(&inputs, VB_CABLE_NAME_HINT))
            .or(if inputs.is_empty() { None } else { Some(0) });
        // Same idea for output: prefer a saved choice, otherwise assume the
        // user wants whatever the OS considers the default playback device.
        let selected_output = config
            .output_device_name
            .as_deref()
            .and_then(|name| find_by_name(&outputs, name))
            .or_else(|| default_output_device_name().and_then(|name| find_by_name(&outputs, &name)))
            .or(if outputs.is_empty() { None } else { Some(0) });
        startup_log(format!(
            "selected input: {}, output: {}",
            selected_input
                .and_then(|i| inputs.get(i))
                .and_then(|d| d.name().ok())
                .unwrap_or_else(|| "<none>".to_string()),
            selected_output
                .and_then(|i| outputs.get(i))
                .and_then(|d| d.name().ok())
                .unwrap_or_else(|| "<none>".to_string()),
        ));

        let initial_mode = config.mode_code.map(ChannelMode::from_code).unwrap_or(ChannelMode::Both);
        let (channel, mirror) = decompose(initial_mode);
        let mode_handle = ModeHandle::new(initial_mode);
        let muted = config.muted;
        let mute_handle = MuteHandle::new(muted);

        // Pre-fetch active audio sessions so a running browser can be
        // auto-routed without the user having to hit refresh first.
        startup_log("checking for active Firefox/Chrome audio sessions...".to_string());
        let audio_sessions = list_audio_sessions().unwrap_or_default();
        let selected_session = find_auto_route_candidate(&audio_sessions);
        startup_log(match selected_session.and_then(|i| audio_sessions.get(i)) {
            Some(s) => format!("auto-selected {} (pid {}) for routing", s.exe_name, s.pid),
            None => "no Firefox/Chrome session found to auto-route".to_string(),
        });
        let pending_route = selected_session;

        while dev_log.len() > DEV_LOG_MAX_LINES {
            dev_log.pop_front();
        }

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
            show_vb_cable_warning: !vb_cable_present,
            dev_console: config.dev_console,
            dev_log,
            simple_browser_selection: config.simple_browser_selection,
            browser_icons: HashMap::new(),
        };
        app.restart_engine();
        app
    }

    fn restart_engine(&mut self) {
        self.engine = None; // drop old streams before starting new ones
        self.error = None;

        let input_name = self
            .selected_input
            .and_then(|i| self.inputs.get(i))
            .and_then(|d| d.name().ok());
        let output_name = self
            .selected_output
            .and_then(|i| self.outputs.get(i))
            .and_then(|d| d.name().ok());
        if let (Some(input_name), Some(output_name)) = (&input_name, &output_name) {
            self.log(format!(
                "starting audio engine: input={input_name}, output={output_name}"
            ));
        }

        let input = self.selected_input.and_then(|i| self.inputs.get(i));
        let output = self.selected_output.and_then(|i| self.outputs.get(i));

        if let (Some(input), Some(output)) = (input, output) {
            match AudioEngine::start(input, output, self.mode_handle.clone(), self.mute_handle.clone()) {
                Ok(engine) => {
                    self.engine = Some(engine);
                    self.log("audio engine started".to_string());
                }
                Err(e) => self.set_error(e.to_string()),
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
            dev_console: self.dev_console,
            simple_browser_selection: self.simple_browser_selection,
        }
        .save();
    }

    /// Appends a line to the developer console, dropping the oldest once it
    /// grows past `DEV_LOG_MAX_LINES`. Also printed to stderr so it's still
    /// visible when run from a terminal with the console hidden.
    fn log(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        eprintln!("{msg}");
        self.dev_log.push_back(msg);
        if self.dev_log.len() > DEV_LOG_MAX_LINES {
            self.dev_log.pop_front();
        }
    }

    fn set_error(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.log(format!("error: {msg}"));
        self.error = Some(msg);
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.log(msg.clone());
        self.route_status = Some(msg);
    }

    /// Returns the cached icon texture for a `KNOWN_BROWSERS` exe, extracting
    /// it from `exe_path` and caching the result (including a cached "no
    /// icon" on failure) the first time this exe name is seen.
    fn browser_icon_texture(
        &mut self,
        ctx: &egui::Context,
        exe_name: &str,
        exe_path: &str,
    ) -> Option<egui::TextureHandle> {
        let key = exe_name.to_lowercase();
        if let Some(cached) = self.browser_icons.get(&key) {
            return cached.clone();
        }

        let texture = winaudio::extract_exe_icon_rgba(exe_path).map(|(rgba, w, h)| {
            let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            ctx.load_texture(key.clone(), image, egui::TextureOptions::default())
        });
        if texture.is_none() {
            self.log(format!("couldn't extract icon for {exe_name}"));
        }
        self.browser_icons.insert(key, texture.clone());
        texture
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
        self.log(format!("routing {exe_name} (pid {pid}) to CABLE Input"));

        // Put whichever app we previously routed back the way it was before
        // routing this newly-selected one.
        self.restore_active_route();

        let original = match winaudio::get_endpoint_override(pid) {
            Ok(original) => original,
            Err(e) => {
                self.set_error(format!(
                    "Couldn't read {exe_name}'s current audio routing: {e}"
                ));
                return;
            }
        };

        match winaudio::find_render_endpoint_id_by_name("CABLE Input") {
            Ok(Some(device_id)) => match winaudio::route_process_to_endpoint(pid, &device_id) {
                Ok(()) => {
                    self.error = None;
                    self.set_status(format!(
                        "{exe_name} is now routed to CABLE Input. If you don't hear it \
                         come through yet, refresh/restart playback in that app."
                    ));
                    self.active_route = Some(ActiveRoute { pid, exe_name, original });
                }
                Err(e) => {
                    self.set_error(format!(
                        "Automatic app routing isn't available on this Windows version ({e}). \
                         Route manually instead: Settings -> System -> Sound -> Volume mixer \
                         -> set your browser's output to 'CABLE Input'."
                    ));
                }
            },
            Ok(None) => {
                self.set_error(
                    "Couldn't find a 'CABLE Input' playback device -- is \
                     VB-Audio Virtual Cable installed?"
                        .to_string(),
                );
            }
            Err(e) => {
                self.set_error(format!("Couldn't look up the CABLE Input device: {e}"));
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
                self.log(format!(
                    "failed to restore {}'s audio routing: {e}",
                    prev.exe_name
                ));
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_pending_route();

        egui::CentralPanel::default().show(ctx, |ui| {
            // If the window gets resized shorter than the content needs, show
            // a vertical scrollbar instead of clipping the bottom off.
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
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

            ui.label("Select your browser");
            ui.horizontal(|ui| {
                if self.simple_browser_selection {
                    let mut any_shown = false;
                    for &browser in KNOWN_BROWSERS.iter() {
                        let Some(idx) = self
                            .audio_sessions
                            .iter()
                            .position(|s| s.exe_name.eq_ignore_ascii_case(browser))
                        else {
                            continue;
                        };
                        any_shown = true;
                        let session = &self.audio_sessions[idx];
                        let exe_path = session.exe_path.clone();
                        let hover = format!("{} (pid {})", session.exe_name, session.pid);
                        let selected = self.selected_session == Some(idx);

                        let clicked = match self.browser_icon_texture(ctx, browser, &exe_path) {
                            Some(texture) => {
                                let image = egui::Image::new(&texture)
                                    .fit_to_exact_size(egui::vec2(28.0, 28.0));
                                ui.add(egui::ImageButton::new(image).selected(selected))
                                    .on_hover_text(hover)
                                    .clicked()
                            }
                            // No icon could be extracted -- fall back to a
                            // plain text toggle rather than showing nothing.
                            None => ui
                                .add(egui::SelectableLabel::new(selected, browser))
                                .on_hover_text(hover)
                                .clicked(),
                        };

                        if clicked && !selected {
                            self.route_status = None;
                            self.error = None;
                            self.pending_route = Some(idx);
                            ctx.request_repaint();
                        }
                    }
                    if !any_shown {
                        ui.weak("No supported browser detected");
                    }
                } else {
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
                }

                if ui
                    .button("🔄")
                    .on_hover_text("Detect apps with audio playing")
                    .clicked()
                {
                    self.log("detecting active audio sessions...".to_string());
                    // Re-detecting shouldn't drop the current selection out
                    // from under the user if that process is still there --
                    // only its index into the (freshly rebuilt) list changes.
                    let selected_pid = self
                        .selected_session
                        .and_then(|i| self.audio_sessions.get(i))
                        .map(|s| s.pid);

                    match list_audio_sessions() {
                        Ok(sessions) => {
                            self.error = None;
                            self.log(format!("found {} active audio session(s)", sessions.len()));
                            if sessions.is_empty() {
                                self.set_error(
                                    "No apps are currently playing audio. Start playback in \
                                     your browser tab, then refresh again."
                                        .to_string(),
                                );
                            }
                            self.audio_sessions = sessions;
                            self.selected_session = selected_pid
                                .and_then(|pid| self.audio_sessions.iter().position(|s| s.pid == pid));
                            if self.selected_session.is_none() {
                                self.route_status = None;
                            }
                        }
                        Err(e) => self.set_error(format!("Couldn't detect apps: {e}")),
                    }
                }
            });

            if self.pending_route.is_some() {
                ui.colored_label(egui::Color32::YELLOW, "Routing to CABLE Input...");
            } else if self.route_status.is_some() {
                ui.colored_label(egui::Color32::GREEN, "Isolating");
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
                    self.set_error(err);
                    self.engine = None;
                }
            }

            // Base users get no further feedback here -- everything below is
            // diagnostic and only appears once Developer Console is on.
            if self.dev_console {
                ui.separator();
                ui.label("Developer Console");
                egui::ScrollArea::vertical()
                    .max_height(120.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &self.dev_log {
                            ui.monospace(line);
                        }
                    });
            }
            });
        });

        if self.show_vb_cable_warning {
            egui::Window::new("⚠ VB-Audio Virtual Cable Not Found")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.set_max_width(320.0);
                    ui.label(
                        "This app couldn't find a VB-Audio Virtual Cable recording \
                         device, so it can't isolate audio yet.",
                    );
                    ui.add_space(8.0);
                    ui.label("To fix this:");
                    ui.horizontal(|ui| {
                        ui.label("1.");
                        ui.hyperlink_to("Download VB-Audio Virtual Cable", VB_CABLE_DOWNLOAD_URL);
                    });
                    ui.label("2. Reopen this app -- it will detect the cable automatically.");
                    ui.add_space(8.0);
                    if ui.button("OK").clicked() {
                        self.show_vb_cable_warning = false;
                    }
                });
        }

        if self.show_settings {
            let mut device_changed = false;
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
                                        device_changed = true;
                                    }
                                }
                            });

                        ui.add_space(8.0);
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

                        ui.add_space(8.0);
                        ui.separator();
                        if ui
                            .checkbox(&mut self.simple_browser_selection, "Simple Browser Selection")
                            .on_hover_text(
                                "Instead of a dropdown listing every process currently playing \
                                 audio, shows one icon per common browser (Chrome, Firefox, Edge, \
                                 Brave, Opera) that's currently playing audio. Click an icon to \
                                 isolate that browser -- same effect as picking it from the \
                                 dropdown, just fewer options to read through.",
                            )
                            .changed()
                        {
                            self.save_config();
                        }

                        ui.separator();
                        if ui.checkbox(&mut self.dev_console, "Developer Console").changed() {
                            self.save_config();
                        }
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

            if device_changed {
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
