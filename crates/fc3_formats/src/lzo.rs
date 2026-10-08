//! Safe LZO1X decompressor (the format produced by lzo1x_999 / lzo1x_1).

use anyhow::{bail, Result};

struct Reader<'a> {
    src: &'a [u8],
    ip: usize,
}

impl<'a> Reader<'a> {
    #[inline]
    fn byte(&mut self) -> Result<usize> {
        match self.src.get(self.ip) {
            Some(&b) => {
                self.ip += 1;
                Ok(b as usize)
            }
            None => bail!("lzo: input overrun"),
        }
    }

    #[inline]
    fn le16(&mut self) -> Result<usize> {
        let lo = self.byte()?;
        let hi = self.byte()?;
        Ok(lo | (hi << 8))
    }

    /// Reads a run-length extension: zero bytes add 255 each, then a final byte.
    #[inline]
    fn extended(&mut self, base: usize) -> Result<usize> {
        let mut t = 0usize;
        loop {
            let b = self.byte()?;
            if b == 0 {
                t += 255;
            } else {
                return Ok(t + base + b);
            }
        }
    }

    #[inline]
    fn prev(&self, back: usize) -> u8 {
        self.src[self.ip - back]
    }
}

#[inline]
fn copy_literals(r: &mut Reader, out: &mut Vec<u8>, n: usize, cap: usize) -> Result<()> {
    if r.ip + n > r.src.len() {
        bail!("lzo: literal input overrun");
    }
    if out.len() + n > cap {
        bail!("lzo: output overrun");
    }
    out.extend_from_slice(&r.src[r.ip..r.ip + n]);
    r.ip += n;
    Ok(())
}

#[inline]
fn copy_match(out: &mut Vec<u8>, distance: usize, n: usize, cap: usize) -> Result<()> {
    if distance == 0 || distance > out.len() {
        bail!("lzo: lookbehind overrun (distance {distance}, have {})", out.len());
    }
    if out.len() + n > cap {
        bail!("lzo: output overrun");
    }
    let start = out.len() - distance;
    if distance >= n {
        out.extend_from_within(start..start + n);
    } else {
        for i in 0..n {
            let b = out[start + i];
            out.push(b);
        }
    }
    Ok(())
}

/// Decompresses `src`, expecting exactly `expected_len` bytes of output.
pub fn decompress(src: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected_len);
    let cap = expected_len;
    let mut r = Reader { src, ip: 0 };

    // State machine labels from the reference implementation.
    enum State {
        Outer,
        FirstLiteralRun,
        Match(usize),
        MatchNext(usize),
    }

    let mut state = if src.first().copied().unwrap_or(0) > 17 {
        let t = r.byte()? - 17;
        if t < 4 {
            State::MatchNext(t)
        } else {
            copy_literals(&mut r, &mut out, t, cap)?;
            State::FirstLiteralRun
        }
    } else {
        State::Outer
    };

    loop {
        match state {
            State::Outer => {
                let mut t = r.byte()?;
                if t >= 16 {
                    state = State::Match(t);
                    continue;
                }
                if t == 0 {
                    t = r.extended(15)?;
                }
                copy_literals(&mut r, &mut out, t + 3, cap)?;
                state = State::FirstLiteralRun;
            }
            State::FirstLiteralRun => {
                let t = r.byte()?;
                if t >= 16 {
                    state = State::Match(t);
                    continue;
                }
                let dist = 1 + 0x0800 + (t >> 2) + (r.byte()? << 2);
                copy_match(&mut out, dist, 3, cap)?;
                let t = (r.prev(2) & 3) as usize;
                state = if t == 0 { State::Outer } else { State::MatchNext(t) };
            }
            State::Match(t) => {
                if t >= 64 {
                    let dist = 1 + ((t >> 2) & 7) + (r.byte()? << 3);
                    let len = (t >> 5) - 1 + 2;
                    copy_match(&mut out, dist, len, cap)?;
                } else if t >= 32 {
                    let mut len = t & 31;
                    if len == 0 {
                        len = r.extended(31)?;
                    }
                    let dist = 1 + (r.le16()? >> 2);
                    copy_match(&mut out, dist, len + 2, cap)?;
                } else if t >= 16 {
                    let high = (t & 8) << 11;
                    let mut len = t & 7;
                    if len == 0 {
                        len = r.extended(7)?;
                    }
                    let low = r.le16()? >> 2;
                    let dist = high + low;
                    if dist == 0 {
                        // End of stream marker.
                        if out.len() != expected_len {
                            bail!("lzo: size mismatch ({} != {expected_len})", out.len());
                        }
                        return Ok(out);
                    }
                    copy_match(&mut out, dist + 0x4000, len + 2, cap)?;
                } else {
                    let dist = 1 + (t >> 2) + (r.byte()? << 2);
                    copy_match(&mut out, dist, 2, cap)?;
                }
                let t = (r.prev(2) & 3) as usize;
                state = if t == 0 { State::Outer } else { State::MatchNext(t) };
            }
            State::MatchNext(t) => {
                copy_literals(&mut r, &mut out, t, cap)?;
                state = State::Match(r.byte()?);
            }
        }
    }
}
