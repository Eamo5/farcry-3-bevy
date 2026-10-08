//! A small CPU interpreter for D3D9 shader model 3 programs (see `d3d9`).
//!
//! The converter uses it to check a candidate vertex/pixel shader pair before
//! committing to it: run the pair on a sample vertex with a material's
//! constants and see whether the outputs are plausible. Semantics follow
//! `d3d9_wgsl` (and so the D3D9 definitions); derivatives are zero and texture
//! reads go through a caller-supplied sampler.

use anyhow::{bail, Result};

use crate::d3d9::{Cmp, Decl, Dst, Instr, Op, Program, RegType, Src, SrcMod, Stage};

pub type Vec4 = [f32; 4];

/// Register files for one run.
pub struct Machine<'a> {
    pub c: [Vec4; 256],
    pub i: [[i32; 4]; 16],
    pub b: [bool; 16],
    /// Inputs (`v#`).
    pub v: [Vec4; 16],
    /// Vertex outputs (`o#`).
    pub o: [Vec4; 16],
    /// Pixel colour outputs (`oC#`).
    pub oc: [Vec4; 4],
    /// Texture lookup: sampler register and coordinate.
    pub sample: &'a dyn Fn(u32, Vec4) -> Vec4,
    /// Set when the program executed `texkill` with a negative coordinate.
    pub killed: bool,
}

impl<'a> Machine<'a> {
    pub fn new(sample: &'a dyn Fn(u32, Vec4) -> Vec4) -> Self {
        Machine { c: [[0.0; 4]; 256], i: [[0; 4]; 16], b: [false; 16], v: [[0.0; 4]; 16], o: [[0.0, 0.0, 0.0, 1.0]; 16], oc: [[0.0; 4]; 4], sample, killed: false }
    }
}

struct State {
    r: [Vec4; 32],
    a0: [i32; 4],
    p0: [bool; 4],
    loops: Vec<i32>,
}

fn cmp(c: Cmp, a: f32, b: f32) -> bool {
    match c {
        Cmp::Gt => a > b,
        Cmp::Eq => a == b,
        Cmp::Ge => a >= b,
        Cmp::Lt => a < b,
        Cmp::Ne => a != b,
        Cmp::Le => a <= b,
    }
}

fn dot3(a: Vec4, b: Vec4) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn dot4(a: Vec4, b: Vec4) -> f32 {
    dot3(a, b) + a[3] * b[3]
}

fn splat(x: f32) -> Vec4 {
    [x; 4]
}

fn map(a: Vec4, f: impl Fn(f32) -> f32) -> Vec4 {
    [f(a[0]), f(a[1]), f(a[2]), f(a[3])]
}

fn zip(a: Vec4, b: Vec4, f: impl Fn(f32, f32) -> f32) -> Vec4 {
    [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2]), f(a[3], b[3])]
}

impl Machine<'_> {
    fn rel_index(&self, st: &State, index: u32, rel: &crate::d3d9::RelAddr) -> i32 {
        let base = match rel.reg_type {
            RegType::Loop => st.loops.last().copied().unwrap_or(0),
            _ => st.a0[rel.component as usize],
        };
        index as i32 + base
    }

    fn read(&self, st: &State, s: &Src) -> Vec4 {
        let raw = match s.reg_type {
            RegType::Temp => st.r[(s.index as usize).min(31)],
            RegType::Input => {
                let i = match &s.relative {
                    Some(rel) => self.rel_index(st, s.index, rel),
                    None => s.index as i32,
                };
                self.v[i.clamp(0, 15) as usize]
            }
            RegType::Const => {
                let i = match &s.relative {
                    Some(rel) => self.rel_index(st, s.index, rel),
                    None => s.index as i32,
                };
                self.c[i.clamp(0, 255) as usize]
            }
            RegType::ConstInt => self.i[(s.index as usize).min(15)].map(|x| x as f32),
            RegType::AddrOrTexture => st.a0.map(|x| x as f32),
            RegType::Loop => splat(st.loops.last().copied().unwrap_or(0) as f32),
            RegType::MiscType => {
                if s.index == 0 {
                    [0.0, 0.0, 0.0, 1.0]
                } else {
                    splat(1.0)
                }
            }
            RegType::Output => self.o[(s.index as usize).min(15)],
            _ => splat(0.0),
        };
        let v = [raw[s.swizzle[0] as usize], raw[s.swizzle[1] as usize], raw[s.swizzle[2] as usize], raw[s.swizzle[3] as usize]];
        match s.modifier {
            SrcMod::Neg => map(v, |x| -x),
            SrcMod::Abs => map(v, f32::abs),
            SrcMod::AbsNeg => map(v, |x| -x.abs()),
            SrcMod::Bias => map(v, |x| x - 0.5),
            SrcMod::BiasNeg => map(v, |x| 0.5 - x),
            SrcMod::Sign => map(v, |x| x * 2.0 - 1.0),
            SrcMod::SignNeg => map(v, |x| 1.0 - x * 2.0),
            SrcMod::Comp => map(v, |x| 1.0 - x),
            SrcMod::X2 => map(v, |x| x * 2.0),
            SrcMod::X2Neg => map(v, |x| x * -2.0),
            _ => v,
        }
    }

    fn cond(&self, st: &State, s: &Src) -> bool {
        let v = match s.reg_type {
            RegType::ConstBool => self.b[(s.index as usize).min(15)],
            RegType::Predicate => st.p0[s.swizzle[0] as usize],
            _ => false,
        };
        if s.modifier == SrcMod::Not {
            !v
        } else {
            v
        }
    }

    fn write(&mut self, i: &Instr, value: Vec4, st: &mut State) {
        let d: &Dst = i.dst.as_ref().unwrap();
        let mut v = value;
        if d.saturate {
            v = map(v, |x| x.clamp(0.0, 1.0));
        }
        for k in 0..4 {
            if d.write_mask & (1 << k) == 0 {
                continue;
            }
            if let Some(p) = &i.predicate {
                let mut on = st.p0[p.swizzle[k] as usize];
                if p.modifier == SrcMod::Not {
                    on = !on;
                }
                if !on {
                    continue;
                }
            }
            let x = v[k];
            match d.reg_type {
                RegType::Temp => st.r[(d.index as usize).min(31)][k] = x,
                RegType::Output => self.o[(d.index as usize).min(15)][k] = x,
                RegType::ColorOut => self.oc[(d.index as usize).min(3)][k] = x,
                RegType::AddrOrTexture => st.a0[k] = x.round() as i32,
                _ => {}
            }
        }
    }
}

