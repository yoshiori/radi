pub struct NoiseGate {
    threshold: f32,
    hold_samples: usize,
    hold_counter: usize,
}

impl NoiseGate {
    pub fn new(threshold: f32, hold_samples: usize) -> Self {
        Self {
            threshold,
            hold_samples,
            hold_counter: 0,
        }
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for sample in samples.iter_mut() {
            if sample.abs() >= self.threshold {
                self.hold_counter = self.hold_samples;
            } else if self.hold_counter > 0 {
                self.hold_counter -= 1;
            } else {
                *sample = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_silence_is_zeroed() {
        let mut gate = NoiseGate::new(0.01, 0);
        let mut samples = vec![0.005, -0.005, 0.001, -0.001];
        gate.process(&mut samples);
        assert!(samples.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn test_loud_signal_passes_through() {
        let mut gate = NoiseGate::new(0.01, 0);
        let original = vec![0.5, -0.3, 0.8, -0.1];
        let mut samples = original.clone();
        gate.process(&mut samples);
        assert_eq!(samples, original);
    }

    #[test]
    fn test_hold_time_keeps_gate_open() {
        let mut gate = NoiseGate::new(0.01, 3);
        // Loud sample followed by quiet samples within hold time
        let mut samples = vec![0.5, 0.001, 0.001, 0.001, 0.001];
        gate.process(&mut samples);
        // First sample is loud, gate opens with hold_counter=3
        // Next 3 quiet samples pass through (hold counting down)
        // 5th sample: hold expired, zeroed
        assert_eq!(samples[0], 0.5);
        assert_eq!(samples[1], 0.001);
        assert_eq!(samples[2], 0.001);
        assert_eq!(samples[3], 0.001);
        assert_eq!(samples[4], 0.0);
    }

    #[test]
    fn test_hold_resets_on_new_loud_sample() {
        let mut gate = NoiseGate::new(0.01, 2);
        let mut samples = vec![0.5, 0.001, 0.5, 0.001, 0.001, 0.001];
        gate.process(&mut samples);
        // [0] loud: hold=2
        // [1] quiet: hold=1, passes
        // [2] loud: hold=2 (reset)
        // [3] quiet: hold=1, passes
        // [4] quiet: hold=0, passes (hold just hit 0)
        // [5] quiet: hold=0, zeroed
        assert_eq!(samples[0], 0.5);
        assert_eq!(samples[1], 0.001);
        assert_eq!(samples[2], 0.5);
        assert_eq!(samples[3], 0.001);
        assert_eq!(samples[4], 0.001);
        assert_eq!(samples[5], 0.0);
    }
}
