//! From the engine's samples to the clip the app stores (design §10.3 step
//! 6): mono at 24 kHz (resampled if the engine runs at another rate), silence
//! trimmed, loudness normalised to −16 LUFS (EBU R128), peaks held under
//! −1.5 dBTP by a look-ahead limiter (speech has a high crest factor: plain
//! scaling to the peak ceiling would leave sentences near −19 LUFS), short
//! silences padded on both ends, MP3 (LAME, VBR).

use std::num::NonZeroU32;

use ebur128::{EbuR128, Mode};
use mp3lame_encoder::{Builder, FlushNoGap, MonoPcm, Quality, VbrMode};

/// Sample rate of every clip (Kokoro's own rate; an MPEG-2 Layer III rate).
pub const OUTPUT_RATE: u32 = 24_000;
/// Integrated loudness of every clip, LUFS.
pub const TARGET_LUFS: f64 = -16.0;
/// The gain is clamped to ±20 dB: a near-silent clip is not blown up.
const MAX_GAIN_DB: f64 = 20.0;
/// True-peak ceiling, dBTP.
const PEAK_LIMIT_DBTP: f64 = -1.5;
/// Leading and trailing samples below this level are cut.
const SILENCE_DBFS: f64 = -50.0;
/// Silence added before and after (mobile browsers clip the first
/// milliseconds of a clip; a short tail sounds natural).
const PAD_BEFORE_MS: u32 = 60;
const PAD_AFTER_MS: u32 = 150;
/// The limiter aims this far below the ceiling (sample peaks; the true
/// peak between samples can be a little higher), dB.
const LIMITER_MARGIN_DB: f64 = 0.5;
/// Limiter look-ahead (the gain falls smoothly over this time before a
/// peak) and release (time constant of the recovery after it).
const LIMITER_LOOKAHEAD_MS: u32 = 5;
const LIMITER_RELEASE_MS: f64 = 80.0;
/// Make-up passes: the limiter lowers the loudness a little; it is made up
/// by at most this much more gain per pass (never beyond `MAX_GAIN_DB`).
const MAX_MAKEUP_DB: f64 = 6.0;
const MAKEUP_PASSES: usize = 4;
/// Half-width of the resampling filter in input samples (at cutoff 1).
const RESAMPLE_TAPS: f64 = 16.0;

/// Mono samples (−1.0..1.0) at a sample rate.
#[derive(Clone, PartialEq)]
pub struct Pcm {
    samples: Vec<f32>,
    sample_rate: u32,
}

impl std::fmt::Debug for Pcm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Pcm({} samples at {} Hz)",
            self.samples.len(),
            self.sample_rate
        )
    }
}

impl Pcm {
    pub fn new(samples: Vec<f32>, sample_rate: u32) -> Self {
        Self {
            samples,
            sample_rate,
        }
    }

    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn duration_ms(&self) -> u64 {
        duration_ms(self.samples.len(), self.sample_rate)
    }
}

/// A finished clip. Serialisable, so that it can come from the engine
/// process (`crate::worker`); the MP3 bytes as hex.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Clip {
    #[serde(with = "hex_bytes")]
    mp3: Vec<u8>,
    duration_ms: u64,
    loudness_lufs: f64,
}

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        hex::decode(text).map_err(D::Error::custom)
    }
}

impl std::fmt::Debug for Clip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Clip({} bytes, {} ms, {:.1} LUFS)",
            self.mp3.len(),
            self.duration_ms,
            self.loudness_lufs
        )
    }
}

impl Clip {
    pub fn mp3(&self) -> &[u8] {
        &self.mp3
    }

    pub fn into_mp3(self) -> Vec<u8> {
        self.mp3
    }

