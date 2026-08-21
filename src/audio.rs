use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, StreamConfig};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

use crate::dsp::ChannelMode;

/// Converts interleaved input audio from the input device's sample rate to the
/// output device's sample rate. Two independently-clocked devices very commonly
/// negotiate different rates (e.g. a 48kHz virtual cable vs 44.1kHz headphones);
/// forwarding raw samples 1:1 in that case causes audible crackling and pitch drift.
struct InputPipeline {
    resampler: Option<SincFixedIn<f32>>,
    chunk_size: usize,
    channels: usize,
    pending: Vec<f32>,
    deinterleaved_in: Vec<Vec<f32>>,
    deinterleaved_out: Vec<Vec<f32>>,
    interleaved_out: Vec<f32>,
}

impl InputPipeline {
    fn new(input_rate: u32, output_rate: u32, channels: usize) -> Result<Self> {
        const CHUNK_SIZE: usize = 1024;

        let resampler = if input_rate != output_rate {
            let params = SincInterpolationParameters {
                sinc_len: 256,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 256,
                window: WindowFunction::BlackmanHarris2,
            };
            let ratio = output_rate as f64 / input_rate as f64;
            let r = SincFixedIn::<f32>::new(ratio, 2.0, params, CHUNK_SIZE, channels)
                .map_err(|e| anyhow!("failed to construct resampler: {e:?}"))?;
            Some(r)
        } else {
            None
        };

        let max_out = resampler
            .as_ref()
            .map(|r| r.output_frames_max())
            .unwrap_or(CHUNK_SIZE);

        Ok(Self {
            resampler,
            chunk_size: CHUNK_SIZE,
            channels,
            pending: Vec::with_capacity(CHUNK_SIZE * channels * 2),
            deinterleaved_in: vec![vec![0.0; CHUNK_SIZE]; channels],
            deinterleaved_out: vec![vec![0.0; max_out]; channels],
            interleaved_out: Vec::with_capacity(max_out * channels),
        })
    }

    /// Feeds raw interleaved input samples, calling `emit` with resampled (or,
    /// if rates already match, passed-through) interleaved samples. May call
    /// `emit` zero or more times depending on how much has accumulated.
    fn process(&mut self, raw: &[f32], mut emit: impl FnMut(&[f32])) {
        let Some(resampler) = self.resampler.as_mut() else {
            emit(raw);
            return;
        };

        self.pending.extend_from_slice(raw);
        let chunk_len = self.chunk_size * self.channels;

        while self.pending.len() >= chunk_len {
            for (ch, dst) in self.deinterleaved_in.iter_mut().enumerate() {
                for i in 0..self.chunk_size {
                    dst[i] = self.pending[i * self.channels + ch];
                }
            }
            self.pending.drain(0..chunk_len);

            let (_, out_frames) = match resampler.process_into_buffer(
                &self.deinterleaved_in,
                &mut self.deinterleaved_out,
                None,
            ) {
                Ok(result) => result,
                Err(err) => {
                    eprintln!("resampler error: {err}");
                    continue;
                }
            };

            self.interleaved_out.clear();
            self.interleaved_out.resize(out_frames * self.channels, 0.0);
            for (ch, src) in self.deinterleaved_out.iter().enumerate() {
                for i in 0..out_frames {
                    self.interleaved_out[i * self.channels + ch] = src[i];
                }
            }
            emit(&self.interleaved_out);
        }
    }
}

/// Shared, lock-free handle the UI thread uses to change the active channel mode
/// without ever touching the real-time audio callback with a mutex.
#[derive(Clone)]
pub struct ModeHandle(Arc<AtomicU8>);

impl ModeHandle {
    pub fn new(initial: ChannelMode) -> Self {
        Self(Arc::new(AtomicU8::new(initial.to_code())))
    }

    pub fn set(&self, mode: ChannelMode) {
        self.0.store(mode.to_code(), Ordering::Relaxed);
    }

    pub fn get(&self) -> ChannelMode {
        ChannelMode::from_code(self.0.load(Ordering::Relaxed))
    }
}

/// Shared, lock-free handle the UI thread uses to mute this app's forwarded
/// audio without touching the OS output device's master mute.
#[derive(Clone)]
pub struct MuteHandle(Arc<AtomicBool>);

impl MuteHandle {
    pub fn new(initial: bool) -> Self {
        Self(Arc::new(AtomicBool::new(initial)))
    }

    pub fn set(&self, muted: bool) {
        self.0.store(muted, Ordering::Relaxed);
    }

    pub fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

pub fn list_input_devices() -> Vec<Device> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|d| d.collect())
        .unwrap_or_default()
}

pub fn list_output_devices() -> Vec<Device> {
    let host = cpal::default_host();
    host.output_devices()
        .map(|d| d.collect())
        .unwrap_or_default()
}

