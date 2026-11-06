//! A port of CPython's `random.Random` (Mersenne Twister) and the float helpers the
//! generator uses, so a seed produces the same answers as the Python Worker.

const N: usize = 624;
const M: usize = 397;

/// random.LOG4 and random.SG_MAGICCONST, as CPython computes them.
const LOG4: f64 = 1.3862943611198906;
const SG_MAGICCONST: f64 = 2.504077396776274;

pub struct PyRandom {
    mt: [u32; N],
    mti: usize,
}

impl PyRandom {
    /// `random.Random(int.from_bytes(digest, "big"))` for a SHA-256 digest.
    pub fn from_digest(digest: &[u8]) -> Self {
        // CPython splits abs(n) into 32-bit words, least significant first, and drops
        // the leading zero words (keeping at least one).
        let mut key: Vec<u32> = digest
            .rchunks(4)
            .map(|chunk| chunk.iter().fold(0u32, |w, &b| (w << 8) | b as u32))
            .collect();
        while key.len() > 1 && key.last() == Some(&0) {
            key.pop();
        }
        let mut rng = PyRandom { mt: [0; N], mti: N + 1 };
        rng.init_by_array(&key);
        rng
    }

    fn init_genrand(&mut self, seed: u32) {
        self.mt[0] = seed;
        for i in 1..N {
            let prev = self.mt[i - 1] ^ (self.mt[i - 1] >> 30);
            self.mt[i] = 1812433253u32.wrapping_mul(prev).wrapping_add(i as u32);
        }
        self.mti = N;
    }

    fn init_by_array(&mut self, key: &[u32]) {
        self.init_genrand(19650218);
        let mt = &mut self.mt;
        let (mut i, mut j) = (1usize, 0usize);
        for _ in 0..N.max(key.len()) {
            let prev = mt[i - 1] ^ (mt[i - 1] >> 30);
            mt[i] = (mt[i] ^ prev.wrapping_mul(1664525)).wrapping_add(key[j]).wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                mt[0] = mt[N - 1];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
        }
        for _ in 0..N - 1 {
            let prev = mt[i - 1] ^ (mt[i - 1] >> 30);
            mt[i] = (mt[i] ^ prev.wrapping_mul(1566083941)).wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                mt[0] = mt[N - 1];
                i = 1;
            }
        }
        mt[0] = 0x80000000;
    }

    fn genrand_u32(&mut self) -> u32 {
        const MAG01: [u32; 2] = [0, 0x9908b0df];
        let mt = &mut self.mt;
        if self.mti >= N {
            for kk in 0..N - M {
                let y = (mt[kk] & 0x80000000) | (mt[kk + 1] & 0x7fffffff);
                mt[kk] = mt[kk + M] ^ (y >> 1) ^ MAG01[(y & 1) as usize];
            }
            for kk in N - M..N - 1 {
                let y = (mt[kk] & 0x80000000) | (mt[kk + 1] & 0x7fffffff);
                mt[kk] = mt[kk + M - N] ^ (y >> 1) ^ MAG01[(y & 1) as usize];
            }
            let y = (mt[N - 1] & 0x80000000) | (mt[0] & 0x7fffffff);
            mt[N - 1] = mt[M - 1] ^ (y >> 1) ^ MAG01[(y & 1) as usize];
            self.mti = 0;
        }
        let mut y = mt[self.mti];
        self.mti += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c5680;
        y ^= (y << 15) & 0xefc60000;
        y ^= y >> 18;
        y
    }

    /// A float in [0, 1) with 53 random bits.
    pub fn random(&mut self) -> f64 {
        let a = (self.genrand_u32() >> 5) as f64;
        let b = (self.genrand_u32() >> 6) as f64;
        (a * 67108864.0 + b) * (1.0 / 9007199254740992.0)
    }

    pub fn uniform(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.random()
    }

    /// `randrange(n)` for 0 < n <= 2**32.
    pub fn randrange(&mut self, n: usize) -> usize {
        let k = usize::BITS - n.leading_zeros();
        loop {
            let r = (self.genrand_u32() >> (32 - k)) as usize;
            if r < n {
                return r;
            }
        }
    }

    pub fn gammavariate(&mut self, alpha: f64, beta: f64) -> f64 {
        if alpha > 1.0 {
            let ainv = (2.0 * alpha - 1.0).sqrt();
            let bbb = alpha - LOG4;
            let ccc = alpha + ainv;
            loop {
                let u1 = self.random();
                if !(1e-7 < u1 && u1 < 0.9999999) {
                    continue;
                }
                let u2 = 1.0 - self.random();
                let v = (u1 / (1.0 - u1)).ln() / ainv;
                let x = alpha * v.exp();
                let z = u1 * u1 * u2;
                let r = bbb + ccc * v - x;
                if r + SG_MAGICCONST - 4.5 * z >= 0.0 || r >= z.ln() {
                    return x * beta;
                }
            }
        }
        if alpha == 1.0 {
            return -(1.0 - self.random()).ln() * beta;
        }
        let e = std::f64::consts::E;
        loop {
            let u = self.random();
            let b = (e + alpha) / e;
            let p = b * u;
            let x = if p <= 1.0 { p.powf(1.0 / alpha) } else { -((b - p) / alpha).ln() };
            let u1 = self.random();
            let accept = if p > 1.0 { u1 <= x.powf(alpha - 1.0) } else { u1 <= (-x).exp() };
            if accept {
                return x * beta;
            }
        }
    }

    pub fn betavariate(&mut self, alpha: f64, beta: f64) -> f64 {
        let y = self.gammavariate(alpha, 1.0);
        if y != 0.0 {
            y / (y + self.gammavariate(beta, 1.0))
        } else {
            0.0
        }
    }
}

/// `sum()` of floats: CPython 3.12+ uses Neumaier compensated summation.
pub fn py_sum(xs: impl IntoIterator<Item = f64>) -> f64 {
    let (mut total, mut c) = (0.0f64, 0.0f64);
    for x in xs {
        let t = total + x;
        if total.abs() >= x.abs() {
            c += (total - t) + x;
        } else {
            c += (x - t) + total;
        }
        total = t;
    }
    if c != 0.0 && c.is_finite() {
        total += c;
    }
    total
}

/// `round(x, 4)`: Rust's fixed-precision formatting is exact with ties to even, like
/// CPython's correctly rounded `round()`.
pub fn py_round4(x: f64) -> f64 {
    format!("{x:.4}").parse().expect("formatted float parses")
}