    /// Duration of the audio including the padding.
    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    /// Loudness of the normalised audio before encoding (integrated, or
    /// the fallback measure of short clips), LUFS.
    pub fn loudness_lufs(&self) -> f64 {
        self.loudness_lufs
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PostError {
    #[error("the engine produced no audible sound")]
    Silent,
    #[error("invalid sample rate {0} Hz")]
    SampleRate(u32),
    #[error("loudness measurement failed: {0}")]
    Loudness(String),
    #[error("MP3 encoding failed: {0}")]
    Encode(String),
}

/// Run the whole chain on `pcm`.
pub fn to_mp3(pcm: Pcm) -> Result<Clip, PostError> {
    if pcm.sample_rate == 0 {
        return Err(PostError::SampleRate(0));
    }
    let mut samples = resample(&sanitize(pcm.samples), pcm.sample_rate, OUTPUT_RATE);
    trim_silence(&mut samples);
    if samples.is_empty() {
        return Err(PostError::Silent);
    }
    normalize(&mut samples, OUTPUT_RATE)?;
    let loudness_lufs = loudness(&samples, OUTPUT_RATE)?;
    let padded = pad(&samples, OUTPUT_RATE);
    let mp3 = encode(&padded, OUTPUT_RATE)?;
    Ok(Clip {
        mp3,
        duration_ms: duration_ms(padded.len(), OUTPUT_RATE),
        loudness_lufs,
    })
}

/// Loudness to [`TARGET_LUFS`] (gain clamped to ±[`MAX_GAIN_DB`]) with the
/// limiter holding the peaks; make-up passes restore the loudness the
/// limiter took (each pass is partly limited again); finally the true peak
/// is checked and, if still above the ceiling, the whole clip is scaled
/// down.
fn normalize(samples: &mut [f32], rate: u32) -> Result<(), PostError> {
    let gain_db = (TARGET_LUFS - loudness(samples, rate)?).clamp(-MAX_GAIN_DB, MAX_GAIN_DB);
    scale(samples, db_to_linear(gain_db));
    let limit = db_to_linear(PEAK_LIMIT_DBTP - LIMITER_MARGIN_DB);
    limit_peaks(samples, rate, limit);
    let mut total_db = gain_db;
    for _ in 0..MAKEUP_PASSES {
        let makeup_db = (TARGET_LUFS - loudness(samples, rate)?)
            .clamp(0.0, MAX_MAKEUP_DB)
            .min(MAX_GAIN_DB - total_db);
        if makeup_db < 0.1 {
            break;
        }
        scale(samples, db_to_linear(makeup_db));
        total_db += makeup_db;
        limit_peaks(samples, rate, limit);
    }
    let peak = true_peak(samples, rate)?;
    let ceiling = db_to_linear(PEAK_LIMIT_DBTP);
    if peak > ceiling {
        scale(samples, ceiling / peak);
    }
    Ok(())
}

/// A look-ahead peak limiter: no sample exceeds `limit`. The gain needed
/// at each sample is spread backwards over the look-ahead (sliding minimum,
/// then a moving average, so it falls smoothly and is fully applied at the
/// peak) and recovers with the release time constant.
fn limit_peaks(samples: &mut [f32], rate: u32, limit: f64) {
    let n = samples.len();
    let window = ((u64::from(rate) * u64::from(LIMITER_LOOKAHEAD_MS) / 1000) as usize).max(1);
    let needed: Vec<f64> = samples
        .iter()
        .map(|s| {
            let level = f64::from(s.abs());
            if level > limit { limit / level } else { 1.0 }
        })
        .collect();
    if needed.iter().all(|g| *g >= 1.0) {
        return;
    }
    // Minimum over [i, i + window): the gain needed within the look-ahead
    // (a monotonic queue of indices with increasing needed gain).
    let mut ahead = vec![1.0; n];
    let mut queue = std::collections::VecDeque::new();
    for i in (0..n).rev() {
        while queue.back().is_some_and(|&j| needed[j] >= needed[i]) {
            queue.pop_back();
        }
        queue.push_back(i);
        while queue.front().is_some_and(|&j| j >= i + window) {
            queue.pop_front();
        }
        ahead[i] = queue.front().map_or(1.0, |&j| needed[j]);
    }
    // Moving average over [i - window + 1, i]: every term whose window
    // contains a peak's position is at most that peak's gain, so the
    // average is at most that gain at the peak itself.
    let release = (-1.0 / (LIMITER_RELEASE_MS / 1000.0 * f64::from(rate))).exp();
    let mut sum = 0.0;
    let mut gain = 1.0f64;
    for i in 0..n {
        sum += ahead[i];
        if i >= window {
            sum -= ahead[i - window];
        }
        let smooth = sum / (i + 1).min(window) as f64;
        // Recover towards 1 with the release time constant, never above
        // what the look-ahead asks for.
        gain = (1.0 - (1.0 - gain) * release).min(smooth);
        samples[i] = (f64::from(samples[i]) * gain) as f32;
    }
}

fn duration_ms(samples: usize, rate: u32) -> u64 {
    if rate == 0 {
        return 0;
    }
    (samples as u64).saturating_mul(1000) / u64::from(rate)
}

fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// NaN/infinite samples (a broken model output) become silence; the rest
/// is clamped to full scale.
fn sanitize(mut samples: Vec<f32>) -> Vec<f32> {
    for s in &mut samples {
        *s = if s.is_finite() {
            s.clamp(-1.0, 1.0)
        } else {
            0.0
        };
    }
    samples
}

fn scale(samples: &mut [f32], factor: f64) {
    for s in samples {
        *s = (f64::from(*s) * factor) as f32;
    }
}

/// Band-limited resampling (windowed sinc, Lanczos window); a no-op at the
/// same rate.
fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = f64::from(from) / f64::from(to);
    // Below the Nyquist frequency of the lower of the two rates.
    let cutoff = (1.0 / ratio).min(1.0);
    let radius = RESAMPLE_TAPS / cutoff;
    let out_len = ((samples.len() as f64) / ratio).round() as usize;
    let sinc = |x: f64| {
        if x.abs() < 1e-12 {
            1.0
        } else {
            let px = std::f64::consts::PI * x;
            px.sin() / px
        }
    };
    (0..out_len)
        .map(|j| {
            let t = j as f64 * ratio;
            let first = (t - radius).ceil().max(0.0) as usize;
            let last = ((t + radius).floor() as usize).min(samples.len() - 1);
            let mut sum = 0.0;
            for (k, &x) in samples.iter().enumerate().take(last + 1).skip(first) {
                let d = t - k as f64;
                let weight = cutoff * sinc(cutoff * d) * sinc(d / radius);
                sum += f64::from(x) * weight;
            }
            sum as f32
        })
        .collect()
}

/// Cut leading and trailing samples below [`SILENCE_DBFS`].
fn trim_silence(samples: &mut Vec<f32>) {
    let threshold = db_to_linear(SILENCE_DBFS) as f32;
    let audible = |s: &f32| s.abs() >= threshold;
    let Some(start) = samples.iter().position(audible) else {
        samples.clear();
        return;
    };
    let end = samples.iter().rposition(audible).map_or(start, |e| e + 1);
    samples.truncate(end);
    samples.drain(..start);
}

/// Integrated loudness (LUFS); for clips too short for the gated
/// measurement (< 400 ms) the maximum momentary loudness, and for clips
/// shorter than one momentary window the K-weighting-free mean-square level.
fn loudness(samples: &[f32], rate: u32) -> Result<f64, PostError> {
    let err = |e: ebur128::Error| PostError::Loudness(e.to_string());
    let mut meter = EbuR128::new(1, rate, Mode::I | Mode::M).map_err(err)?;
    let step = (rate / 10) as usize; // 100 ms
    let mut max_momentary = f64::NEG_INFINITY;
    for block in samples.chunks(step.max(1)) {
        meter.add_frames_f32(block).map_err(err)?;
        let momentary = meter.loudness_momentary().map_err(err)?;
        if momentary.is_finite() {
            max_momentary = max_momentary.max(momentary);
        }
    }
    let integrated = meter.loudness_global().map_err(err)?;
    if integrated.is_finite() {
        return Ok(integrated);
    }
    if max_momentary.is_finite() {
        return Ok(max_momentary);
    }
    let mean_square =
        samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64;
    if mean_square > 0.0 {
        Ok(-0.691 + 10.0 * mean_square.log10())
    } else {
        Err(PostError::Silent)
    }
}

/// True peak (linear, 1.0 = full scale), EBU R128 oversampled.
fn true_peak(samples: &[f32], rate: u32) -> Result<f64, PostError> {
    let err = |e: ebur128::Error| PostError::Loudness(e.to_string());
    let mut meter = EbuR128::new(1, rate, Mode::TRUE_PEAK).map_err(err)?;
    meter.add_frames_f32(samples).map_err(err)?;
    meter.true_peak(0).map_err(err)
}

fn pad(samples: &[f32], rate: u32) -> Vec<f32> {
    let frames = |ms: u32| (u64::from(rate) * u64::from(ms) / 1000) as usize;
    let mut out = Vec::with_capacity(samples.len() + frames(PAD_BEFORE_MS) + frames(PAD_AFTER_MS));
    out.resize(frames(PAD_BEFORE_MS), 0.0);
    out.extend_from_slice(samples);
    out.resize(out.len() + frames(PAD_AFTER_MS), 0.0);
    out
}

/// MP3, mono, VBR quality 5 (≈ 48–64 kbit/s for speech at 24 kHz), with a
/// LAME/Xing tag in the first frame so players know the exact duration.
fn encode(samples: &[f32], rate: u32) -> Result<Vec<u8>, PostError> {
    let err = |e: &dyn std::fmt::Display| PostError::Encode(e.to_string());
    let mut builder = Builder::new()
        .ok_or_else(|| PostError::Encode("cannot allocate the encoder".to_owned()))?;
    builder.set_num_channels(1).map_err(|e| err(&e))?;
    builder.set_sample_rate(rate).map_err(|e| err(&e))?;
    builder
        .set_output_sample_rate(NonZeroU32::new(rate))
        .map_err(|e| err(&e))?;
    builder
        .set_mode(mp3lame_encoder::Mode::Mono)
        .map_err(|e| err(&e))?;
    builder.set_vbr_mode(VbrMode::Mtrh).map_err(|e| err(&e))?;
    builder
        .set_vbr_quality(Quality::Good)
        .map_err(|e| err(&e))?;
    builder.set_quality(Quality::Good).map_err(|e| err(&e))?;
    builder.set_to_write_vbr_tag(true).map_err(|e| err(&e))?;
    let mut encoder = builder.build().map_err(|e| err(&e))?;
    let mut mp3 = Vec::with_capacity(mp3lame_encoder::max_required_buffer_size(samples.len()));
    encoder
        .encode_to_vec(MonoPcm(samples), &mut mp3)
        .map_err(|e| err(&e))?;
    mp3.reserve(7200);
    encoder
        .flush_to_vec::<FlushNoGap>(&mut mp3)
        .map_err(|e| err(&e))?;
    // The first frame was a placeholder for the tag (no ID3 tag precedes it).
    let mut tag = Vec::with_capacity(encoder.lame_tag_size());
    if encoder.lame_tag_encode_to_vec(&mut tag).is_some() && tag.len() <= mp3.len() {
        mp3[..tag.len()].copy_from_slice(&tag);
    }
    Ok(mp3)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tone: `seconds` of a sine at `freq` Hz and `amplitude`.
    fn tone(freq: f64, seconds: f64, amplitude: f64, rate: u32) -> Vec<f32> {
        let n = (seconds * f64::from(rate)) as usize;
        (0..n)
            .map(|i| {
                (amplitude * (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin())
                    as f32
            })
            .collect()
    }

    /// Speech-like test signal: a tone with silence before and after.
    fn padded_tone(amplitude: f64, rate: u32) -> Vec<f32> {
        let mut samples = vec![0.0; rate as usize / 2];
        samples.extend(tone(440.0, 1.5, amplitude, rate));
        samples.extend(vec![0.0; rate as usize / 2]);
        samples
    }

    /// Frames of an MPEG audio stream: (count, sample rate of the first).
    /// Checks every frame header (sync word, layer III, valid bitrate and
    /// sample rate) and that frames follow each other without gaps.
    fn mp3_frames(data: &[u8]) -> (usize, u32) {
        const BITRATES_V2: [u32; 16] = [
            0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
        ];
        const BITRATES_V1: [u32; 16] = [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
        ];
        let mut pos = 0;
        let mut count = 0;
        let mut first_rate = 0;
        while pos + 4 <= data.len() {
            let h = &data[pos..pos + 4];
            assert!(
                h[0] == 0xFF && h[1] & 0xE0 == 0xE0,
                "no frame sync at {pos}"
            );
            let version = (h[1] >> 3) & 0b11; // 3 = MPEG-1, 2 = MPEG-2
            assert_eq!((h[1] >> 1) & 0b11, 0b01, "layer III");
            let bitrate_index = (h[2] >> 4) as usize;
            let rate_index = ((h[2] >> 2) & 0b11) as usize;
            let padding = u32::from((h[2] >> 1) & 1);
            assert!(rate_index < 3);
            let (bitrate, rate, coefficient) = match version {
                3 => (
                    BITRATES_V1[bitrate_index],
                    [44_100, 48_000, 32_000][rate_index],
                    144,
                ),
                2 => (
                    BITRATES_V2[bitrate_index],
                    [22_050, 24_000, 16_000][rate_index],
                    72,
                ),
                _ => panic!("unexpected MPEG version {version}"),
            };
            assert!(bitrate > 0, "bitrate index {bitrate_index}");
            if count == 0 {
                first_rate = rate;
            }
            pos += (coefficient * bitrate * 1000 / rate + padding) as usize;
            count += 1;
        }
        assert_eq!(pos, data.len(), "trailing bytes after the last frame");
        (count, first_rate)
    }

    fn measured_loudness(samples: &[f32]) -> f64 {
        let mut meter = EbuR128::new(1, OUTPUT_RATE, Mode::I).expect("test setup");
        meter.add_frames_f32(samples).expect("test setup");
        meter.loudness_global().expect("test setup")
    }

    #[test]
    fn r40_clip_is_valid_mp3_of_the_expected_duration() {
        let clip =
            to_mp3(Pcm::new(padded_tone(0.3, OUTPUT_RATE), OUTPUT_RATE)).expect("test setup");
        // 1.5 s of tone, silences trimmed, 60 + 150 ms of padding added.
        // (The sine's zero crossings at both ends are trimmed: a sample or two.)
        let expected_ms = 1500 + u64::from(PAD_BEFORE_MS) + u64::from(PAD_AFTER_MS);
        assert!(
            clip.duration_ms().abs_diff(expected_ms) <= 2,
            "{} ms",
            clip.duration_ms()
        );
        let (frames, rate) = mp3_frames(clip.mp3());
        assert_eq!(rate, OUTPUT_RATE);
        // MPEG-2 layer III: 576 samples per frame (plus encoder delay frames
        // and the tag frame).
        let expected = (1.71 * f64::from(OUTPUT_RATE) / 576.0) as usize;
        assert!(
            (expected..expected + 6).contains(&frames),
            "{frames} frames, expected ~{expected}"
        );
        // VBR quality 5 for a pure tone: far below 64 kbit/s.
        assert!(
            clip.mp3().len() < 64_000 / 8 * 2,
            "{} bytes",
            clip.mp3().len()
        );
        // The first frame carries the VBR tag.
        assert!(
            clip.mp3()
                .windows(4)
                .take(200)
                .any(|w| w == b"Xing" || w == b"Info")
        );
    }

    /// Speech-like test signal with a high crest factor: a quiet carrier
    /// with short loud bursts (plosives), between silences.
    fn peaky(rate: u32) -> Vec<f32> {
        let eighth = rate as usize / 8;
        let mut samples = vec![0.0; rate as usize / 4];
        samples.extend(
            tone(180.0, 2.0, 0.1, rate)
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    if (i / eighth).is_multiple_of(4) && i % eighth < 240 {
                        s * 9.0
                    } else {
                        *s
                    }
                }),
        );
        samples.extend(vec![0.0; rate as usize / 4]);
        samples
    }

