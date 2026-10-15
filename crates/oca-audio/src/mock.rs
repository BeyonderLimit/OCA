//! Deterministic device stand-ins for tests and headless runs.
//!
//! `MockHost` implements the same [`AudioHost`](crate::host::AudioHost)
//! traits as the real cpal host: capture "delivers" preset chunks (the
//! first on start, one more per resume), and playback records what it was
//! asked to play. No threads, no hardware, no timing.

use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};

use crate::host::{AudioHost, CaptureSink, CaptureStream, PlaybackStream};
use crate::wav::AudioSpec;

/// Frames the mock host has played: one entry per `playback()` call.
type PlayedLog = Arc<Mutex<Vec<(AudioSpec, Vec<f32>)>>>;

pub struct MockHost {
    spec: AudioSpec,
    /// Chunk template; each capture gets its own copy.
    chunks: Vec<Vec<f32>>,
    devices: bool,
    played: PlayedLog,
}

impl MockHost {
    /// One capture chunk of 80 frames at 8 kHz mono.
    pub fn new() -> Self {
        Self::with_chunks(AudioSpec::mono(8_000), vec![vec![0.5; 80]])
    }

    pub fn with_chunks(spec: AudioSpec, chunks: Vec<Vec<f32>>) -> Self {
        Self {
            spec,
            chunks,
            devices: true,
            played: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A machine with no sound hardware (CI boxes, containers).
    pub fn without_devices() -> Self {
        let mut host = Self::new();
        host.devices = false;
        host
    }

    /// What playback was asked to play, in order.
    pub fn played(&self) -> Vec<(AudioSpec, Vec<f32>)> {
        self.played.lock().unwrap().clone()
    }
}

impl Default for MockHost {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioHost for MockHost {
    fn default_input(&self) -> Result<AudioSpec> {
        if self.devices {
            Ok(self.spec)
        } else {
            Err(anyhow!("no audio input device found (mock)"))
        }
    }

    fn default_output(&self) -> Result<AudioSpec> {
        if self.devices {
            Ok(self.spec)
        } else {
            Err(anyhow!("no audio output device found (mock)"))
        }
    }

    fn capture(&self, sink: CaptureSink) -> Result<Box<dyn CaptureStream>> {
        if !self.devices {
            return Err(anyhow!("no audio input device found (mock)"));
        }
        let mut pending = self.chunks.clone();
        if let Some(first) = pending.first() {
            sink.lock().unwrap().extend_from_slice(first);
            pending.remove(0);
        }
        Ok(Box::new(MockCapture {
            sink,
            inner: Mutex::new(MockCaptureInner {
                pending,
                paused: false,
            }),
            spec: self.spec,
        }))
    }

    fn playback(&self) -> Result<Box<dyn PlaybackStream>> {
        if !self.devices {
            return Err(anyhow!("no audio output device found (mock)"));
        }
        Ok(Box::new(MockPlayback {
            spec: self.spec,
            played: Arc::clone(&self.played),
            done: true,
        }))
    }
}

struct MockCapture {
    sink: CaptureSink,
    inner: Mutex<MockCaptureInner>,
    spec: AudioSpec,
}

struct MockCaptureInner {
    pending: Vec<Vec<f32>>,
    paused: bool,
}

impl CaptureStream for MockCapture {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn pause(&self) -> Result<()> {
        self.inner.lock().unwrap().paused = true;
        Ok(())
    }

    fn resume(&self) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.paused = false;
        // One more chunk per resume models samples arriving over time
        // (the real host delivers them continuously from the device).
        if !inner.pending.is_empty() {
            let chunk = inner.pending.remove(0);
            self.sink.lock().unwrap().extend_from_slice(&chunk);
        }
        Ok(())
    }
}

struct MockPlayback {
    spec: AudioSpec,
    played: PlayedLog,
    done: bool,
}

impl PlaybackStream for MockPlayback {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn start(&mut self, frames: Vec<f32>) -> Result<()> {
        self.played.lock().unwrap().push((self.spec, frames));
        self.done = true;
        Ok(())
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn wait_done(&mut self) -> Result<()> {
        Ok(())
    }
}