/// Matching control-flow partners (`if` -> `else`/`endif`, loops -> end).
fn match_blocks(p: &Program) -> Result<Vec<usize>> {
    let mut partner = vec![usize::MAX; p.instrs.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (pc, i) in p.instrs.iter().enumerate() {
        match i.op {
            Op::If | Op::Ifc(_) | Op::Rep | Op::Loop => stack.push(pc),
            Op::Else => {
                let Some(&open) = stack.last() else { bail!("else without if") };
                partner[open] = pc;
                stack.pop();
                stack.push(pc);
            }
            Op::EndIf | Op::EndRep | Op::EndLoop => {
                let Some(open) = stack.pop() else { bail!("unbalanced block end") };
                partner[open] = pc;
                partner[pc] = open;
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        bail!("unterminated block");
    }
    Ok(partner)
}

/// Runs a program to completion (with an instruction budget against bad loops).
pub fn run(p: &Program, m: &mut Machine) -> Result<()> {
    for i in &p.instrs {
        let Some(d) = i.dst.as_ref() else { continue };
        match &i.op {
            Op::Def(v) if (d.index as usize) < 256 => m.c[d.index as usize] = *v,
            Op::DefI(v) if (d.index as usize) < 16 => m.i[d.index as usize] = *v,
            Op::DefB(b) if (d.index as usize) < 16 => m.b[d.index as usize] = *b,
            _ => {}
        }
    }
    let partner = match_blocks(p)?;
    // Loop frames: (start pc, remaining iterations, aL step).
    let mut frames: Vec<(usize, i32, i32)> = Vec::new();
    let mut st = State { r: [[0.0; 4]; 32], a0: [0; 4], p0: [false; 4], loops: Vec::new() };
    let mut pc = 0;
    let mut budget = 200_000u32;
    while pc < p.instrs.len() {
        budget = budget.checked_sub(1).ok_or_else(|| anyhow::anyhow!("instruction budget exceeded"))?;
        let i = &p.instrs[pc];
        let src = |k: usize, m: &Machine, st: &State| m.read(st, &i.src[k]);
        match &i.op {
            Op::Nop | Op::Dcl(Decl::Plain) | Op::Dcl(_) | Op::Def(_) | Op::DefI(_) | Op::DefB(_) => {}
            Op::Mov | Op::Mova => {
                let a = src(0, m, &st);
                m.write(i, a, &mut st);
            }
            Op::Add => m.write(i, zip(src(0, m, &st), src(1, m, &st), |a, b| a + b), &mut st),
            Op::Sub => m.write(i, zip(src(0, m, &st), src(1, m, &st), |a, b| a - b), &mut st),
            Op::Mul => m.write(i, zip(src(0, m, &st), src(1, m, &st), |a, b| a * b), &mut st),
            Op::Min => m.write(i, zip(src(0, m, &st), src(1, m, &st), f32::min), &mut st),
            Op::Max => m.write(i, zip(src(0, m, &st), src(1, m, &st), f32::max), &mut st),
            Op::Slt => m.write(i, zip(src(0, m, &st), src(1, m, &st), |a, b| if a < b { 1.0 } else { 0.0 }), &mut st),
            Op::Sge => m.write(i, zip(src(0, m, &st), src(1, m, &st), |a, b| if a >= b { 1.0 } else { 0.0 }), &mut st),
            Op::Mad => {
                let (a, b, c) = (src(0, m, &st), src(1, m, &st), src(2, m, &st));
                m.write(i, [a[0] * b[0] + c[0], a[1] * b[1] + c[1], a[2] * b[2] + c[2], a[3] * b[3] + c[3]], &mut st);
            }
            Op::Lrp => {
                let (a, b, c) = (src(0, m, &st), src(1, m, &st), src(2, m, &st));
                m.write(i, [0, 1, 2, 3].map(|k| c[k] + a[k] * (b[k] - c[k])), &mut st);
            }
            Op::Cmp => {
                let (a, b, c) = (src(0, m, &st), src(1, m, &st), src(2, m, &st));
                m.write(i, [0, 1, 2, 3].map(|k| if a[k] >= 0.0 { b[k] } else { c[k] }), &mut st);
            }
            Op::Cnd => {
                let (a, b, c) = (src(0, m, &st), src(1, m, &st), src(2, m, &st));
                m.write(i, [0, 1, 2, 3].map(|k| if a[k] > 0.5 { b[k] } else { c[k] }), &mut st);
            }
            Op::Rcp => m.write(i, splat(1.0 / src(0, m, &st)[0]), &mut st),
            Op::Rsq => m.write(i, splat(1.0 / src(0, m, &st)[0].abs().sqrt()), &mut st),
            Op::Exp | Op::ExpP => m.write(i, splat(src(0, m, &st)[0].exp2()), &mut st),
            Op::Log | Op::LogP => m.write(i, splat(src(0, m, &st)[0].abs().log2()), &mut st),
            Op::Pow => m.write(i, splat(src(0, m, &st)[0].abs().powf(src(1, m, &st)[0])), &mut st),
            Op::Frc => m.write(i, map(src(0, m, &st), |x| x - x.floor()), &mut st),
            Op::Abs => m.write(i, map(src(0, m, &st), f32::abs), &mut st),
            Op::Sgn => m.write(i, map(src(0, m, &st), |x| if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 }), &mut st),
            Op::Dp3 => m.write(i, splat(dot3(src(0, m, &st), src(1, m, &st))), &mut st),
            Op::Dp4 => m.write(i, splat(dot4(src(0, m, &st), src(1, m, &st))), &mut st),
            Op::Dp2Add => {
                let (a, b, c) = (src(0, m, &st), src(1, m, &st), src(2, m, &st));
                m.write(i, splat(a[0] * b[0] + a[1] * b[1] + c[0]), &mut st);
            }
            Op::Nrm => {
                let a = src(0, m, &st);
                let s = 1.0 / dot3(a, a).sqrt();
                m.write(i, map(a, |x| x * s), &mut st);
            }
            Op::Crs => {
                let (a, b) = (src(0, m, &st), src(1, m, &st));
                m.write(i, [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0], 0.0], &mut st);
            }
            Op::SinCos => {
                let a = src(0, m, &st)[0];
                m.write(i, [a.cos(), a.sin(), 0.0, 0.0], &mut st);
            }
            Op::Lit => {
                let a = src(0, m, &st);
                let spec = if a[0] > 0.0 { a[1].max(0.0).powf(a[3].clamp(-128.0, 128.0)) } else { 0.0 };
                m.write(i, [1.0, a[0].max(0.0), spec, 1.0], &mut st);
            }
            Op::Dst => {
                let (a, b) = (src(0, m, &st), src(1, m, &st));
                m.write(i, [1.0, a[1] * b[1], a[2], b[3]], &mut st);
            }
            Op::M4x4 | Op::M4x3 | Op::M3x4 | Op::M3x3 | Op::M3x2 => {
                let (rows, dims) = match i.op {
                    Op::M4x4 => (4, 4),
                    Op::M4x3 => (3, 4),
                    Op::M3x4 => (4, 3),
                    Op::M3x3 => (3, 3),
                    _ => (2, 3),
                };
                let a = src(0, m, &st);
                let mut out = [0.0; 4];
                for (k, o) in out.iter_mut().enumerate().take(rows) {
                    let mut row = i.src[1];
                    row.index += k as u32;
                    let r = m.read(&st, &row);
                    *o = if dims == 4 { dot4(a, r) } else { dot3(a, r) };
                }
                m.write(i, out, &mut st);
            }
            Op::Dsx | Op::Dsy => m.write(i, splat(0.0), &mut st),
            Op::Tex(control) => {
                let mut coord = src(0, m, &st);
                if *control == 1 && coord[3] != 0.0 {
                    coord = [coord[0] / coord[3], coord[1] / coord[3], coord[2] / coord[3], 1.0];
                }
                let s = &i.src[1];
                let t = (m.sample)(s.index, coord);
                m.write(i, [t[s.swizzle[0] as usize], t[s.swizzle[1] as usize], t[s.swizzle[2] as usize], t[s.swizzle[3] as usize]], &mut st);
            }
            Op::TexLdl | Op::TexLdd => {
                let coord = src(0, m, &st);
                let s = &i.src[1];
                let t = (m.sample)(s.index, coord);
                m.write(i, [t[s.swizzle[0] as usize], t[s.swizzle[1] as usize], t[s.swizzle[2] as usize], t[s.swizzle[3] as usize]], &mut st);
            }
            Op::TexKill => {
                let d = i.dst.as_ref().unwrap();
                let s = Src { reg_type: d.reg_type, index: d.index, swizzle: [0, 1, 2, 3], modifier: SrcMod::None, relative: None };
                let v = m.read(&st, &s);
                if v[0] < 0.0 || v[1] < 0.0 || v[2] < 0.0 {
                    m.killed = true;
                }
            }
            Op::Setp(c) => {
                let (a, b) = (src(0, m, &st), src(1, m, &st));
                let d = i.dst.as_ref().unwrap();
                for k in 0..4 {
                    if d.write_mask & (1 << k) != 0 {
                        st.p0[k] = cmp(*c, a[k], b[k]);
                    }
                }
            }
            Op::If => {
                if !m.cond(&st, &i.src[0]) {
                    pc = partner[pc];
                }
            }
            Op::Ifc(c) => {
                let (a, b) = (src(0, m, &st)[0], src(1, m, &st)[0]);
                if !cmp(*c, a, b) {
                    pc = partner[pc];
                }
            }
            // Reaching `else` means the `if` branch ran: skip to `endif`.
            Op::Else => pc = partner[pc],
            Op::EndIf => {}
            Op::Rep | Op::Loop => {
                let count_reg = if i.op == Op::Rep { &i.src[0] } else { &i.src[1] };
                let iv = m.i[(count_reg.index as usize).min(15)];
                let count = iv[0].clamp(0, 255);
                if count == 0 {
                    pc = partner[pc];
                } else {
                    let step = if i.op == Op::Loop { iv[2] } else { 0 };
                    if i.op == Op::Loop {
                        st.loops.push(iv[1]);
                    }
                    frames.push((pc, count, step));
                }
            }
            Op::EndRep | Op::EndLoop => {
                let Some(frame) = frames.last_mut() else { bail!("loop end without loop") };
                frame.1 -= 1;
                if i.op == Op::EndLoop {
                    if let Some(al) = st.loops.last_mut() {
                        *al += frame.2;
                    }
                }
                if frame.1 > 0 {
                    pc = frame.0;
                } else {
                    frames.pop();
                    if i.op == Op::EndLoop {
                        st.loops.pop();
                    }
                }
            }
            Op::Break | Op::Breakc(_) | Op::BreakP => {
                let take = match &i.op {
                    Op::Break => true,
                    Op::Breakc(c) => cmp(*c, src(0, m, &st)[0], src(1, m, &st)[0]),
                    _ => m.cond(&st, &i.src[0]),
                };
                if take {
                    let Some((start, _, _)) = frames.pop() else { bail!("break outside a loop") };
                    if p.instrs[start].op == Op::Loop {
                        st.loops.pop();
                    }
                    pc = partner[start];
                }
            }
            op => bail!("unsupported instruction {op:?}"),
        }
        pc += 1;
    }
    if p.stage == Stage::Vertex && m.o.iter().all(|o| o.iter().all(|x| *x == 0.0)) {
        bail!("vertex shader wrote no outputs");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_sample_vs() {
        let p = crate::d3d9::parse_program(&crate::d3d9::tests::sample_vs()).unwrap();
        let sample = |_: u32, _: Vec4| [0.0; 4];
        let mut m = Machine::new(&sample);
        m.v[0] = [1.0, 2.0, 3.0, 1.0];
        m.c[0] = [1.0, 1.0, 1.0, 1.0];
        run(&p, &mut m).unwrap();
        // dp4 o0.x, v0, c0 ; mov o0.yzw, c4 (def 1, 0, 0, 1)
        assert_eq!(m.o[0], [7.0, 0.0, 0.0, 1.0]);
    }
}
