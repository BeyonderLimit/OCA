//! The recorder: record → pause → resume → stop, host-agnostic. The state
//! machine and buffer handling live here; device specifics live behind
//! [`crate::host::AudioHost`].

use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::host::{AudioHost, CaptureSink, CaptureStream};
use crate::wav::AudioSpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecState {
    Recording,
    Paused,
}

/// What a finished recording is: interleaved `f32` samples + the spec they
/// were captured at. Saving to disk happens separately ([`crate::TakeStore`]).
#[derive(Clone, Debug, PartialEq)]
pub struct RecordedAudio {
    pub samples: Vec<f32>,
    pub spec: AudioSpec,
}

impl RecordedAudio {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.spec.channels.max(1) as usize
    }

    pub fn duration_ms(&self) -> u64 {
        if self.spec.sample_rate == 0 {
            return 0;
        }
        self.frames() as u64 * 1_000 / self.spec.sample_rate as u64
    }
}

pub struct Recorder {
    stream: Box<dyn CaptureStream>,
    sink: CaptureSink,
    state: RecState,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("state", &self.state)
            .field("spec", &self.spec())
            .field("captured_samples", &self.captured_samples())
            .finish_non_exhaustive()
    }
}

impl Recorder {
    /// Open the default input device and start capturing.
    pub fn start(host: &dyn AudioHost) -> Result<Self> {
        let sink: CaptureSink = Arc::new(Mutex::new(Vec::new()));
        let stream = host.capture(Arc::clone(&sink))?;
        Ok(Self {
            stream,
            sink,
            state: RecState::Recording,
        })
    }

    pub fn state(&self) -> RecState {
        self.state
    }

    pub fn spec(&self) -> AudioSpec {
        self.stream.spec()
    }

    /// Samples captured so far (interleaved frames).
    pub fn captured_samples(&self) -> usize {
        self.sink.lock().unwrap().len()
    }

    pub fn pause(&mut self) -> Result<()> {
        if self.state != RecState::Recording {
            return Ok(());
        }
        self.stream.pause()?;
        self.state = RecState::Paused;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        if self.state != RecState::Paused {
            return Ok(());
        }
        self.stream.resume()?;
        self.state = RecState::Recording;
        Ok(())
    }

    /// Stop capturing and take the samples. Dropping the stream stops the
    /// device (no more samples are appended after this returns).
    pub fn stop(self) -> RecordedAudio {
        let spec = self.stream.spec();
        let Recorder { stream, sink, .. } = self;
        drop(stream);
        let samples = Arc::try_unwrap(sink)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_else(|arc| arc.lock().unwrap().clone());
        RecordedAudio { samples, spec }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockHost;

    #[test]
    fn pause_and_resume_split_the_take() {
        let host =
            MockHost::with_chunks(AudioSpec::mono(8_000), vec![vec![0.1; 100], vec![0.2; 100]]);
        let mut rec = Recorder::start(&host).unwrap();
        assert_eq!(rec.state(), RecState::Recording);
        assert_eq!(rec.captured_samples(), 100, "first chunk arrives on start");

        rec.pause().unwrap();
        assert_eq!(rec.state(), RecState::Paused);
        assert_eq!(rec.captured_samples(), 100, "paused captures add nothing");

        rec.resume().unwrap();
        assert_eq!(rec.captured_samples(), 200, "resume keeps capturing");

        let done = rec.stop();
        assert_eq!(done.samples.len(), 200);
        assert_eq!(done.spec, AudioSpec::mono(8_000));
        assert_eq!(done.duration_ms(), 25);
    }

    #[test]
    fn pause_without_resume_keeps_the_short_take() {
        let host =
            MockHost::with_chunks(AudioSpec::mono(8_000), vec![vec![0.1; 50], vec![0.2; 50]]);
        let mut rec = Recorder::start(&host).unwrap();
        rec.pause().unwrap();
        rec.pause().unwrap(); // idempotent
        let done = rec.stop();
        assert_eq!(done.samples.len(), 50);
    }

    #[test]
    fn deviceless_hosts_refuse_to_record() {
        let host = MockHost::without_devices();
        let err = Recorder::start(&host).unwrap_err();
        assert!(err.to_string().contains("no audio input device"));
    }
}