    fn normalized(mut samples: Vec<f32>) -> Vec<f32> {
        trim_silence(&mut samples);
        normalize(&mut samples, OUTPUT_RATE).expect("test setup");
        samples
    }

    #[test]
    fn r40_loudness_is_normalised_to_minus_16_lufs() {
        for amplitude in [0.05, 0.1, 0.5] {
            let samples = normalized(padded_tone(amplitude, OUTPUT_RATE));
            let lufs = measured_loudness(&samples);
            assert!(
                (lufs - TARGET_LUFS).abs() < 0.5,
                "amplitude {amplitude}: {lufs} LUFS"
            );
        }
    }

    #[test]
    fn r40_peaky_speech_reaches_the_target_under_the_peak_ceiling() {
        let samples = normalized(peaky(OUTPUT_RATE));
        let lufs = measured_loudness(&samples);
        assert!((lufs - TARGET_LUFS).abs() < 0.7, "{lufs} LUFS");
        let peak = true_peak(&samples, OUTPUT_RATE).expect("test setup");
        assert!(peak <= db_to_linear(PEAK_LIMIT_DBTP) + 1e-6, "peak {peak}");
        // A full-scale square-ish signal is limited, not clipped.
        let square: Vec<f32> = tone(200.0, 1.0, 1.0, OUTPUT_RATE)
            .iter()
            .map(|s| s.signum() * 0.99)
            .collect();
        let samples = normalized(square);
        assert!(
            true_peak(&samples, OUTPUT_RATE).expect("test setup")
                <= db_to_linear(PEAK_LIMIT_DBTP) + 1e-6
        );
    }

