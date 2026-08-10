use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// FFT window length. 2048 samples at 44.1kHz is a ~46ms analysis window,
/// wide enough to resolve the lowest displayed band (~50Hz needs ≥1 bin at
/// ~21.5Hz/bin) while staying comfortably real-time at the UI tick rate.
pub const FFT_SIZE: usize = 2048;

/// Number of log-spaced frequency bands the spectrum is collapsed into.
/// The renderer regroups these into however many columns fit on screen.
pub const NUM_BANDS: usize = 64;

/// Display range. Podcast material is voice, so the top end stops at 10kHz
/// rather than Nyquist — everything above is sibilance and noise-floor.
const FREQ_MIN: f32 = 50.0;
const FREQ_MAX: f32 = 10_000.0;

/// Magnitudes below this are drawn as silence. -60dB matches the practical
/// noise floor after RNNoise rather than the theoretical f32 floor.
const DB_FLOOR: f32 = -60.0;

/// Per-tick smoothing coefficients (fraction of the distance to the target
/// applied each tick, at the 50ms UI tick). Attack is fast so speech onsets
/// pop; release is slow so the bars fall instead of flickering.
const ATTACK: f32 = 0.6;
const RELEASE: f32 = 0.25;

/// How far a peak-hold marker falls per tick once its band has dropped.
/// 0.03/tick at 20 ticks/s sweeps full scale in ~1.7s.
const PEAK_FALL: f32 = 0.03;

/// Lock-free-enough sample sharing between the cpal callback and the UI.
/// The callback uses `try_lock` and simply skips the push when the UI is
/// mid-snapshot: dropping a visualization chunk is invisible, blocking the
/// audio thread is not. The encoder path (mpsc) is untouched by this tap.
#[derive(Clone, Default)]
pub struct SampleTap {
    inner: Arc<Mutex<VecDeque<f32>>>,
}

impl SampleTap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append samples, keeping only the newest `FFT_SIZE`. Never blocks.
    pub fn push(&self, samples: &[f32]) {
        if let Ok(mut buf) = self.inner.try_lock() {
            buf.extend(samples.iter().copied());
            let excess = buf.len().saturating_sub(FFT_SIZE);
            if excess > 0 {
                buf.drain(..excess);
            }
        }
    }

    /// Copy out exactly `FFT_SIZE` samples, zero-padded at the front (the
    /// old side) when fewer have arrived yet.
    pub fn snapshot(&self) -> Vec<f32> {
        let mut out = vec![0.0; FFT_SIZE];
        if let Ok(buf) = self.inner.lock() {
            let (a, b) = buf.as_slices();
            let start = FFT_SIZE - buf.len();
            out[start..start + a.len()].copy_from_slice(a);
            out[start + a.len()..].copy_from_slice(b);
        }
        out
    }

    pub fn clear(&self) {
        if let Ok(mut buf) = self.inner.lock() {
            buf.clear();
        }
    }
}

/// Streaming FFT band analyzer with attack/release smoothing and
/// peak-hold markers, all in display units (0..1 over `DB_FLOOR`..0dB).
pub struct SpectrumAnalyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    /// Sum of window coefficients, for amplitude normalization.
    window_gain: f32,
    band_bins: Vec<(usize, usize)>,
    bands: Vec<f32>,
    peaks: Vec<f32>,
    scratch: Vec<Complex<f32>>,
}

