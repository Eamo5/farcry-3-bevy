//! Values read out of the game's own engine code (`bin\FC3.dll`, a 32-bit
//! PE) at conversion, for behaviour the data files don't carry: the lights'
//! flicker wave and the intensity below which an effect's light is off. Only what's needed is decoded: sections, the
//! import table, and the few instruction forms of the routines looked for.

use std::collections::HashMap;

use crate::lights::{FlickerWave, Trig};

/// Where the engine's code sits, from the game's data folder.
pub fn dll_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.parent().unwrap_or(data_dir).join("bin").join("FC3.dll")
}

struct Section {
    va: u32,
    vsize: u32,
    raw: u32,
    raw_size: u32,
    exec: bool,
}

/// A loaded 32-bit PE image's file bytes, addressed by virtual address.
pub struct Pe<'a> {
    data: &'a [u8],
    sections: Vec<Section>,
    /// Import address table slot -> imported function name.
    imports: HashMap<u32, String>,
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

impl<'a> Pe<'a> {
    pub fn parse(data: &'a [u8]) -> Option<Pe<'a>> {
        let pe = u32_at(data, 0x3c)? as usize;
        if data.get(pe..pe + 4)? != b"PE\0\0" {
            return None;
        }
        let sections_n = u16_at(data, pe + 6)? as usize;
        let opt = pe + 24;
        let opt_size = u16_at(data, pe + 20)? as usize;
        if u16_at(data, opt)? != 0x10b {
            return None;
        }
        let base = u32_at(data, opt + 28)?;
        let mut sections = Vec::new();
        for i in 0..sections_n {
            let s = opt + opt_size + i * 40;
            sections.push(Section {
                vsize: u32_at(data, s + 8)?,
                va: base.wrapping_add(u32_at(data, s + 12)?),
                raw_size: u32_at(data, s + 16)?,
                raw: u32_at(data, s + 20)?,
                exec: u32_at(data, s + 36)? & 0x2000_0000 != 0,
            });
        }
        let mut pe = Pe { data, sections, imports: HashMap::new() };
        let import_rva = u32_at(data, opt + 96 + 8)?;
        let mut desc = base.wrapping_add(import_rva);
        loop {
            let (Some(lookup), Some(first)) = (pe.u32_va(desc), pe.u32_va(desc + 16)) else { break };
            if first == 0 {
                break;
            }
            let names = if lookup != 0 { lookup } else { first };
            for i in 0.. {
                let Some(entry) = pe.u32_va(base.wrapping_add(names) + 4 * i) else { break };
                if entry == 0 {
                    break;
                }
                if entry & 0x8000_0000 == 0 {
                    if let Some(name) = pe.cstr_va(base.wrapping_add(entry) + 2) {
                        pe.imports.insert(base.wrapping_add(first) + 4 * i, name);
                    }
                }
            }
            desc += 20;
        }
        Some(pe)
    }

    fn offset(&self, va: u32) -> Option<usize> {
        let s = self.sections.iter().find(|s| va >= s.va && va - s.va < s.vsize.max(s.raw_size))?;
        let o = va - s.va;
        (o < s.raw_size).then(|| (s.raw + o) as usize)
    }

    fn u32_va(&self, va: u32) -> Option<u32> {
        u32_at(self.data, self.offset(va)?)
    }

    pub fn f32_va(&self, va: u32) -> Option<f32> {
        self.u32_va(va).map(f32::from_bits)
    }

    fn cstr_va(&self, va: u32) -> Option<String> {
        let o = self.offset(va)?;
        let end = self.data[o..].iter().take(256).position(|&b| b == 0)?;
        std::str::from_utf8(&self.data[o..o + end]).ok().map(str::to_string)
    }

    /// The imported function a call lands on, through its `jmp [slot]` thunk.
    fn called_import(&self, target: u32) -> Option<&str> {
        let o = self.offset(target)?;
        (self.data.get(o..o + 2)? == [0xff, 0x25]).then_some(())?;
        self.imports.get(&u32_at(self.data, o + 2)?).map(String::as_str)
    }

    /// Every place in the code whose bytes start with `pattern`.
    fn find_code(&self, pattern: &[u8]) -> Vec<u32> {
        let mut out = Vec::new();
        for s in self.sections.iter().filter(|s| s.exec) {
            let raw = &self.data[s.raw as usize..(s.raw + s.raw_size) as usize];
            out.extend(raw.windows(pattern.len()).enumerate().filter(|(_, w)| *w == pattern).map(|(i, _)| s.va + i as u32));
        }
        out
    }

    /// Every place in the code matching a hex pattern (`??` for any byte).
    fn find_masked(&self, pattern: &str) -> Vec<u32> {
        let pat: Vec<Option<u8>> = pattern.split_whitespace().map(|b| u8::from_str_radix(b, 16).ok()).collect();
        let mut out = Vec::new();
        for s in self.sections.iter().filter(|s| s.exec) {
            let raw = &self.data[s.raw as usize..(s.raw + s.raw_size) as usize];
            out.extend(
                raw.windows(pat.len()).enumerate().filter(|(_, w)| w.iter().zip(&pat).all(|(b, p)| p.is_none_or(|p| p == *b))).map(|(i, _)| s.va + i as u32),
            );
        }
        out
    }
}

/// The intensity below which the engine leaves a particle effect's light off:
/// its light update multiplies the fades and flicker into the emitter's
/// intensity and compares the product with this before lighting.
pub fn effect_light_cutoff(pe: &Pe) -> Option<f32> {
    // mulss xmm1, [esp+d8]; mulss xmm1, [eax+d32]; comiss xmm1, [abs]; jb; movss xmm2, [eax+d32]
    const PATTERN: &str = "f3 0f 59 4c 24 ?? f3 0f 59 88 ?? ?? ?? ?? 0f 2f 0d ?? ?? ?? ?? 72 ?? f3 0f 10 90";
    let found: Vec<f32> = pe
        .find_masked(PATTERN)
        .into_iter()
        .filter_map(|at| {
            let o = pe.offset(at + 17)?;
            pe.f32_va(u32_at(pe.data, o)?)
        })
        .collect();
    (found.len() == 1).then(|| found[0])
}

/// The light component's flicker wave, read out of its routine: time
/// (a double) to float, times a constant and the frequency; then each term's
/// multiplier and its sine or cosine, summed; the sum times a constant and
/// the noise setting; the main term added; that times a constant and the
/// amplitude.
pub fn flicker_wave(pe: &Pe) -> Option<FlickerWave> {
    // movsd xmm0, [eax+0x30]; cvtpd2ps xmm0, xmm0; mulss xmm0, [abs]
    const START: [u8; 13] = [0xf2, 0x0f, 0x10, 0x40, 0x30, 0x66, 0x0f, 0x5a, 0xc0, 0xf3, 0x0f, 0x59, 0x05];
    pe.find_code(&START).into_iter().find_map(|at| decode_flicker(pe, at))
}

fn decode_flicker(pe: &Pe, at: u32) -> Option<FlickerWave> {
    let code = |va: u32, n: usize| -> Option<&[u8]> {
        let o = pe.offset(va)?;
        pe.data.get(o..o + n)
    };
    let mut pc = at + 13;
    let time_scale = pe.f32_va(u32_at(code(pc, 4)?, 0)?)?;
    pc += 4;
    // mulss xmm0, xmm1 (the frequency); the scaled time kept twice.
    if code(pc, 4)? != [0xf3, 0x0f, 0x59, 0xc1] {
        return None;
    }
    pc += 4;
    let (mut terms, mut main, mut noise_scale, mut amplitude_scale) = (Vec::new(), None, None, None);
    let mut multiplier: Option<f32> = None;
    let mut xmm1_const: Option<f32> = None;
    for _ in 0..64 {
        let b = code(pc, 9)?;
        match b {
            // mulss xmm0, [abs]: a term's multiplier.
            [0xf3, 0x0f, 0x59, 0x05, ..] => {
                multiplier = Some(pe.f32_va(u32_at(b, 4)?)?);
                pc += 8;
            }
            // mulss xmm1, [abs]: the noise or the amplitude scale.
            [0xf3, 0x0f, 0x59, 0x0d, ..] => {
                xmm1_const = Some(pe.f32_va(u32_at(b, 4)?)?);
                pc += 8;
            }
            // mulss xmm1, [esi+disp8]: the noise (0xc) or amplitude (8) setting.
            [0xf3, 0x0f, 0x59, 0x4e, disp, ..] => {
                let scale = xmm1_const.take()?;
                match disp {
                    0x0c => noise_scale = Some(scale),
                    0x08 => amplitude_scale = Some(scale),
                    _ => return None,
                }
                pc += 5;
                if amplitude_scale.is_some() {
                    break;
                }
            }
            // call: sinf or cosf of the term (no multiplier: the main term).
            [0xe8, ..] => {
                let target = (pc + 5).wrapping_add(u32_at(b, 1)?);
                let trig = match pe.called_import(target)? {
                    n if n.contains("sin") => Trig::Sin,
                    n if n.contains("cos") => Trig::Cos,
                    _ => return None,
                };
                match multiplier.take() {
                    Some(k) => terms.push((trig, k)),
                    None => main = Some(trig),
                }
                pc += 5;
            }
            // movss to or from [esp+disp8], addss xmm1, xmm0: bookkeeping.
            [0xf3, 0x0f, 0x10 | 0x11, 0x44 | 0x4c, 0x24, ..] => pc += 6,
            [0xf3, 0x0f, 0x58, 0xc8, ..] => pc += 4,
            _ => return None,
        }
    }
    let effect_cutoff = effect_light_cutoff(pe).unwrap_or(0.0);
    let wave = FlickerWave { time_scale, main: main?, terms, noise_scale: noise_scale?, amplitude_scale: amplitude_scale?, effect_cutoff };
    (wave.terms.len() >= 2).then_some(wave)
}
