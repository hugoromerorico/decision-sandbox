/**
 * A port of CPython's `random.Random` (Mersenne Twister) and the float helpers the
 * generator uses, so a seed produces the same answers as the Python Worker.
 */

const N = 624;
const M = 397;

/** `random.Random(int.from_bytes(digest, "big"))` for a SHA-256 digest. */
export class PyRandom {
  private readonly mt = new Uint32Array(N);
  private mti = N + 1;

  constructor(digest: Uint8Array) {
    // CPython splits abs(n) into 32-bit words, least significant first, and drops the
    // leading zero words (keeping at least one).
    const key: number[] = [];
    for (let end = digest.length; end > 0; end -= 4) {
      const start = Math.max(0, end - 4);
      let word = 0;
      for (let i = start; i < end; i++) word = (word * 256 + (digest[i] as number)) >>> 0;
      key.push(word);
    }
    while (key.length > 1 && key[key.length - 1] === 0) key.pop();
    this.initByArray(key);
  }

  private initGenrand(seed: number): void {
    const mt = this.mt;
    mt[0] = seed >>> 0;
    for (let i = 1; i < N; i++) {
      const prev = (mt[i - 1] as number) ^ ((mt[i - 1] as number) >>> 30);
      mt[i] = (Math.imul(1812433253, prev) + i) >>> 0;
    }
    this.mti = N;
  }

  private initByArray(key: number[]): void {
    const mt = this.mt;
    this.initGenrand(19650218);
    let i = 1;
    let j = 0;
    for (let k = Math.max(N, key.length); k; k--) {
      const prev = (mt[i - 1] as number) ^ ((mt[i - 1] as number) >>> 30);
      mt[i] = (((mt[i] as number) ^ Math.imul(prev, 1664525)) + (key[j] as number) + j) >>> 0;
      i++;
      j++;
      if (i >= N) {
        mt[0] = mt[N - 1] as number;
        i = 1;
      }
      if (j >= key.length) j = 0;
    }
    for (let k = N - 1; k; k--) {
      const prev = (mt[i - 1] as number) ^ ((mt[i - 1] as number) >>> 30);
      mt[i] = (((mt[i] as number) ^ Math.imul(prev, 1566083941)) - i) >>> 0;
      i++;
      if (i >= N) {
        mt[0] = mt[N - 1] as number;
        i = 1;
      }
    }
    mt[0] = 0x80000000;
  }

  private genrandUint32(): number {
    const mt = this.mt;
    if (this.mti >= N) {
      let kk = 0;
      let y: number;
      for (; kk < N - M; kk++) {
        y = ((mt[kk] as number) & 0x80000000) | ((mt[kk + 1] as number) & 0x7fffffff);
        mt[kk] = (mt[kk + M] as number) ^ (y >>> 1) ^ (y & 1 ? 0x9908b0df : 0);
      }
      for (; kk < N - 1; kk++) {
        y = ((mt[kk] as number) & 0x80000000) | ((mt[kk + 1] as number) & 0x7fffffff);
        mt[kk] = (mt[kk + (M - N)] as number) ^ (y >>> 1) ^ (y & 1 ? 0x9908b0df : 0);
      }
      y = ((mt[N - 1] as number) & 0x80000000) | ((mt[0] as number) & 0x7fffffff);
      mt[N - 1] = (mt[M - 1] as number) ^ (y >>> 1) ^ (y & 1 ? 0x9908b0df : 0);
      this.mti = 0;
    }
    let y = mt[this.mti++] as number;
    y ^= y >>> 11;
    y ^= (y << 7) & 0x9d2c5680;
    y ^= (y << 15) & 0xefc60000;
    y ^= y >>> 18;
    return y >>> 0;
  }

  /** A float in [0, 1) with 53 random bits. */
  random(): number {
    const a = this.genrandUint32() >>> 5;
    const b = this.genrandUint32() >>> 6;
    return (a * 67108864.0 + b) * (1.0 / 9007199254740992.0);
  }

  uniform(a: number, b: number): number {
    return a + (b - a) * this.random();
  }

  /** `randrange(n)` for 0 < n <= 2**32. */
  randrange(n: number): number {
    const k = 32 - Math.clz32(n);
    let r = this.genrandUint32() >>> (32 - k);
    while (r >= n) r = this.genrandUint32() >>> (32 - k);
    return r;
  }

  gammavariate(alpha: number, beta: number): number {
    if (alpha > 1.0) {
      const ainv = Math.sqrt(2.0 * alpha - 1.0);
      const bbb = alpha - LOG4;
      const ccc = alpha + ainv;
      for (;;) {
        const u1 = this.random();
        if (!(1e-7 < u1 && u1 < 0.9999999)) continue;
        const u2 = 1.0 - this.random();
        const v = Math.log(u1 / (1.0 - u1)) / ainv;
        const x = alpha * Math.exp(v);
        const z = u1 * u1 * u2;
        const r = bbb + ccc * v - x;
        if (r + SG_MAGICCONST - 4.5 * z >= 0.0 || r >= Math.log(z)) return x * beta;
      }
    }
    if (alpha === 1.0) return -Math.log(1.0 - this.random()) * beta;
    let x: number;
    for (;;) {
      const u = this.random();
      const b = (Math.E + alpha) / Math.E;
      const p = b * u;
      x = p <= 1.0 ? Math.pow(p, 1.0 / alpha) : -Math.log((b - p) / alpha);
      const u1 = this.random();
      if (p > 1.0) {
        if (u1 <= Math.pow(x, alpha - 1.0)) break;
      } else if (u1 <= Math.exp(-x)) break;
    }
    return x * beta;
  }

  betavariate(alpha: number, beta: number): number {
    const y = this.gammavariate(alpha, 1.0);
    return y ? y / (y + this.gammavariate(beta, 1.0)) : 0.0;
  }
}

/** random.LOG4 and random.SG_MAGICCONST, as CPython computes them. */
const LOG4 = 1.3862943611198906;
const SG_MAGICCONST = 2.504077396776274;

/** `sum()` of floats: CPython 3.12+ uses Neumaier compensated summation. */
export function pySum(xs: number[]): number {
  let total = 0.0;
  let c = 0.0;
  for (const x of xs) {
    const t = total + x;
    if (Math.abs(total) >= Math.abs(x)) c += total - t + x;
    else c += x - t + total;
    total = t;
  }
  if (c && Number.isFinite(c)) total += c;
  return total;
}

/**
 * `round(x, ndigits)` for x >= 0: the correctly rounded decimal, ties to even.
 * toFixed rounds exact ties up, so an exact tie on an odd digit is pulled back down.
 */
export function pyRound(x: number, ndigits: number): number {
  const fixed = x.toFixed(ndigits);
  // 40 more digits are exact enough to tell a tie from its neighbouring doubles.
  const long = x.toFixed(ndigits + 40);
  const tail = long.slice(long.indexOf(".") + 1 + ndigits);
  if (!/^50*$/.test(tail)) return Number(fixed);
  const units = BigInt(fixed.replace(".", ""));
  const even = units % 2n === 0n ? units : units - 1n;
  return Number(`${even}e-${ndigits}`);
}
