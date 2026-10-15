//! Device abstraction: everything above this module is testable without
//! hardware. `CpalHost` is the real (cross-platform) implementation;
//! `mock::MockHost` implements the same traits deterministically for tests
//! and headless runs. Each cpal stream lives on its own thread and is
//! driven over a channel — the cross-platform risk table calls for
//! abstracting device selection early, and this keeps platform quirks
//! inside this module.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

use crate::wav::AudioSpec;

/// A capture sink: interleaved `f32` frames are appended here from the
/// device callback while recording.
pub type CaptureSink = Arc<Mutex<Vec<f32>>>;

pub trait AudioHost: Send + Sync {
    /// The default input device's format — or why recording is unavailable
    /// here (for `oca doctor`).
    fn default_input(&self) -> Result<AudioSpec>;
    /// The default output device's format — or why playback is unavailable
    /// here (for `oca doctor`).
    fn default_output(&self) -> Result<AudioSpec>;
    /// Start capturing from the default input device into `sink`.
    fn capture(&self, sink: CaptureSink) -> Result<Box<dyn CaptureStream>>;
    /// Open the default output device for playback.
    fn playback(&self) -> Result<Box<dyn PlaybackStream>>;
}

pub trait CaptureStream: Send {
    /// The format samples arrive in.
    fn spec(&self) -> AudioSpec;
    fn pause(&self) -> Result<()>;
    fn resume(&self) -> Result<()>;
}

pub trait PlaybackStream: Send {
    /// The format this stream expects frames in.
    fn spec(&self) -> AudioSpec;
    /// Start playing already-mapped interleaved frames; returns immediately.
    fn start(&mut self, frames: Vec<f32>) -> Result<()>;
    fn is_done(&self) -> bool;
    /// Block until the frames have been played (CLI path).
    fn wait_done(&mut self) -> Result<()>;
}

// ---- real host -------------------------------------------------------------

/// cpal-backed host: default input/output devices, no device selection UI
/// in v1 (device pickers arrive with user demand).
#[derive(Default)]
pub struct CpalHost;

impl AudioHost for CpalHost {
    fn default_input(&self) -> Result<AudioSpec> {
        ensure_sound_present()?;
        let device = cpal::default_host()
            .default_input_device()
            .context("no audio input device found — recording is unavailable here")?;
        let config = device
            .default_input_config()
            .context("input device has no default format")?;
        Ok(AudioSpec {
            sample_rate: config.sample_rate().0,
            channels: config.channels(),
        })
    }

    fn default_output(&self) -> Result<AudioSpec> {
        ensure_sound_present()?;
        let device = cpal::default_host()
            .default_output_device()
            .context("no audio output device found — playback is unavailable here")?;
        let config = device
            .default_output_config()
            .context("output device has no default format")?;
        Ok(AudioSpec {
            sample_rate: config.sample_rate().0,
            channels: config.channels(),
        })
    }

    fn capture(&self, sink: CaptureSink) -> Result<Box<dyn CaptureStream>> {
        ensure_sound_present()?;
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("no audio input device found — recording is unavailable here")?;
        let supported = device
            .default_input_config()
            .context("input device has no default format")?;
        let spec = AudioSpec {
            sample_rate: supported.sample_rate().0,
            channels: supported.channels(),
        };
        let config = supported.config();
        let format = supported.sample_format();

        let (ctrl_tx, ctrl_rx) = mpsc::channel::<CaptureCtrl>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let thread = std::thread::Builder::new()
            .name("oca-capture".into())
            .spawn(move || {
                let stream = match build_input(&device, &config, format, sink) {
                    Ok(stream) => stream,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                loop {
                    match ctrl_rx.recv() {
                        Ok(CaptureCtrl::Pause) => {
                            let _ = stream.pause();
                        }
                        Ok(CaptureCtrl::Resume) => {
                            let _ = stream.play();
                        }
                        Ok(CaptureCtrl::Stop) | Err(_) => break,
                    }
                }
                // Dropping the stream stops capture.
            })
            .context("cannot spawn capture thread")?;
        ready_rx.recv().context("capture thread stopped early")??;
        Ok(Box::new(CpalCapture {
            ctrl: ctrl_tx,
            spec,
            thread: Some(thread),
        }))
    }

    fn playback(&self) -> Result<Box<dyn PlaybackStream>> {
        ensure_sound_present()?;
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("no audio output device found — playback is unavailable here")?;
        let supported = device
            .default_output_config()
            .context("output device has no default format")?;
        let spec = AudioSpec {
            sample_rate: supported.sample_rate().0,
            channels: supported.channels(),
        };
        let config = supported.config();
        let format = supported.sample_format();

        let shared = Arc::new(Mutex::new(PlayFrames {
            frames: Vec::new(),
            pos: 0,
            done: true,
        }));
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<Vec<f32>>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let thread = std::thread::Builder::new()
            .name("oca-playback".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || {
                    let stream = match build_output(&device, &config, format, Arc::clone(&shared)) {
                        Ok(stream) => stream,
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    };
                    let _ = ready_tx.send(Ok(()));
                    while let Ok(frames) = ctrl_rx.recv() {
                        {
                            let mut state = shared.lock().unwrap();
                            state.frames = frames;
                            state.pos = 0;
                            state.done = false;
                        }
                        let _ = stream.play();
                    }
                    // Dropping the stream stops output.
                }
            })
            .context("cannot spawn playback thread")?;
        ready_rx.recv().context("playback thread stopped early")??;
        Ok(Box::new(CpalPlayback {
            ctrl: Some(ctrl_tx),
            shared,
            spec,
            thread: Some(thread),
        }))
    }
}