    #[test]
    fn r40_limiter_holds_the_limit_and_leaves_quiet_parts_alone() {
        let mut samples = peaky(OUTPUT_RATE);
        let before = samples.clone();
        limit_peaks(&mut samples, OUTPUT_RATE, 0.5);
        assert!(samples.iter().all(|s| s.abs() <= 0.5 + 1e-6));
        // Far from any burst (the carrier between bursts) nothing changes.
        let quiet = OUTPUT_RATE as usize / 4 + OUTPUT_RATE as usize / 8 * 2 + 1000;
        assert!(
            (samples[quiet] - before[quiet]).abs() < 1e-3,
            "{} vs {}",
            samples[quiet],
            before[quiet]
        );
        // A signal under the limit is untouched.
        let mut soft = tone(440.0, 0.5, 0.3, OUTPUT_RATE);
        let copy = soft.clone();
        limit_peaks(&mut soft, OUTPUT_RATE, 0.5);
        assert_eq!(soft, copy);
    }

    #[test]
    fn r40_gain_is_clamped_for_near_silence() {
        let samples = normalized(tone(440.0, 1.0, 0.004, OUTPUT_RATE)); // about -51 LUFS
        let lufs = measured_loudness(&samples);
        assert!((lufs - (-51.0 + MAX_GAIN_DB)).abs() < 1.5, "{lufs}");
    }

