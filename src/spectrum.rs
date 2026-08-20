use std::sync::Arc;

use rustfft::{Fft, FftPlanner, num_complex::Complex32};

pub struct Spectrum {
    fft: Arc<dyn Fft<f32>>,
    input: Vec<Complex32>,
    bins: Vec<f32>,
}

impl Spectrum {
    #[must_use]
    pub fn new(size: usize) -> Self {
        assert!(size.is_power_of_two());
        let mut planner = FftPlanner::new();
        Self {
            fft: planner.plan_fft_forward(size),
            input: vec![Complex32::ZERO; size],
            bins: vec![0.0; size / 2],
        }
    }

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
}
