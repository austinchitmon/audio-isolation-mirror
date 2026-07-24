# Changelog

All notable changes to this project are documented here. Format loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [0.1.0] - 2026-07-23

Initial working version. Isolates a livestream's stereo audio to Left, Right, or Both channels, with an option to mirror an isolated channel into both outputs instead of muting the other side.

### Added
- Real-time audio engine (`cpal`) that captures from a chosen input device (e.g. a virtual audio cable) and renders to a chosen output device.
- Automatic sample-rate conversion (`rubato`) between input and output devices when their negotiated rates differ — fixes crackling/pitch-shifted audio when, for example, a virtual cable runs at 48kHz but the output device runs at 44.1kHz.
- Channel gain-matrix DSP (`src/dsp.rs`) implementing Both / Left (± mirror) / Right (± mirror) modes, unit tested.
- Minimal always-on-top egui control panel: input/output device dropdowns, channel mode selector, mirror toggle.
- Config persistence (`%APPDATA%/audio-isolation-mirror/config.json`) — remembers last-used devices and mode across restarts.
- Graceful handling of mid-stream device errors (e.g. a device is unplugged): surfaced in the UI instead of crashing.

### Known limitations
- Windows only (relies on WASAPI via cpal's default host).
- Requires a virtual audio cable (e.g. VB-Audio Virtual Cable) to route a browser/app's output into the app; no native per-process loopback capture yet.
- No system tray mode, hotkeys, or level meters yet.
