use std::collections::VecDeque;

const STARTUP_CAPACITY: usize = 4_096;

#[derive(Clone, Debug)]
pub struct Waveform {
    left: VecDeque<f32>,
    right: VecDeque<f32>,
    left_peak: f32,
    right_peak: f32,
    capacity: usize,
}

impl Waveform {
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

    pub fn push(&mut self, left: &[f32], right: &[f32]) {
        debug_assert_eq!(left.len(), right.len());
        self.left_peak = 0.0;
        self.right_peak = 0.0;
        for (&left_sample, &right_sample) in left.iter().zip(right) {
            if self.left.len() == self.capacity {
                self.left.pop_front();
                self.right.pop_front();
            }
            let left_sample = left_sample.clamp(-1.0, 1.0);
            let right_sample = right_sample.clamp(-1.0, 1.0);
            self.left_peak = self.left_peak.max(left_sample.abs());
            self.right_peak = self.right_peak.max(right_sample.abs());
            self.left.push_back(left_sample);
            self.right.push_back(right_sample);
        }
    }

    #[must_use]
    pub fn channels(&self) -> (&VecDeque<f32>, &VecDeque<f32>) {
        (&self.left, &self.right)
    }

    #[must_use]
    pub fn peaks(&self) -> (f32, f32) {
        (self.left_peak, self.right_peak)
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
}
