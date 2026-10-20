//! Microphone capture through WASAPI (via cpal). Shared-mode capture only
//! delivers the device's own format, so this downmixes to mono and resamples
//! to the 16 kHz float32 stream the server's speech model expects.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, I24, Sample, SampleFormat, SizedSample};
use std::f64::consts::PI;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};

pub const SAMPLE_RATE: u32 = 16_000;
/// The server accepts at most 180 seconds per recording.
pub const MAX_FRAMES: u64 = 180 * SAMPLE_RATE as u64;

/// Why capture stopped on its own.
pub enum Stop {
    Limit,
    Backlog,
    Device(String),
}

pub type Notify = Arc<dyn Fn(Stop) + Send + Sync>;

/// The microphone's smoothed loudness, 0 to 1, for the recording pill: -55
/// to -5 dBFS, with a fast rise and a quick fall.
static LEVEL: AtomicU32 = AtomicU32::new(0);

pub fn level() -> f32 {
    f32::from_bits(LEVEL.load(Ordering::Relaxed))
}

pub fn input_names() -> Vec<String> {
    match cpal::default_host().input_devices() {
        Ok(devices) => devices.map(|device| device.to_string()).collect(),
        Err(error) => {
            log!("audio: cannot list microphones: {error}");
            Vec::new()
        }
    }
}

/// The name of Windows' default input, if there is one.
pub fn default_input_name() -> Option<String> {
    cpal::default_host()
        .default_input_device()
        .map(|device| device.to_string())
}

pub struct Recording {
    stream: cpal::Stream,
    stats: Arc<CaptureStats>,
    pub device: String,
}

#[derive(Default)]
struct CaptureStats {
    frames: AtomicU64,
    discontinuities: AtomicU64,
}

impl Recording {
    /// Stops the microphone and returns how many 16 kHz frames were handed on.
    pub fn stop(self) -> u64 {
        drop(self.stream);
        let discontinuities = self.stats.discontinuities.load(Ordering::Relaxed);
        if discontinuities > 0 {
            log!(
                "audio: recording continued through {discontinuities} capture discontinuity notification(s)"
            );
        }
        self.stats.frames.load(Ordering::SeqCst)
    }
}

/// Opens `preferred` (or the default input) and sends converted chunks to `chunks`.
pub fn start(
    preferred: Option<&str>,
    chunks: SyncSender<Vec<f32>>,
    notify: Notify,
) -> Result<Recording, String> {
    let host = cpal::default_host();
    let chosen = preferred.and_then(|name| {
        host.input_devices()
            .ok()?
            .find(|device| device.to_string() == name)
    });
    if let (Some(name), None) = (preferred, &chosen) {
        log!("audio: {name} is not connected; using the default microphone");
    }
    let device = chosen
        .or_else(|| host.default_input_device())
        .ok_or("No microphone is available. Connect one and try again.")?;
    let name = device.to_string();
    let supported = device
        .default_input_config()
        .map_err(|error| format!("{name} cannot record: {error}"))?;
    let format = supported.sample_format();
    let config = supported.config();
    log!(
        "audio: {name} at {} Hz, {} channel(s), {format:?}",
        config.sample_rate,
        config.channels
    );
    let stats = Arc::new(CaptureStats::default());
    LEVEL.store(0, Ordering::Relaxed);
    let sink = Sink {
        channels: config.channels.max(1) as usize,
        resampler: Resampler::new(config.sample_rate),
        rate: config.sample_rate as f32,
        level: 0.0,
        mono: Vec::new(),
        chunks,
        stats: stats.clone(),
        notify: notify.clone(),
        done: false,
    };
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, config, sink, notify),
        SampleFormat::I16 => build::<i16>(&device, config, sink, notify),
        SampleFormat::I24 => build::<I24>(&device, config, sink, notify),
        SampleFormat::I32 => build::<i32>(&device, config, sink, notify),
        SampleFormat::I64 => build::<i64>(&device, config, sink, notify),
        SampleFormat::F64 => build::<f64>(&device, config, sink, notify),
        SampleFormat::U16 => build::<u16>(&device, config, sink, notify),
        SampleFormat::U8 => build::<u8>(&device, config, sink, notify),
        other => {
            return Err(format!(
                "{name} uses an unsupported sample format ({other:?})."
            ));
        }
    }
    .map_err(|error| format!("Could not open {name}: {error}"))?;
    stream
        .play()
        .map_err(|error| format!("Could not start {name}: {error}"))?;
    Ok(Recording {
        stream,
        stats,
        device: name,
    })
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut sink: Sink,
    notify: Notify,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let stats = sink.stats.clone();
    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| sink.push(data),
        move |error| {
            // WASAPI reports a discontinuity as Xrun, then keeps delivering
            // packets. A missed audio deadline must not cancel the take.
            if error.kind() == cpal::ErrorKind::Xrun {
                stats.discontinuities.fetch_add(1, Ordering::Relaxed);
            } else {
                notify(Stop::Device(error.to_string()));
            }
        },
        None,
    )
}

