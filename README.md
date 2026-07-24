# Audio Isolation Mirror

A small Windows desktop utility for isolating a livestream's stereo audio to the Left or Right channel — or mirroring one channel into both — instead of always hearing both mixed together.

Built with a multiview livestream in mind (e.g. Big Brother's 4-camera feed, where two camera groups are split across the Left/Right stereo channels), but works with any stereo source.

## What it does

- **Both** (default): normal stereo passthrough, unchanged.
- **Left**: only the Left channel plays; Right is muted.
- **Left + Mirror**: the Left channel plays out of both outputs (mono-from-left).
- **Right** / **Right + Mirror**: same, mirrored the other direction.

The app captures audio from an input device, applies the selected channel mode, and plays it out to a real output device (your headphones/speakers) in real time. It also automatically resamples if the input and output devices negotiate different sample rates, which is common and otherwise causes crackling/pitch-shifted audio.

## How audio gets in

Windows doesn't let one app directly grab another app's audio output, so this app expects to capture from a **virtual audio cable** instead:

```
Browser (livestream tab)
  -> routed via Windows per-app audio output -> "CABLE Input" (VB-Audio Virtual Cable)
  -> this app captures "CABLE Output" as its input device
  -> channel isolation / mirroring applied
  -> this app renders to your real headphones/speakers
```

## Prerequisites

- **Windows 10/11** (uses WASAPI via `cpal`; other platforms not supported yet — see [Known limitations](#known-limitations)).
- **[Rust](https://rustup.rs/)** (stable toolchain) with the MSVC linker — on Windows this means Visual Studio Build Tools with the "Desktop development with C++" workload. If you use `winget`:
  ```
  winget install --id Rustlang.Rustup -e
  winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
  ```
- **[VB-Audio Virtual Cable](https://vb-audio.com/Cable/)** (free) — a driver-only install, no Start Menu entry. Verify it installed via Settings → Sound: you should see **"CABLE Input"** under Output devices and **"CABLE Output"** under Input devices.

## Building

```
git clone https://github.com/austinchitmon/audio-isolation-mirror.git
cd audio-isolation-mirror
cargo build --release
```

The binary will be at `target/release/audio_isolation_mirror.exe`.

## Running

```
cargo run --release
```

This opens the control panel (an always-on-top window). Pick your devices:

1. **Input**: "CABLE Output (VB-Audio Virtual Cable)"
2. **Output**: your real headphones or speakers

Then select a channel mode (Both / Left / Right) and, if isolating to one side, whether to mirror it.

### Routing your livestream into the virtual cable

Settings → System → Sound → Volume mixer → find your browser (or whichever app is playing the stream) → set its **Output device** to "CABLE Input (VB-Audio Virtual Cable)".

Your device and mode selections are remembered between runs (saved to `%APPDATA%\audio-isolation-mirror\config.json`).

## Development

- `cargo test` runs the channel-mode DSP unit tests (`src/dsp.rs`).
- `src/audio.rs` owns the real-time audio engine: device I/O, resampling, and the lock-free handoff between the input and output audio threads.
- `src/main.rs` is the egui UI.
- `src/config.rs` handles loading/saving the last-used devices and mode.

See [CHANGELOG.md](CHANGELOG.md) for release notes.

## Known limitations

- Windows only for now. The audio engine (`cpal`) and UI (`egui`/`eframe`) both support macOS and Linux, so a port mainly means swapping the virtual-cable tool (e.g. [BlackHole](https://github.com/ExistentialAudio/BlackHole) on macOS, a PipeWire/PulseAudio null-sink on Linux) rather than rewriting the engine.
- No system tray mode, hotkeys, or level meters yet.
- No native per-process loopback capture — a virtual audio cable is required.
