//! The inverse transforms Bink audio needs: an inverse real DFT and a DCT-III, both on power-of-two
//! lengths and built on one radix-2 complex FFT.

use std::f64::consts::PI;

/// Unnormalised inverse complex FFT, `x[m] = sum X[k] e^(2 pi i k m / n)`.
struct Fft {
    twiddles: Vec<(f32, f32)>,
    bit_reverse: Vec<u32>,
}

impl Fft {
    fn new(n: usize) -> Self {
        assert!(n.is_power_of_two(), "FFT length {n} is not a power of two");
        let bits = n.trailing_zeros();
        let bit_reverse = (0..n as u32).map(|i| if bits == 0 { 0 } else { i.reverse_bits() >> (32 - bits) }).collect();
        let twiddles = (0..n / 2)
            .map(|k| {
                let a = 2.0 * PI * k as f64 / n as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .collect();
        Self { twiddles, bit_reverse }
    }

    fn run(&self, re: &mut [f32], im: &mut [f32]) {
        let n = self.bit_reverse.len();
        for (i, &j) in self.bit_reverse.iter().enumerate() {
            let j = j as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let (wr, wi) = self.twiddles[k * step];
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len *= 2;
        }
    }
}

/// Unnormalised inverse real DFT of length `n`: from bins `0..=n/2` of a Hermitian spectrum to `n` real
/// samples, through a complex FFT of half the length.
pub(crate) struct RealInverse {
    fft: Fft,
    /// `e^(2 pi i k / n)` for `k < n/2`.
    twiddles: Vec<(f32, f32)>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl RealInverse {
    pub fn new(n: usize) -> Self {
        let half = n / 2;
        let twiddles = (0..half)
            .map(|k| {
                let a = 2.0 * PI * k as f64 / n as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .collect();
        Self { fft: Fft::new(half), twiddles, re: vec![0.0; half], im: vec![0.0; half] }
    }

    /// `spec_re` and `spec_im` hold bins `0..=n/2`; `out` receives `n` samples.
    pub fn run(&mut self, spec_re: &[f32], spec_im: &[f32], out: &mut [f32]) {
        let half = self.re.len();
        // Even samples come from the folded spectrum, odd ones from its twisted difference; pack them as
        // the real and imaginary parts of one half-length transform.
        for k in 0..half {
            let (ar, ai) = (spec_re[k], spec_im[k]);
            let (br, bi) = (spec_re[half - k], -spec_im[half - k]);
            let (dr, di) = (ar - br, ai - bi);
            let (wr, wi) = self.twiddles[k];
            let (or, oi) = (dr * wr - di * wi, dr * wi + di * wr);
            self.re[k] = ar + br - oi;
            self.im[k] = ai + bi + or;
        }
        self.fft.run(&mut self.re, &mut self.im);
        for (pair, (&r, &i)) in out.chunks_exact_mut(2).zip(self.re.iter().zip(&self.im)) {
            pair[0] = r;
            pair[1] = i;
        }
    }
}

/// DCT-III of length `n`: `y[m] = x[0]/2 + sum_{k>=1} x[k] cos(pi k (2m + 1) / 2n)`, through a real
/// inverse DFT of the same length.
pub(crate) struct Dct3 {
    inverse: RealInverse,
    /// `e^(i pi k / 2n)` for `k <= n/2`.
    rotations: Vec<(f32, f32)>,
    spec_re: Vec<f32>,
    spec_im: Vec<f32>,
    folded: Vec<f32>,
}

impl Dct3 {
    pub fn new(n: usize) -> Self {
        let rotations = (0..=n / 2)
            .map(|k| {
                let a = PI * k as f64 / (2 * n) as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .collect();
        Self { inverse: RealInverse::new(n), rotations, spec_re: vec![0.0; n / 2 + 1], spec_im: vec![0.0; n / 2 + 1], folded: vec![0.0; n] }
    }

    /// Transforms `x` (`n` values) into `out` (`n` samples), multiplying by `scale`.
    pub fn run(&mut self, x: &[f32], out: &mut [f32], scale: f32) {
        let n = self.folded.len();
        // The spectrum of the reordered output: bin k is (x[k] - i x[n-k]) rotated by pi k / 2n.
        self.spec_re[0] = x[0];
        self.spec_im[0] = 0.0;
        for k in 1..=n / 2 {
            let (a, b) = (x[k], -x[n - k]);
            let (c, s) = self.rotations[k];
            self.spec_re[k] = a * c - b * s;
            self.spec_im[k] = a * s + b * c;
        }
        self.inverse.run(&self.spec_re, &self.spec_im, &mut self.folded);
        let scale = scale * 0.5;
        for m in 0..n / 2 {
            out[2 * m] = self.folded[m] * scale;
            out[2 * m + 1] = self.folded[n - 1 - m] * scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(n: usize) -> Vec<f32> {
        (0..n).map(|i| ((i * 7919 % 101) as f32 - 50.0) / 50.0).collect()
    }

    #[test]
    fn real_inverse_matches_definition() {
        for n in [4, 16, 64, 256] {
            let half = n / 2;
            let (re, mut im) = (signal(half + 1), signal(half + 1).iter().rev().copied().collect::<Vec<_>>());
            im[0] = 0.0;
            im[half] = 0.0;
            let mut out = vec![0.0; n];
            RealInverse::new(n).run(&re, &im, &mut out);
            for (m, &got) in out.iter().enumerate() {
                let mut want = 0.0f64;
                for k in 0..n {
                    let (r, i) = if k <= half { (re[k] as f64, im[k] as f64) } else { (re[n - k] as f64, -im[n - k] as f64) };
                    let a = 2.0 * PI * (k * m) as f64 / n as f64;
                    want += r * a.cos() - i * a.sin();
                }
                assert!((got as f64 - want).abs() < 1e-3, "n {n} m {m}: {got} vs {want}");
            }
        }
    }

    #[test]
    fn dct3_matches_definition() {
        for n in [4, 16, 64, 256] {
            let x = signal(n);
            let mut out = vec![0.0; n];
            Dct3::new(n).run(&x, &mut out, 1.0);
            for (m, &got) in out.iter().enumerate() {
                let mut want = x[0] as f64 / 2.0;
                for (k, &v) in x.iter().enumerate().skip(1) {
                    want += v as f64 * (PI * k as f64 * (2 * m + 1) as f64 / (2 * n) as f64).cos();
                }
                assert!((got as f64 - want).abs() < 1e-3, "n {n} m {m}: {got} vs {want}");
            }
        }
    }
}