/// Runs on the audio thread: never blocks, and fails the take rather than
/// queueing audio without bound when the upload falls behind.
struct Sink {
    channels: usize,
    resampler: Resampler,
    rate: f32,
    level: f32,
    mono: Vec<f32>,
    chunks: SyncSender<Vec<f32>>,
    stats: Arc<CaptureStats>,
    notify: Notify,
    done: bool,
}

impl Sink {
    fn push<T>(&mut self, data: &[T])
    where
        T: Sample,
        f32: FromSample<T>,
    {
        if self.done {
            return;
        }
        let scale = 1.0 / self.channels as f32;
        self.mono.clear();
        self.mono
            .extend(data.chunks_exact(self.channels).map(|frame| {
                frame
                    .iter()
                    .map(|sample| sample.to_sample::<f32>())
                    .sum::<f32>()
                    * scale
            }));
        if !self.mono.is_empty() {
            let power = self.mono.iter().map(|sample| sample * sample).sum::<f32>()
                / self.mono.len() as f32;
            let rms = power.sqrt();
            // -55 to -5 dBFS. The Mac's -68 to -18 pinned every bar at full
            // height with hot inputs such as speakerphones, so speech showed
            // as one solid band instead of following the syllables.
            let target = if rms > 0.0 {
                ((20.0 * rms.log10() + 55.0) / 50.0).clamp(0.0, 1.0)
            } else {
                0.0
            };
            // Timing comes from the samples, not from how often buffers arrive.
            // A quick release lets the gaps between syllables show.
            let seconds = self.mono.len() as f32 / self.rate;
            let constant = if target > self.level { 0.020 } else { 0.060 };
            self.level += (target - self.level) * -(-seconds / constant).exp_m1();
            if target == 0.0 && self.level < 0.02 {
                self.level = 0.0;
            }
            LEVEL.store(self.level.to_bits(), Ordering::Relaxed);
        }
        let mut output = Vec::with_capacity(self.mono.len());
        self.resampler.process(&self.mono, &mut output);
        let room = (MAX_FRAMES - self.stats.frames.load(Ordering::SeqCst)) as usize;
        let at_limit = output.len() >= room;
        output.truncate(room);
        let count = output.len() as u64;
        if count > 0 {
            match self.chunks.try_send(output) {
                Ok(()) => {
                    self.stats.frames.fetch_add(count, Ordering::SeqCst);
                }
                Err(TrySendError::Full(_)) => {
                    self.done = true;
                    (self.notify)(Stop::Backlog);
                    return;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.done = true;
                    return;
                }
            }
        }
        if at_limit {
            self.done = true;
            (self.notify)(Stop::Limit);
        }
    }
}

/// Windowed-sinc resampler. It low-passes below the output Nyquist rate, so
/// high frequencies do not alias into the 16 kHz stream.
struct Resampler {
    step: f64,
    position: f64,
    history: Vec<f32>,
    table: Vec<f32>,
    passthrough: bool,
}

const HALF_TAPS: usize = 24;
const PHASES: usize = 256;

impl Resampler {
    fn new(input_rate: u32) -> Resampler {
        let step = f64::from(input_rate) / f64::from(SAMPLE_RATE);
        let cutoff = (1.0 / step).min(1.0) * 0.92;
        // Kernel weights for each fractional position, indexed [phase][tap].
        let mut table = Vec::with_capacity(PHASES * 2 * HALF_TAPS);
        for phase in 0..PHASES {
            let fraction = phase as f64 / PHASES as f64;
            for tap in 0..2 * HALF_TAPS {
                let distance = fraction + (HALF_TAPS - 1) as f64 - tap as f64;
                table.push(kernel(distance, cutoff) as f32);
            }
        }
        Resampler {
            step,
            position: HALF_TAPS as f64,
            history: vec![0.0; HALF_TAPS],
            table,
            passthrough: input_rate == SAMPLE_RATE,
        }
    }

    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if self.passthrough {
            output.extend_from_slice(input);
            return;
        }
        self.history.extend_from_slice(input);
        // Each output sample needs HALF_TAPS input samples on both sides.
        while self.position + (HALF_TAPS as f64) < self.history.len() as f64 {
            let center = self.position.floor() as usize;
            let phase = ((self.position - center as f64) * PHASES as f64) as usize;
            let weights = &self.table[phase * 2 * HALF_TAPS..(phase + 1) * 2 * HALF_TAPS];
            let samples = &self.history[center + 1 - HALF_TAPS..=center + HALF_TAPS];
            output.push(
                samples
                    .iter()
                    .zip(weights)
                    .map(|(sample, weight)| sample * weight)
                    .sum(),
            );
            self.position += self.step;
        }
        let consumed = (self.position.floor() as usize).saturating_sub(HALF_TAPS);
        self.history.drain(..consumed);
        self.position -= consumed as f64;
    }
}

fn kernel(distance: f64, cutoff: f64) -> f64 {
    let x = distance * cutoff;
    let sinc = if x.abs() < 1e-9 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    };
    let edge = distance / HALF_TAPS as f64;
    let window = if edge.abs() >= 1.0 {
        0.0
    } else {
        0.5 * (1.0 + (PI * edge).cos())
    };
    cutoff * sinc * window
}
