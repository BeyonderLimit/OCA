//! WAV encode/decode (16-bit PCM on disk, `f32` in memory) and the
//! channel/rate mapping used to play a take through whatever format the
//! output device speaks.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The shape of a sample stream: interleaved, one rate for both directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSpec {
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioSpec {
    pub fn mono(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            channels: 1,
        }
    }
}

/// Decoded samples (`f32`, interleaved) plus the spec they were captured at.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedAudio {
    pub samples: Vec<f32>,
    pub spec: AudioSpec,
}

impl DecodedAudio {
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

/// Encode interleaved `f32` samples as 16-bit PCM WAV bytes.
pub fn encode_wav(samples: &[f32], spec: AudioSpec) -> Result<Vec<u8>> {
    let spec_wav = hound::WavSpec {
        channels: spec.channels,
        sample_rate: spec.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer =
            hound::WavWriter::new(&mut cursor, spec_wav).context("cannot create WAV writer")?;
        for s in samples {
            // f32 in [-1.0, 1.0] → i16 full scale.
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            writer.write_sample(v).context("cannot write WAV sample")?;
        }
        writer.finalize().context("cannot finish WAV file")?;
    }
    Ok(cursor.into_inner())
}

/// Decode WAV bytes of any bit depth (≤ 32) into `f32` samples.
pub fn decode_wav(bytes: &[u8]) -> Result<DecodedAudio> {
    let cursor = std::io::Cursor::new(bytes);
    let mut reader = hound::WavReader::new(cursor).context("not a readable WAV file")?;
    let hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample,
        sample_format,
    } = reader.spec();
    if channels == 0 {
        anyhow::bail!("WAV file declares zero channels");
    }
    let samples: Vec<f32> = match sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| {
                s.map(|v| v as f32 / i16::MAX as f32)
                    .context("cannot read WAV sample")
            })
            .collect::<Result<_>>()?,
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.context("cannot read WAV sample"))
            .collect::<Result<_>>()?,
    };
    let _ = bits_per_sample; // hound converts to the requested sample type
    Ok(DecodedAudio {
        samples,
        spec: AudioSpec {
            sample_rate,
            channels,
        },
    })
}

/// Map interleaved samples from `src` to `dst` (channel count + rate).
///
/// This is how playback stays device-agnostic: takes are stored at whatever
/// the input device captured, and the player feeds the output device in
/// *its* native format. Mono↔multi is duplicated/averaged; anything else
/// routes through mono. Rates use linear interpolation — cheap, dependency
/// free, and good enough for voice notes and demos.
pub fn map_frames(samples: &[f32], src: AudioSpec, dst: AudioSpec) -> Vec<f32> {
    if src.channels == 0 || dst.channels == 0 || src.sample_rate == 0 || dst.sample_rate == 0 {
        return samples.to_vec();
    }
    let channel_mapped: Vec<f32> = if src.channels == dst.channels {
        samples.to_vec()
    } else {
        let src_frames = samples.len() / src.channels as usize;
        let mut out = Vec::with_capacity(src_frames * dst.channels as usize);
        for frame in samples.chunks(src.channels as usize) {
            let mono: f32 = frame.iter().sum::<f32>() / frame.len().max(1) as f32;
            for _ in 0..dst.channels {
                out.push(mono);
            }
        }
        out
    };
    if src.sample_rate == dst.sample_rate {
        return channel_mapped;
    }
    let channels = dst.channels as usize;
    let src_frames = channel_mapped.len() / channels.max(1);
    if src_frames == 0 {
        return Vec::new();
    }
    let dst_frames =
        (src_frames as u64 * dst.sample_rate as u64 / src.sample_rate as u64).max(1) as usize;
    let step = src.sample_rate as f64 / dst.sample_rate as f64;
    let mut out = Vec::with_capacity(dst_frames * channels);
    for i in 0..dst_frames {
        let pos = i as f64 * step;
        let left = (pos.floor() as usize).min(src_frames - 1);
        let right = (left + 1).min(src_frames - 1);
        let frac = (pos - left as f64) as f32;
        for c in 0..channels {
            let a = channel_mapped[left * channels + c];
            let b = channel_mapped[right * channels + c];
            out.push(a + (b - a) * frac);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_round_trip_preserves_samples() {
        let spec = AudioSpec::mono(8_000);
        let samples: Vec<f32> = (0..100).map(|i| (i as f32 / 100.0) * 0.9).collect();
        let bytes = encode_wav(&samples, spec).unwrap();
        let decoded = decode_wav(&bytes).unwrap();
        assert_eq!(decoded.spec, spec);
        assert_eq!(decoded.samples.len(), samples.len());
        for (a, b) in samples.iter().zip(&decoded.samples) {
            assert!((a - b).abs() < 2.0 / i16::MAX as f32, "{a} vs {b}");
        }
        assert_eq!(decoded.duration_ms(), 12); // 100 frames at 8 kHz
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode_wav(b"definitely not a wav").is_err());
    }

    #[test]
    fn mono_duplicates_to_stereo_and_back() {
        let mono = vec![0.5, -0.5];
        let stereo = map_frames(
            &mono,
            AudioSpec::mono(8_000),
            AudioSpec {
                sample_rate: 8_000,
                channels: 2,
            },
        );
        assert_eq!(stereo, vec![0.5, 0.5, -0.5, -0.5]);
        let back = map_frames(
            &stereo,
            AudioSpec {
                sample_rate: 8_000,
                channels: 2,
            },
            AudioSpec::mono(8_000),
        );
        for (a, b) in mono.iter().zip(&back) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn resample_halves_the_frames() {
        let samples: Vec<f32> = (0..400).map(|i| (i % 10) as f32 / 10.0).collect();
        let out = map_frames(&samples, AudioSpec::mono(8_000), AudioSpec::mono(4_000));
        assert_eq!(out.len(), 200);
        // Linear interpolation keeps values on the source waveform.
        for (i, v) in out.iter().enumerate() {
            let src = samples[i * 2];
            assert!((v - src).abs() < 0.11, "frame {i}: {v} vs {src}");
        }
    }

    #[test]
    fn identical_specs_pass_through() {
        let samples = vec![0.1, 0.2, 0.3];
        let spec = AudioSpec {
            sample_rate: 48_000,
            channels: 2,
        };
        assert_eq!(map_frames(&samples, spec, spec), samples);
    }
}
