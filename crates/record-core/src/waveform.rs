//! Rolling stereo sample history for the live terminal display.
//!
//! [`crate::waveform::Waveform`] retains only the newest samples needed by the six-second scope.
//! It starts with a small allocation and grows with captured audio. This avoids
//! a large allocation before WASAPI starts. Level peaks describe only the most
//! recently pushed block, while channel history spans the configured capacity.

use std::collections::VecDeque;

const STARTUP_CAPACITY: usize = 4_096;

#[derive(Clone, Debug)]
/// Stores bounded left and right sample history plus current level peaks.
pub struct Waveform {
    left: VecDeque<f32>,
    right: VecDeque<f32>,
    left_peak: f32,
    right_peak: f32,
    capacity: usize,
}

impl Waveform {
    /// Creates an empty rolling history with the specified frame capacity.
    ///
    /// # Panics
    ///
    /// Panics when `capacity` is zero.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "waveform capacity must be greater than zero");
        Self {
            // Grow the six-second scope with incoming audio instead of reserving
            // hundreds of thousands of samples on the capture hot path.
            left: VecDeque::with_capacity(capacity.min(STARTUP_CAPACITY)),
            right: VecDeque::with_capacity(capacity.min(STARTUP_CAPACITY)),
            left_peak: 0.0,
            right_peak: 0.0,
            capacity,
        }
    }

    /// Appends one equal-length stereo block and clamps samples to `-1.0..=1.0`.
    pub fn push(&mut self, left: &[f32], right: &[f32]) {
        assert_eq!(
            left.len(),
            right.len(),
            "waveform channels must have equal lengths"
        );
        self.left_peak = 0.0;
        self.right_peak = 0.0;
        for (&left_sample, &right_sample) in left.iter().zip(right) {
            if self.left.len() == self.capacity {
                self.left.pop_front();
                self.right.pop_front();
            }
            let left_sample = finite_sample(left_sample);
            let right_sample = finite_sample(right_sample);
            self.left_peak = self.left_peak.max(left_sample.abs());
            self.right_peak = self.right_peak.max(right_sample.abs());
            self.left.push_back(left_sample);
            self.right.push_back(right_sample);
        }
    }

    #[must_use]
    /// Borrows the left and right rolling histories.
    pub fn channels(&self) -> (&VecDeque<f32>, &VecDeque<f32>) {
        (&self.left, &self.right)
    }

    #[must_use]
    /// Returns absolute peaks for the most recently pushed block.
    pub fn peaks(&self) -> (f32, f32) {
        (self.left_peak, self.right_peak)
    }
}

/// Clamps a display sample and converts invalid values to silence.
fn finite_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_the_newest_samples() {
        let mut waveform = Waveform::new(3);
        waveform.push(&[0.1, 0.2], &[-0.1, -0.2]);
        waveform.push(&[0.3, 0.4], &[-0.3, -0.4]);
        let (left, right) = waveform.channels();
        assert_eq!(
            left.iter().copied().collect::<Vec<_>>(),
            vec![0.2, 0.3, 0.4]
        );
        assert_eq!(
            right.iter().copied().collect::<Vec<_>>(),
            vec![-0.2, -0.3, -0.4]
        );
    }

    #[test]
    fn clamps_samples() {
        let mut waveform = Waveform::new(2);
        waveform.push(&[2.0], &[-2.0]);
        assert_eq!(waveform.peaks(), (1.0, 1.0));
    }

    #[test]
    fn meters_follow_the_latest_block() {
        let mut waveform = Waveform::new(4);
        waveform.push(&[1.0], &[-0.8]);
        waveform.push(&[0.2, -0.3], &[0.1, -0.4]);
        assert_eq!(waveform.peaks(), (0.3, 0.4));
    }

    #[test]
    fn nonfinite_samples_become_silence() {
        let mut waveform = Waveform::new(2);
        waveform.push(&[f32::NAN], &[f32::INFINITY]);
        assert_eq!(waveform.peaks(), (0.0, 0.0));
        let (left, right) = waveform.channels();
        assert_eq!(left[0], 0.0);
        assert_eq!(right[0], 0.0);
    }

    #[test]
    fn an_empty_block_clears_the_current_meter_peaks() {
        let mut waveform = Waveform::new(2);
        waveform.push(&[0.8], &[0.6]);
        waveform.push(&[], &[]);
        assert_eq!(waveform.peaks(), (0.0, 0.0));
        assert_eq!(waveform.channels().0.len(), 1);
    }

    #[test]
    #[should_panic(expected = "waveform capacity must be greater than zero")]
    fn zero_capacity_is_rejected() {
        let _ = Waveform::new(0);
    }

    #[test]
    #[should_panic]
    fn unequal_channel_lengths_are_rejected() {
        let mut waveform = Waveform::new(2);
        waveform.push(&[0.1], &[]);
    }
}
