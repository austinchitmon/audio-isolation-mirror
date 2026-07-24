# Audio Isolation Mirror

A small Windows desktop utility for isolating a livestream's stereo audio to the Left or Right channel — or mirroring one channel into both — instead of always hearing both mixed together.

Built with a multiview livestream in mind (e.g. Big Brother's 4-camera feed, where two camera groups are split across the Left/Right stereo channels), but works with any stereo source.

## What it does

- **Both** (default): normal stereo passthrough, unchanged.
- **Left**: only the Left channel plays; Right is muted.
- **Left + Mirror**: the Left channel plays out of both outputs (mono-from-left).
- **Right** / **Right + Mirror**: same, mirrored the other direction.
- **Mute**: silences the audio this app is forwarding, without touching your PC's overall volume or muting anything else (Discord, other browser tabs, etc.).

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

## Quick Start (for viewers)

You only need to do this once.

1. **Install VB-Audio Virtual Cable** (free): [vb-audio.com/Cable](https://vb-audio.com/Cable/). It's a driver-only install — there's no Start Menu entry, and that's normal. To confirm it worked, open Settings → Sound and look for **"CABLE Input"** under Output devices and **"CABLE Output"** under Input devices.
2. **Download the app**: go to the [Releases](https://github.com/austinchitmon/audio-isolation-mirror/releases) page and download `audio_isolation_mirror.exe` (the latest version, at the top).
3. **Run it**: double-click the file you downloaded. Windows will likely show a blue **"Windows protected your PC"** screen — this is expected for any small app that isn't from a big paid publisher, and does not mean anything is wrong. Click **More info**, then click **Run anyway**.
4. **Pick your devices** in the app window that opens:
   - **Input**: "CABLE Output (VB-Audio Virtual Cable)"
   - **Output**: your real headphones or speakers
5. **Send your livestream into the cable**: Settings → System → Sound → Volume mixer → find your browser (or whichever app is playing the stream) → set its **Output device** to "CABLE Input (VB-Audio Virtual Cable)".
6. **Use the controls** as needed: pick a channel mode (Both / Left / Right, with Mirror if isolating to one side), or check **Mute** to silence just this app's audio without touching anything else on your PC.

That's it — the app remembers your device and mode choices, so future launches just work. Keep the app running in the background while you watch.

## For developers

### Prerequisites

- **Windows 10/11** (uses WASAPI via `cpal`; other platforms not supported yet — see [Known limitations](#known-limitations)).
- **[Rust](https://rustup.rs/)** (stable toolchain) with the MSVC linker — on Windows this means Visual Studio Build Tools with the "Desktop development with C++" workload. If you use `winget`:
  ```
  winget install --id Rustlang.Rustup -e
  winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
  ```
- **[VB-Audio Virtual Cable](https://vb-audio.com/Cable/)** (free) — see step 1 above.

### Building

```
git clone https://github.com/austinchitmon/audio-isolation-mirror.git
cd audio-isolation-mirror
cargo build --release
```

The binary will be at `target/release/audio_isolation_mirror.exe`. It statically links the MSVC C runtime (`.cargo/config.toml`), so it runs standalone on any Windows 10/11 machine with no separate Visual C++ Redistributable install required.

### Running

```
cargo run --release
```

This opens the control panel (an always-on-top window). See the [Quick Start](#quick-start-for-viewers) above for device setup and routing.

- `cargo test` runs the channel-mode DSP unit tests (`src/dsp.rs`).
- `src/audio.rs` owns the real-time audio engine: device I/O, resampling, and the lock-free handoff between the input and output audio threads.
- `src/main.rs` is the egui UI.
- `src/config.rs` handles loading/saving the last-used devices and mode.

Your device and mode selections are saved to `%APPDATA%\audio-isolation-mirror\config.json`.

See [CHANGELOG.md](CHANGELOG.md) for release notes.

### Cutting a release

Pushing a version tag triggers `.github/workflows/release.yml`, which builds the Windows binary and publishes it to the Releases page automatically:

```
git tag v0.1.0
git push origin v0.1.0
```

## Known limitations

- Windows only for now. The audio engine (`cpal`) and UI (`egui`/`eframe`) both support macOS and Linux, so a port mainly means swapping the virtual-cable tool (e.g. [BlackHole](https://github.com/ExistentialAudio/BlackHole) on macOS, a PipeWire/PulseAudio null-sink on Linux) rather than rewriting the engine.
- No system tray mode, hotkeys, or level meters yet.
- No native per-process loopback capture — a virtual audio cable is required.
- The released `.exe` isn't code-signed, so the SmartScreen warning in the Quick Start above will appear on first run. This is a one-time click-through, not a sign of a problem.
