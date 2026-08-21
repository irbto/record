//! Lazy frequency-spectrum calculation for the terminal display.
//!
//! The TUI creates [`crate::spectrum::Spectrum`] only when a spectrum-bearing view receives its
//! first sample block. This keeps FFT planning out of the initial capture path.
//! Each update applies a Hann window, runs a forward FFT, converts magnitudes to
//! a 60 dB display range, and clamps the result for Ratatui.

use std::sync::Arc;

use rustfft::{Fft, FftPlanner, num_complex::Complex32};

/// Owns one reusable FFT plan and its display bins.
pub struct Spectrum {
    fft: Arc<dyn Fft<f32>>,
    input: Vec<Complex32>,
    bins: Vec<f32>,
}

impl Spectrum {
    /// Creates a spectrum with a power-of-two transform size.
    ///
    /// # Panics
    ///
    /// Panics when `size` is less than two or is not a power of two.
    #[must_use]
    pub fn new(size: usize) -> Self {
        assert!(size >= 2 && size.is_power_of_two());
        let mut planner = FftPlanner::new();
        Self {
            fft: planner.plan_fft_forward(size),
            input: vec![Complex32::ZERO; size],
            bins: vec![0.0; size / 2],
        }
    }

    /// Replaces the input window with the newest samples and updates the bins.
    pub fn update(&mut self, samples: impl DoubleEndedIterator<Item = f32>) {
        self.input.fill(Complex32::ZERO);
        let input_len = self.input.len();
        for (index, sample) in samples.rev().take(input_len).enumerate() {
            let target = input_len - 1 - index;
            let phase = target as f32 / (input_len - 1) as f32;
            let hann = 0.5 - 0.5 * (std::f32::consts::TAU * phase).cos();
            self.input[target].re = sample * hann;
        }
        self.fft.process(&mut self.input);
        let normalizer = 2.0 / input_len as f32;
        for (bin, sample) in self.bins.iter_mut().zip(&self.input) {
            *bin = (sample.norm() * normalizer)
                .max(1.0e-6)
                .log10()
                .mul_add(20.0, 60.0)
                / 60.0;
            *bin = bin.clamp(0.0, 1.0);
        }
    }

    #[must_use]
    /// Returns normalized bins from zero hertz to the Nyquist limit.
    pub fn bins(&self) -> &[f32] {
        &self.bins
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_has_no_visible_bins() {
        let mut spectrum = Spectrum::new(256);
        spectrum.update(std::iter::repeat_n(0.0, 256));
        assert!(spectrum.bins().iter().all(|bin| *bin == 0.0));
    }

    #[test]
    fn a_bin_centered_tone_has_its_largest_bin_at_the_expected_index() {
        let size = 256;
        let expected = 16;
        let samples = (0..size).map(|index| {
            (std::f32::consts::TAU * expected as f32 * index as f32 / size as f32).sin()
        });
        let mut spectrum = Spectrum::new(size);
        spectrum.update(samples);
        let strongest = spectrum
            .bins()
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map(|(index, _)| index)
            .unwrap();
        assert_eq!(strongest, expected);
    }

    #[test]
    fn all_bins_remain_normalized() {
        let mut spectrum = Spectrum::new(64);
        spectrum
            .update((0_usize..128).map(|index| if index.is_multiple_of(2) { 4.0 } else { -4.0 }));
        assert!(
            spectrum
                .bins()
                .iter()
                .all(|bin| bin.is_finite() && (0.0..=1.0).contains(bin))
        );
    }

    #[test]
    #[should_panic]
    fn a_non_power_of_two_size_is_rejected() {
        let _ = Spectrum::new(100);
    }

    #[test]
    #[should_panic]
    fn a_one_sample_transform_is_rejected() {
        let _ = Spectrum::new(1);
    }
}