impl SpectrumAnalyzer {
    pub fn new(sample_rate: f32) -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                let x = i as f32 / (FFT_SIZE - 1) as f32;
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * x).cos())
            })
            .collect();
        let window_gain = window.iter().sum();
        Self {
            fft,
            window,
            window_gain,
            band_bins: band_bins(sample_rate),
            bands: vec![0.0; NUM_BANDS],
            peaks: vec![0.0; NUM_BANDS],
            scratch: vec![Complex::default(); FFT_SIZE],
        }
    }

    /// Analyze one window of samples and fold the result into the smoothed
    /// band/peak state. `samples` shorter than `FFT_SIZE` are front-padded.
    pub fn update(&mut self, samples: &[f32]) {
        let tail = &samples[samples.len().saturating_sub(FFT_SIZE)..];
        let pad = FFT_SIZE - tail.len();
        for (i, slot) in self.scratch.iter_mut().enumerate() {
            let s = if i < pad { 0.0 } else { tail[i - pad] };
            *slot = Complex::new(s * self.window[i], 0.0);
        }
        self.fft.process(&mut self.scratch);

        // Single-sided amplitude: |X[k]| * 2 / sum(window), so a full-scale
        // sine lands at ~1.0 (0dB) in its band.
        let scale = 2.0 / self.window_gain;
        for (i, &(start, end)) in self.band_bins.iter().enumerate() {
            let amp = self.scratch[start..end]
                .iter()
                .map(|c| c.norm() * scale)
                .fold(0.0f32, f32::max);
            let db = 20.0 * amp.max(1e-9).log10();
            let target = ((db - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0);
            Self::step(&mut self.bands[i], target);
        }
        self.settle_peaks();
    }

    /// Advance smoothing toward silence. Called on ticks with no live audio
    /// (Idle etc.) so the bars fall and the peak markers drift down.
    pub fn decay(&mut self) {
        for band in &mut self.bands {
            let mut v = *band;
            Self::step(&mut v, 0.0);
            *band = if v < 1e-3 { 0.0 } else { v };
        }
        self.settle_peaks();
    }

    /// Smoothed band values, 0..1.
    pub fn bands(&self) -> &[f32] {
        &self.bands
    }

    /// Peak-hold values, 0..1. Always >= the matching band value.
    pub fn peaks(&self) -> &[f32] {
        &self.peaks
    }

    /// One tick of asymmetric smoothing toward `target`.
    fn step(value: &mut f32, target: f32) {
        let coeff = if target > *value { ATTACK } else { RELEASE };
        *value += (target - *value) * coeff;
    }

    /// Peak markers ride the bands up instantly and fall at `PEAK_FALL`.
    fn settle_peaks(&mut self) {
        for (peak, &band) in self.peaks.iter_mut().zip(&self.bands) {
            *peak = band.max(*peak - PEAK_FALL);
        }
    }
}

