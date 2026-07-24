mod audio;
mod config;
mod dsp;

use cpal::traits::DeviceTrait;
use eframe::egui;

use audio::{list_input_devices, list_output_devices, AudioEngine, ModeHandle, MuteHandle};
use dsp::ChannelMode;

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
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Big Brother Channel Isolator");
            ui.separator();

            let mut device_changed = false;

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

        // Stream errors arrive on a background audio thread with no repaint of
        // their own; poll periodically so a mid-stream disconnect shows up promptly.
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
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
        "Big Brother Channel Isolator",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}
