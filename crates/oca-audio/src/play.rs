//! Playback: decode a take, map it to the output device's format, play.
//! The player holds the stream; dropping it stops playback.

use anyhow::Result;

use crate::host::{AudioHost, PlaybackStream};
use crate::wav::{DecodedAudio, map_frames};

pub struct Player {
    stream: Box<dyn PlaybackStream>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("done", &self.is_done())
            .finish_non_exhaustive()
    }
}

impl Player {
    /// Start playing `audio` through the host's default output device.
    /// Returns as soon as playback has started; poll [`Self::is_done`] or
    /// block with [`Self::wait_done`].
    pub fn start(host: &dyn AudioHost, audio: DecodedAudio) -> Result<Self> {
        let mut stream = host.playback()?;
        let mapped = map_frames(&audio.samples, audio.spec, stream.spec());
        stream.start(mapped)?;
        Ok(Self { stream })
    }

    pub fn is_done(&self) -> bool {
        self.stream.is_done()
    }

    pub fn wait_done(&mut self) -> Result<()> {
        self.stream.wait_done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockHost;
    use crate::wav::AudioSpec;

    #[test]
    fn play_sends_the_file_frames_to_the_device() {
        let host = MockHost::with_chunks(AudioSpec::mono(8_000), vec![]);
        let audio = DecodedAudio {
            samples: vec![0.25, -0.25, 0.5],
            spec: AudioSpec::mono(8_000),
        };
        let player = Player::start(&host, audio.clone()).unwrap();
        assert!(player.is_done());
        let played = host.played();
        assert_eq!(played.len(), 1);
        assert_eq!(played[0].0, audio.spec);
        assert_eq!(played[0].1, audio.samples);
    }

    #[test]
    fn deviceless_hosts_refuse_playback() {
        let host = MockHost::without_devices();
        let audio = DecodedAudio {
            samples: vec![0.0],
            spec: AudioSpec::mono(8_000),
        };
        let err = Player::start(&host, audio).unwrap_err();
        assert!(err.to_string().contains("no audio output device"));
    }
}