/// Half-open FFT bin ranges `[start, end)` for each of the `NUM_BANDS`
/// log-spaced bands between `FREQ_MIN` and `FREQ_MAX`.
fn band_bins(sample_rate: f32) -> Vec<(usize, usize)> {
    let bin_hz = sample_rate / FFT_SIZE as f32;
    let max_bin = FFT_SIZE / 2;
    let mut bins = Vec::with_capacity(NUM_BANDS);
    let mut prev_end = 0usize;
    for i in 0..NUM_BANDS {
        let f_lo = FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf(i as f32 / NUM_BANDS as f32);
        let f_hi = FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf((i + 1) as f32 / NUM_BANDS as f32);
        // Low bands are narrower than one FFT bin; force ≥1 bin per band and
        // keep ranges contiguous so no bin is counted twice.
        let start = ((f_lo / bin_hz) as usize).max(prev_end).min(max_bin - 1);
        let end = ((f_hi / bin_hz) as usize).clamp(start + 1, max_bin);
        bins.push((start, end));
        prev_end = end;
    }
    bins
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f32 = 44_100.0;

    fn sine(freq: f32, amplitude: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| amplitude * (2.0 * std::f32::consts::PI * freq * i as f32 / SAMPLE_RATE).sin())
            .collect()
    }

    /// Index of the band whose frequency range contains `freq`.
    fn band_index_for(freq: f32) -> usize {
        let ratio = (freq / 50.0).ln() / (10_000.0f32 / 50.0).ln();
        ((ratio * NUM_BANDS as f32) as usize).min(NUM_BANDS - 1)
    }

    #[test]
    fn band_bins_are_monotonic_and_within_nyquist() {
        let bins = band_bins(SAMPLE_RATE);
        assert_eq!(bins.len(), NUM_BANDS);
        let mut prev_end = 0;
        for &(start, end) in &bins {
            assert!(start < end, "empty band: [{start}, {end})");
            assert!(start >= prev_end, "bands overlap or go backwards");
            assert!(end <= FFT_SIZE / 2, "band beyond Nyquist");
            prev_end = end;
        }
    }

    #[test]
    fn sine_lights_up_its_own_band_only() {
        let mut analyzer = SpectrumAnalyzer::new(SAMPLE_RATE);
        let signal = sine(1000.0, 1.0, FFT_SIZE);
        // Converge through the attack smoothing.
        for _ in 0..20 {
            analyzer.update(&signal);
        }
        let bands = analyzer.bands();
        let hit = band_index_for(1000.0);
        let far = band_index_for(100.0);
        assert!(bands[hit] > 0.7, "1kHz band too low: {}", bands[hit]);
        assert!(
            bands[far] < 0.2,
            "100Hz band should stay dark: {}",
            bands[far]
        );
    }

    #[test]
    fn silence_stays_dark() {
        let mut analyzer = SpectrumAnalyzer::new(SAMPLE_RATE);
        for _ in 0..5 {
            analyzer.update(&vec![0.0; FFT_SIZE]);
        }
        assert!(analyzer.bands().iter().all(|&b| b < 0.01));
        assert!(analyzer.bands().iter().all(|&b| (0.0..=1.0).contains(&b)));
    }

    #[test]
    fn attack_is_faster_than_release() {
        let mut analyzer = SpectrumAnalyzer::new(SAMPLE_RATE);
        let signal = sine(1000.0, 1.0, FFT_SIZE);
        let hit = band_index_for(1000.0);

        analyzer.update(&signal);
        let after_one_hit = analyzer.bands()[hit];
        assert!(
            after_one_hit > 0.4,
            "attack too slow: {after_one_hit} after one update"
        );

        analyzer.update(&vec![0.0; FFT_SIZE]);
        let after_one_silence = analyzer.bands()[hit];
        assert!(
            after_one_silence > 0.0 && after_one_silence < after_one_hit,
            "release should fall gradually: {after_one_hit} -> {after_one_silence}"
        );
        // The single-step rise was bigger than the single-step fall.
        assert!(after_one_hit - 0.0 > after_one_hit - after_one_silence);
    }

    #[test]
    fn decay_drains_bands_to_silence() {
        let mut analyzer = SpectrumAnalyzer::new(SAMPLE_RATE);
        let signal = sine(1000.0, 1.0, FFT_SIZE);
        for _ in 0..10 {
            analyzer.update(&signal);
        }
        for _ in 0..200 {
            analyzer.decay();
        }
        assert!(analyzer.bands().iter().all(|&b| b < 0.01));
        assert!(analyzer.peaks().iter().all(|&p| p < 0.01));
    }

    #[test]
    fn peaks_hold_above_falling_bands() {
        let mut analyzer = SpectrumAnalyzer::new(SAMPLE_RATE);
        let signal = sine(1000.0, 1.0, FFT_SIZE);
        let hit = band_index_for(1000.0);
        for _ in 0..10 {
            analyzer.update(&signal);
        }
        let peak_before = analyzer.peaks()[hit];

        // A few silent ticks: the band falls fast, the peak marker falls
        // at its own slower PEAK_FALL rate, staying above the band.
        for _ in 0..3 {
            analyzer.decay();
        }
        let band_after = analyzer.bands()[hit];
        let peak_after = analyzer.peaks()[hit];
        assert!(peak_after >= band_after, "peak marker fell below its band");
        assert!(peak_after < peak_before, "peak marker should drift down");
        assert!(
            peak_before - peak_after < 0.2,
            "peak marker fell too fast: {peak_before} -> {peak_after}"
        );
    }

    #[test]
    fn peaks_never_below_bands() {
        let mut analyzer = SpectrumAnalyzer::new(SAMPLE_RATE);
        let signal = sine(3000.0, 0.8, FFT_SIZE);
        for _ in 0..7 {
            analyzer.update(&signal);
        }
        for (b, p) in analyzer.bands().iter().zip(analyzer.peaks()) {
            assert!(p >= b);
        }
    }

    #[test]
    fn tap_keeps_only_newest_window() {
        let tap = SampleTap::new();
        tap.push(&vec![1.0; FFT_SIZE]);
        tap.push(&vec![2.0; 100]);
        let snap = tap.snapshot();
        assert_eq!(snap.len(), FFT_SIZE);
        // Newest 100 samples are the 2.0s, the rest are the tail of the 1.0s.
        assert!(snap[FFT_SIZE - 100..].iter().all(|&s| s == 2.0));
        assert!(snap[..FFT_SIZE - 100].iter().all(|&s| s == 1.0));
    }

    #[test]
    fn tap_zero_pads_short_snapshots() {
        let tap = SampleTap::new();
        tap.push(&[0.5; 10]);
        let snap = tap.snapshot();
        assert_eq!(snap.len(), FFT_SIZE);
        assert!(snap[..FFT_SIZE - 10].iter().all(|&s| s == 0.0));
        assert!(snap[FFT_SIZE - 10..].iter().all(|&s| s == 0.5));
    }

    #[test]
    fn tap_clear_empties_buffer() {
        let tap = SampleTap::new();
        tap.push(&[0.5; 500]);
        tap.clear();
        assert!(tap.snapshot().iter().all(|&s| s == 0.0));
    }
}
