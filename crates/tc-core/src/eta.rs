//! Transfer speed and ETA from a weighted linear fit over recent (time, bytes) samples.

use std::collections::VecDeque;

const MAX_SAMPLES: usize = 60;
/// At most ~4 samples per second.
const MIN_GAP: f64 = 0.25;
/// Weight multiplier per sample of age: newer samples dominate, the rate adapts in a few seconds.
const DECAY: f64 = 0.9;

#[derive(Default)]
pub struct Estimator {
    samples: VecDeque<(f64, f64)>,
}

impl Estimator {
    /// Record `bytes` done at `secs` since start; samples closer than 0.25 s are dropped.
    pub fn push(&mut self, secs: f64, bytes: f64) {
        if self
            .samples
            .back()
            .is_some_and(|&(t, _)| secs - t < MIN_GAP)
        {
            return;
        }
        if self.samples.len() == MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back((secs, bytes));
    }

    /// Bytes per second, `None` without at least two distinct times or with a rate <= 0.
    pub fn rate(&self) -> Option<f64> {
        let n = self.samples.len();
        let t0 = self.samples.front()?.0;
        let (mut sw, mut st, mut sb, mut stt, mut stb) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (i, &(t, b)) in self.samples.iter().enumerate() {
            let (w, t) = (DECAY.powi((n - 1 - i) as i32), t - t0);
            sw += w;
            st += w * t;
            sb += w * b;
            stt += w * t * t;
            stb += w * t * b;
        }
        let var = stt / sw - (st / sw).powi(2);
        let slope = (stb / sw - st / sw * (sb / sw)) / var;
        (n >= 2 && var > 1e-9 && slope > 0.0 && slope.is_finite()).then_some(slope)
    }

    /// Seconds until `remaining` bytes are done at the current rate.
    pub fn eta(&self, remaining: f64) -> Option<f64> {
        self.rate().map(|r| remaining.max(0.0) / r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(e: &mut Estimator, from: usize, to: usize, rate: f64, base: f64) {
        for i in from..to {
            let t = i as f64 * 0.25;
            e.push(t, base + rate * (t - from as f64 * 0.25));
        }
    }

    #[test]
    fn steady_rate_gives_exact_eta() {
        let mut e = Estimator::default();
        feed(&mut e, 0, 40, 1000.0, 0.0);
        assert!((e.rate().unwrap() - 1000.0).abs() < 1e-6);
        assert!((e.eta(5000.0).unwrap() - 5.0).abs() < 1e-6);
    }

    #[test]
    fn adapts_to_new_rate() {
        let mut e = Estimator::default();
        feed(&mut e, 0, 40, 1000.0, 0.0);
        feed(&mut e, 40, 60, 4000.0, 9750.0);
        let r = e.rate().unwrap();
        assert!(r > 2500.0 && r < 4000.0, "{r}");
    }

    #[test]
    fn degenerate_inputs_give_none() {
        let mut e = Estimator::default();
        assert!(e.rate().is_none());
        e.push(0.0, 0.0);
        assert!(e.rate().is_none());
        e.push(0.1, 10.0); // too close, dropped
        assert!(e.rate().is_none());
        e.push(1.0, 0.0); // flat
        assert!(e.rate().is_none() && e.eta(10.0).is_none());
        e.push(2.0, -5.0); // falling
        assert!(e.rate().is_none());
    }

    #[test]
    fn ring_is_bounded() {
        let mut e = Estimator::default();
        feed(&mut e, 0, 500, 10.0, 0.0);
        assert_eq!(e.samples.len(), MAX_SAMPLES);
    }
}