    #[test]
    fn r40_short_clip_uses_momentary_loudness() {
        let samples = tone(440.0, 0.25, 0.2, OUTPUT_RATE);
        let lufs = loudness(&samples, OUTPUT_RATE).expect("test setup");
        assert!(lufs.is_finite() && (-30.0..-5.0).contains(&lufs), "{lufs}");
        let clip = to_mp3(Pcm::new(samples, OUTPUT_RATE)).expect("test setup");
        assert!(
            clip.duration_ms().abs_diff(250 + 60 + 150) <= 2,
            "{} ms",
            clip.duration_ms()
        );
        mp3_frames(clip.mp3());
    }

    #[test]
    fn r40_silence_and_broken_samples_are_rejected() {
        assert!(matches!(
            to_mp3(Pcm::new(vec![0.0; 24_000], OUTPUT_RATE)),
            Err(PostError::Silent)
        ));
        assert!(matches!(
            to_mp3(Pcm::new(vec![f32::NAN; 24_000], OUTPUT_RATE)),
            Err(PostError::Silent)
        ));
        assert!(matches!(
            to_mp3(Pcm::new(Vec::new(), OUTPUT_RATE)),
            Err(PostError::Silent)
        ));
        assert!(matches!(
            to_mp3(Pcm::new(vec![0.5; 10], 0)),
            Err(PostError::SampleRate(0))
        ));
    }