/// Refuse to touch ALSA when the machine has no sound subsystem.
///
/// Without this, every probe on a device-less box (CI runners, containers)
/// makes libasound print `ALSA lib …` lines straight to stderr — breaking
/// `oca doctor`'s quiet-stderr contract. Hardware PCM nodes live under
/// `/dev/snd`; if the directory is missing or has no PCM/control nodes,
/// every open would fail anyway, so returning early loses nothing.
/// (Hint *enumeration* is avoided entirely for the same reason.)
fn ensure_sound_present() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let present = std::path::Path::new("/dev/snd")
            .read_dir()
            .map(|entries| {
                entries.filter_map(Result::ok).any(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with("pcm") || name.starts_with("control")
                })
            })
            .unwrap_or(false);
        if !present {
            anyhow::bail!(
                "no audio devices on this machine — recording and playback are unavailable"
            );
        }
    }
    #[cfg(not(target_os = "linux"))]
    {} // coreaudio/wasapi fail cleanly without printing
    Ok(())
}

enum CaptureCtrl {
    Pause,
    Resume,
    Stop,
}

struct CpalCapture {
    ctrl: Sender<CaptureCtrl>,
    spec: AudioSpec,
    thread: Option<JoinHandle<()>>,
}

impl CaptureStream for CpalCapture {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn pause(&self) -> Result<()> {
        self.ctrl
            .send(CaptureCtrl::Pause)
            .map_err(|_| anyhow!("capture thread is gone"))
    }

    fn resume(&self) -> Result<()> {
        self.ctrl
            .send(CaptureCtrl::Resume)
            .map_err(|_| anyhow!("capture thread is gone"))
    }
}

impl Drop for CpalCapture {
    fn drop(&mut self) {
        let _ = self.ctrl.send(CaptureCtrl::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct PlayFrames {
    frames: Vec<f32>,
    pos: usize,
    done: bool,
}

struct CpalPlayback {
    /// Taken on drop so the channel closes and the thread can finish.
    ctrl: Option<Sender<Vec<f32>>>,
    shared: Arc<Mutex<PlayFrames>>,
    spec: AudioSpec,
    thread: Option<JoinHandle<()>>,
}

impl PlaybackStream for CpalPlayback {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn start(&mut self, frames: Vec<f32>) -> Result<()> {
        self.ctrl
            .as_ref()
            .ok_or_else(|| anyhow!("playback already stopped"))?
            .send(frames)
            .map_err(|_| anyhow!("playback thread is gone"))
    }

    fn is_done(&self) -> bool {
        self.shared.lock().unwrap().done
    }

    fn wait_done(&mut self) -> Result<()> {
        while !self.is_done() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Ok(())
    }
}

impl Drop for CpalPlayback {
    fn drop(&mut self) {
        // Dropping the sender closes the channel, ending the thread loop.
        self.ctrl = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// ---- cpal stream construction ---------------------------------------------

fn build_input(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    sink: CaptureSink,
) -> Result<cpal::Stream> {
    match format {
        SampleFormat::F32 => build_input_t::<f32>(device, config, sink),
        SampleFormat::I16 => build_input_t::<i16>(device, config, sink),
        SampleFormat::U16 => build_input_t::<u16>(device, config, sink),
        other => Err(anyhow!("unsupported input sample format: {other}")),
    }
}

fn build_input_t<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    sink: CaptureSink,
) -> Result<cpal::Stream>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let err_fn = |e| tracing::warn!("capture stream error: {e}");
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let mut buf = sink.lock().unwrap();
                buf.extend(data.iter().map(|s| f32::from_sample_(*s)));
            },
            err_fn,
            None,
        )
        .context("cannot open input stream")
}

fn build_output(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    shared: Arc<Mutex<PlayFrames>>,
) -> Result<cpal::Stream> {
    match format {
        SampleFormat::F32 => build_output_t::<f32>(device, config, shared),
        SampleFormat::I16 => build_output_t::<i16>(device, config, shared),
        SampleFormat::U16 => build_output_t::<u16>(device, config, shared),
        other => Err(anyhow!("unsupported output sample format: {other}")),
    }
}

fn build_output_t<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    shared: Arc<Mutex<PlayFrames>>,
) -> Result<cpal::Stream>
where
    T: SizedSample + Send + 'static,
    T: FromSample<f32>,
{
    let channels = config.channels as usize;
    let err_fn = |e| tracing::warn!("playback stream error: {e}");
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _| {
                let mut state = shared.lock().unwrap();
                for frame in out.chunks_mut(channels.max(1)) {
                    if state.pos < state.frames.len() {
                        for (i, sample) in frame.iter_mut().enumerate() {
                            let v = state.frames.get(state.pos + i).copied().unwrap_or(0.0);
                            *sample = T::from_sample_(v);
                        }
                        state.pos += channels;
                    } else {
                        if !state.done {
                            state.done = true;
                        }
                        for sample in frame.iter_mut() {
                            *sample = T::from_sample_(0.0);
                        }
                    }
                }
            },
            err_fn,
            None,
        )
        .context("cannot open output stream")
}
