# Changelog

All notable changes to this project are documented here. Format loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added
- One-click app audio routing: click **Detect apps with audio playing**, pick your browser (or whichever app is playing the stream), and click **Route to CABLE Input** to send its output into the virtual cable directly from the app — no more digging through Settings → Sound → Volume mixer. Uses the same (undocumented) per-app routing API Windows' own Volume Mixer uses; falls back to the manual Volume Mixer method if it's ever unavailable.

## [0.2.0] - 2026-07-24

### Added
- **Mute** checkbox: silences only this app's forwarded audio, without touching the output device's master volume/mute (so other apps on the same output aren't affected). Persists across restarts.

### Changed
- The release exe no longer opens a background console window alongside the GUI.
- The release exe statically links the MSVC C runtime, so it no longer depends on a separately-installed Visual C++ Redistributable.
- README rewritten with a plain-language "Quick Start (for viewers)" section aimed at non-technical users, separate from developer/build instructions.

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