    #[test]
    fn r40_trim_cuts_only_the_quiet_ends() {
        let mut samples = vec![0.0, 0.001, 0.5, 0.0, -0.5, 0.001, 0.0];
        trim_silence(&mut samples);
        assert_eq!(samples, vec![0.5, 0.0, -0.5]);
    }

    #[test]
    fn r40_resampling_keeps_duration_pitch_and_level() {
        let input = tone(1000.0, 1.0, 0.5, 48_000);
        let output = resample(&input, 48_000, OUTPUT_RATE);
        assert_eq!(output.len(), 24_000);
        // Zero crossings of a 1 kHz tone: ~2000 per second.
        let crossings = output
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        assert!((1990..=2010).contains(&crossings), "{crossings}");
        let middle = &output[2400..21_600];
        let peak = middle.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((0.48..0.52).contains(&peak), "{peak}");
        // An 11 kHz tone is below the 12 kHz Nyquist limit, 13 kHz is not:
        // it must be filtered out, not aliased.
        let high = resample(&tone(13_000.0, 0.5, 0.5, 48_000), 48_000, OUTPUT_RATE);
        let residue = high[1200..10_800].iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(residue < 0.05, "aliasing residue {residue}");
        // Upsampling works too, and the same rate is a no-op.
        assert_eq!(resample(&input[..480], 48_000, 96_000).len(), 960);
        assert_eq!(
            resample(&input[..480], 48_000, 48_000),
            input[..480].to_vec()
        );
    }

    #[test]
    fn r40_engine_rate_other_than_24k_is_resampled() {
        let clip = to_mp3(Pcm::new(padded_tone(0.3, 22_050), 22_050)).expect("test setup");
        let (_, rate) = mp3_frames(clip.mp3());
        assert_eq!(rate, OUTPUT_RATE);
        assert!(
            (1700..=1720).contains(&clip.duration_ms()),
            "{}",
            clip.duration_ms()
        );
    }
}