/// The name of the OS's current default playback device (e.g. whatever the
/// user picked in Windows' Sound settings), so we can assume that's what
/// they want to actually listen on without asking.
pub fn default_output_device_name() -> Option<String> {
    cpal::default_host().default_output_device()?.name().ok()
}

/// Owns the live input+output cpal streams. Dropping this stops audio.
pub struct AudioEngine {
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
    error: Arc<Mutex<Option<String>>>,
}

impl AudioEngine {
    /// Returns and clears any stream error that occurred since the last check
    /// (e.g. the input/output device was unplugged mid-stream).
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().unwrap().take()
    }

    pub fn start(input: &Device, output: &Device, mode: ModeHandle, mute: MuteHandle) -> Result<Self> {
        let input_config: StreamConfig = input
            .default_input_config()
            .context("no default input config")?
            .into();
        let output_config: StreamConfig = output
            .default_output_config()
            .context("no default output config")?
            .into();

        eprintln!(
            "input config: {:?} channels @ {:?} Hz, output config: {:?} channels @ {:?} Hz",
            input_config.channels, input_config.sample_rate, output_config.channels, output_config.sample_rate
        );

        let channels = input_config.channels as usize;
        if channels < 2 || output_config.channels as usize != channels {
            return Err(anyhow!(
                "expected matching stereo input/output devices, got {} in / {} out",
                channels,
                output_config.channels
            ));
        }

        let mut pipeline =
            InputPipeline::new(input_config.sample_rate.0, output_config.sample_rate.0, channels)
                .context("failed to set up resampler")?;

        // ~200ms of headroom at the output's sample rate (what's actually stored in the
        // buffer post-resample), enough to absorb scheduling jitter between the
        // independently-clocked input and output audio threads.
        let capacity = output_config.sample_rate.0 as usize * channels / 5;
        let rb = HeapRb::<f32>::new(capacity.max(4096));
        let (mut producer, mut consumer) = rb.split();

        let error = Arc::new(Mutex::new(None));

        let sample_format = input.default_input_config()?.sample_format();
        let input_stream = build_input_stream(
            input,
            &input_config,
            sample_format,
            move |samples| {
                pipeline.process(samples, |resampled| {
                    let _ = producer.push_slice(resampled);
                });
            },
            error.clone(),
        )?;

        let output_stream = build_output_stream(
            output,
            &output_config,
            output.default_output_config()?.sample_format(),
            channels,
            mode,
            mute,
            move |out: &mut [f32]| {
                let filled = consumer.pop_slice(out);
                for sample in &mut out[filled..] {
                    *sample = 0.0;
                }
            },
            error.clone(),
        )?;

        input_stream.play().context("failed to start input stream")?;
        output_stream.play().context("failed to start output stream")?;

        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            error,
        })
    }
}

fn build_input_stream(
    device: &Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    mut on_samples: impl FnMut(&[f32]) + Send + 'static,
    error: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream> {
    let err_fn = move |err| {
        eprintln!("input stream error: {err}");
        *error.lock().unwrap() = Some(format!("input device error: {err}"));
    };

    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _| on_samples(data),
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _| {
                let converted: Vec<f32> = data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                on_samples(&converted);
            },
            err_fn,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _| {
                let converted: Vec<f32> = data
                    .iter()
                    .map(|s| (*s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                    .collect();
                on_samples(&converted);
            },
            err_fn,
            None,
        )?,
        other => return Err(anyhow!("unsupported input sample format: {other:?}")),
    };

    Ok(stream)
}

fn build_output_stream(
    device: &Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    channels: usize,
    mode: ModeHandle,
    mute: MuteHandle,
    mut fill: impl FnMut(&mut [f32]) + Send + 'static,
    error: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream> {
    let err_fn = move |err| {
        eprintln!("output stream error: {err}");
        *error.lock().unwrap() = Some(format!("output device error: {err}"));
    };

    let stream = match sample_format {
        SampleFormat::F32 => device.build_output_stream(
            config,
            move |data: &mut [f32], _| {
                fill(data);
                apply_mode_in_place(data, channels, mode.get());
                if mute.get() {
                    data.fill(0.0);
                }
            },
            err_fn,
            None,
        )?,
        other => return Err(anyhow!("unsupported output sample format: {other:?}, use F32 output device")),
    };

    Ok(stream)
}

/// Applies the channel gain matrix to an interleaved stereo buffer in place.
/// Only the first two channels are treated as left/right; any extra channels pass through.
fn apply_mode_in_place(buf: &mut [f32], channels: usize, mode: ChannelMode) {
    for frame in buf.chunks_exact_mut(channels) {
        let (l, r) = mode.apply(frame[0], frame[1]);
        frame[0] = l;
        frame[1] = r;
    }
}
