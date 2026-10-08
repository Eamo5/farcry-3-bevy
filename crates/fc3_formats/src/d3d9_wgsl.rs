//! Translates Direct3D 9 shader model 3 programs (see `d3d9`) into WGSL.
//!
//! The output keeps D3D9 semantics close to the original:
//!
//! * Registers become `vec4` variables (`r#`, `v#`, `o#`, `oC#`), the address
//!   register `a0` a `vec4<i32>`, and the predicate `p0` a `vec4<bool>`.
//! * Float, integer and boolean constants live in one uniform block
//!   (`D3dConstants`); constants defined in the shader (`def`) override it, as
//!   in D3D9.
//! * Samplers become a texture plus sampler binding each, at bindings derived
//!   from the sampler register and texture type (see [`Options`]).
//! * Vertex attributes take locations in declaration order (recorded in
//!   [`Translation::inputs`]). Vertex outputs and pixel inputs get fixed
//!   locations from their semantics ([`varying_location`]), so any vertex
//!   shader links with any pixel shader that reads a subset of its outputs.
//!
//! WGSL only allows implicit-derivative sampling (`texld`, `dsx`, ...) in
//! uniform control flow. Inside branches or loops whose conditions depend on
//! per-pixel values, sampling falls back to the base mip level and
//! derivatives to zero; [`Translation::nonuniform_samples`] counts those.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use anyhow::{bail, Result};

use crate::d3d9::{Cmp, Decl, Dst, Instr, Op, Program, RegType, Src, SrcMod, Stage, TextureType, Usage};

/// Binding layout for the generated module.
#[derive(Debug, Clone)]
pub struct Options {
    /// Bind group expression, e.g. `"0"` or `"#{MATERIAL_BIND_GROUP}"`.
    pub group: String,
    /// Binding of the `D3dConstants` uniform block.
    pub constants_binding: u32,
    /// Textures for sampler `s#` bind at `base + 2 * #`, their samplers at `base + 2 * # + 1`.
    pub sampler_base_2d: u32,
    pub sampler_base_cube: u32,
    pub sampler_base_3d: u32,
    /// Emit `@vertex fn vs_main` / `@fragment fn fs_main` entry points.
    pub entry_point: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            group: "0".into(),
            constants_binding: 0,
            sampler_base_2d: 16,
            sampler_base_cube: 48,
            sampler_base_3d: 80,
            entry_point: true,
        }
    }
}

/// A shader input or output with its semantic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Io {
    pub usage: Usage,
    pub usage_index: u8,
    /// `v#` / `o#` register.
    pub register: u32,
    /// Location in the generated entry point (`None` for `oPos`).
    pub location: Option<u32>,
    /// Dataflow summary. Vertex outputs: bit `n` set when input `v#n` feeds
    /// the output. Pixel inputs: [`ROLE_TEXCOORD`] / [`ROLE_NORMAL`].
    pub flags: u32,
}

/// The pixel shader input feeds a texture coordinate.
pub const ROLE_TEXCOORD: u32 = 1;
/// The pixel shader input is normalised (used as a direction).
pub const ROLE_NORMAL: u32 = 2;

/// Taint analysis: which inputs feed each output register, and how pixel
/// shader inputs are used. Tracked per component: a write replaces what the
/// written components held (outside branches, loops and predication, where it
/// adds to it), so a register reused for something else stops carrying its
/// old inputs (a UV register reused for a normal isn't "normalised").
fn dataflow(p: &Program) -> (BTreeMap<u32, u32>, BTreeMap<u32, u32>) {
    let mut taint: BTreeMap<(RegType, u32), [u32; 4]> = BTreeMap::new();
    let mut roles: BTreeMap<u32, u32> = BTreeMap::new();
    // Inputs a source's component `c` (after swizzle) carries.
    let comp = |taint: &BTreeMap<(RegType, u32), [u32; 4]>, s: &Src, c: usize| -> u32 {
        match s.reg_type {
            RegType::Input if s.index < 32 => 1 << s.index,
            RegType::Temp => taint.get(&(RegType::Temp, s.index)).map_or(0, |t| t[s.swizzle[c] as usize & 3]),
            _ => 0,
        }
    };
    let all = |taint: &BTreeMap<(RegType, u32), [u32; 4]>, s: &Src| -> u32 { (0..4).fold(0, |m, c| m | comp(taint, s, c)) };
    // Subroutines run out of order: there, every write adds.
    let subroutines = p.instrs.iter().any(|i| matches!(i.op, Op::Call | Op::CallNz | Op::Label));
    let mut depth = 0i32;
    for i in &p.instrs {
        match i.op {
            Op::If | Op::Ifc(_) | Op::Loop | Op::Rep => depth += 1,
            Op::EndIf | Op::EndLoop | Op::EndRep => depth -= 1,
            _ => {}
        }
        let Some(d) = i.dst.as_ref() else { continue };
        if matches!(i.op, Op::Dcl(_) | Op::Def(_) | Op::DefI(_) | Op::DefB(_) | Op::TexKill) {
            continue;
        }
        let is_tex = matches!(i.op, Op::Tex(_) | Op::TexLdl | Op::TexLdd);
        if p.stage == Stage::Pixel {
            for (k, s) in i.src.iter().enumerate() {
                let role = if is_tex && k == 0 {
                    ROLE_TEXCOORD
                } else if i.op == Op::Nrm {
                    ROLE_NORMAL
                } else {
                    0
                };
                let st = if i.op == Op::Nrm { (0..3).fold(0, |m, c| m | comp(&taint, s, c)) } else { all(&taint, s) };
                if role != 0 {
                    for bit in (0..32).filter(|b| st & (1 << b) != 0) {
                        *roles.entry(bit).or_default() |= role;
                    }
                }
            }
        }
        // Component-wise operations carry component c from the sources' c;
        // the rest (dot products, normalise, matrix and scalar operations)
        // mix every component they read.
        let componentwise = matches!(
            i.op,
            Op::Mov | Op::Add | Op::Sub | Op::Mad | Op::Mul | Op::Min | Op::Max | Op::Slt | Op::Sge | Op::Frc | Op::Abs
                | Op::Sgn | Op::Lrp | Op::Cnd | Op::Cmp | Op::Mova | Op::Dsx | Op::Dsy
        );
        // A sample's value comes from the texture, not from where it was
        // read (otherwise a UV that reads a normal map looks normalised).
        let sources: Vec<&Src> = i.src.iter().enumerate().filter(|(k, _)| !(is_tex && *k == 0)).map(|(_, s)| s).collect();
        let mixed = sources.iter().fold(0, |m, s| m | all(&taint, s));
        let add = subroutines || depth > 0 || i.predicate.is_some();
        let mut t = taint.get(&(d.reg_type, d.index)).copied().unwrap_or_default();
        for c in (0..4).filter(|c| d.write_mask & (1 << c) != 0) {
            let v = if componentwise { sources.iter().fold(0, |m, s| m | comp(&taint, s, c)) } else { mixed };
            t[c] = if add { t[c] | v } else { v };
        }
        taint.insert((d.reg_type, d.index), t);
    }
    let outputs = taint.iter().filter(|((t, _), _)| *t == RegType::Output).map(|((_, n), m)| (*n, m.iter().fold(0, |a, b| a | b))).collect();
    (outputs, roles)
}

/// Which samplers (bit per sampler register) feed the alpha of colour output
/// 0 in a pixel shader, following values through temporaries per component
/// as `dataflow` does. A texture read's value carries its sampler.
pub fn colour_alpha_samplers(p: &Program) -> u32 {
    let mut taint: BTreeMap<(RegType, u32), [u32; 4]> = BTreeMap::new();
    let comp = |taint: &BTreeMap<(RegType, u32), [u32; 4]>, s: &Src, c: usize| -> u32 {
        match s.reg_type {
            RegType::Temp => taint.get(&(RegType::Temp, s.index)).map_or(0, |t| t[s.swizzle[c] as usize & 3]),
            _ => 0,
        }
    };
    let all = |taint: &BTreeMap<(RegType, u32), [u32; 4]>, s: &Src| -> u32 { (0..4).fold(0, |m, c| m | comp(taint, s, c)) };
    let mut depth = 0i32;
    for i in &p.instrs {
        match i.op {
            Op::If | Op::Ifc(_) | Op::Loop | Op::Rep => depth += 1,
            Op::EndIf | Op::EndLoop | Op::EndRep => depth -= 1,
            _ => {}
        }
        let Some(d) = i.dst.as_ref() else { continue };
        if matches!(i.op, Op::Dcl(_) | Op::Def(_) | Op::DefI(_) | Op::DefB(_) | Op::TexKill) {
            continue;
        }
        let is_tex = matches!(i.op, Op::Tex(_) | Op::TexLdl | Op::TexLdd);
        let componentwise = matches!(
            i.op,
            Op::Mov | Op::Add | Op::Sub | Op::Mad | Op::Mul | Op::Min | Op::Max | Op::Slt | Op::Sge | Op::Frc | Op::Abs
                | Op::Sgn | Op::Lrp | Op::Cnd | Op::Cmp | Op::Dsx | Op::Dsy
        );
        let sampler = if is_tex { i.src.get(1).filter(|s| s.reg_type == RegType::Sampler).map_or(0, |s| 1u32 << (s.index & 31)) } else { 0 };
        let mixed = if is_tex { sampler } else { i.src.iter().fold(0, |m, s| m | all(&taint, s)) };
        let add = depth > 0 || i.predicate.is_some();
        let mut t = taint.get(&(d.reg_type, d.index)).copied().unwrap_or_default();
        for c in (0..4).filter(|c| d.write_mask & (1 << c) != 0) {
            let v = if componentwise { i.src.iter().fold(0, |m, s| m | comp(&taint, s, c)) } else { mixed };
            t[c] = if add { t[c] | v } else { v };
        }
        taint.insert((d.reg_type, d.index), t);
    }
    taint.get(&(RegType::ColorOut, 0)).map_or(0, |t| t[3])
}

#[derive(Debug, Clone, Default)]
pub struct Translation {
    pub wgsl: String,
    pub inputs: Vec<Io>,
    pub outputs: Vec<Io>,
    /// Declared samplers: register and texture type.
    pub samplers: Vec<(u32, TextureType)>,
    /// `def` constants (they override the constant block).
    pub float_defs: Vec<(u32, [f32; 4])>,
    /// Pixel shader colour outputs written (bit per `oC#`).
    pub color_outputs: u8,
    pub writes_depth: bool,
    pub uses_vpos: bool,
    pub uses_vface: bool,
    /// Samples or derivatives emitted without implicit derivatives because
    /// they sit in non-uniform control flow.
    pub nonuniform_samples: u32,
}

/// Inter-stage location for a vertex output / pixel input semantic.
pub fn varying_location(usage: Usage, index: u8) -> Option<u32> {
    let i = index as u32;
    Some(match usage {
        Usage::TexCoord if i < 10 => i,
        Usage::Color if i < 2 => 10 + i,
        Usage::Fog if i == 0 => 12,
        Usage::Normal if i == 0 => 13,
        Usage::Tangent if i == 0 => 14,
        Usage::Binormal if i == 0 => 15,
        Usage::Position if i == 1 => 15,
        _ => return None,
    })
}

fn float_lit(v: f32) -> String {
    if v.is_finite() {
        format!("{v:?}")
    } else {
        format!("bitcast<f32>({:#010x}u)", v.to_bits())
    }
}

fn vec4_lit(v: [f32; 4]) -> String {
    format!("vec4<f32>({}, {}, {}, {})", float_lit(v[0]), float_lit(v[1]), float_lit(v[2]), float_lit(v[3]))
}

const COMP: [char; 4] = ['x', 'y', 'z', 'w'];

/// Open control-flow blocks, used for the uniformity analysis and `aL`.
#[derive(Clone)]
enum Block {
    If { uniform: bool },
    /// `counter` names the `aL` variable of a `loop` (none for `rep`), which
    /// advances by `step` at the end of each iteration.
    Loop { uniform: bool, counter: Option<usize>, step: String },
}

struct Gen<'a> {
    p: &'a Program,
    t: Translation,
    body: String,
    indent: usize,
    blocks: Vec<Block>,
    loop_counter: usize,
    temps: BTreeSet<u32>,
    float_defs: BTreeMap<u32, [f32; 4]>,
    int_defs: BTreeMap<u32, [i32; 4]>,
    bool_defs: BTreeMap<u32, bool>,
    samplers: BTreeMap<u32, TextureType>,
    uses_a0: bool,
    uses_p0: bool,
    uses_crel: bool,
    /// Loops (by instruction index of their start) that contain a data-dependent break.
    nonuniform_loops: BTreeSet<usize>,
    pc: usize,
}

impl Gen<'_> {
    fn line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.body.push_str("    ");
        }
        self.body.push_str(s);
        self.body.push('\n');
    }

    fn uniform_flow(&self) -> bool {
        self.blocks.iter().all(|b| match b {
            Block::If { uniform } | Block::Loop { uniform, .. } => *uniform,
        })
    }

    fn current_al(&self) -> Result<String> {
        for b in self.blocks.iter().rev() {
            if let Block::Loop { counter: Some(counter), .. } = b {
                return Ok(format!("aL{counter}"));
            }
        }
        bail!("aL used outside a loop")
    }

    fn rel_index(&self, s_index: u32, rel: &crate::d3d9::RelAddr) -> Result<String> {
        let base = match rel.reg_type {
            RegType::AddrOrTexture => {
                format!("a0.{}", COMP[rel.component as usize])
            }
            RegType::Loop => self.current_al()?,
            t => bail!("unsupported relative address register {t:?}"),
        };
        Ok(format!("({base} + {}i)", s_index))
    }

    fn float_const(&mut self, index: u32) -> String {
        if self.float_defs.contains_key(&index) {
            format!("D3D_C{index}")
        } else {
            format!("d3d_c.f[{index}]")
        }
    }

    /// Unswizzled vec4 expression for a source register.
    fn reg_expr(&mut self, s: &Src) -> Result<String> {
        if let Some(rel) = &s.relative {
            let idx = self.rel_index(s.index, rel)?;
            return match s.reg_type {
                RegType::Const => {
                    self.uses_crel = true;
                    Ok(format!("d3d_crel({idx})"))
                }
                RegType::Input => Ok(format!("d3d_vrel({idx})")),
                t => bail!("unsupported relative source {t:?}"),
            };
        }
        Ok(match s.reg_type {
            RegType::Temp => {
                self.temps.insert(s.index);
                format!("r{}", s.index)
            }
            RegType::Input => format!("v{}", s.index),
            RegType::Const => self.float_const(s.index),
            RegType::ConstInt => {
                if self.int_defs.contains_key(&s.index) {
                    format!("vec4<f32>(D3D_I{})", s.index)
                } else {
                    format!("vec4<f32>(d3d_c.i[{}])", s.index)
                }
            }
            RegType::AddrOrTexture if self.p.stage == Stage::Vertex => {
                self.uses_a0 = true;
                "vec4<f32>(a0)".into()
            }
            RegType::Loop => format!("vec4<f32>(f32({}))", self.current_al()?),
            RegType::MiscType => {
                if s.index == 0 {
                    self.t.uses_vpos = true;
                    "d3d_vpos".into()
                } else {
                    self.t.uses_vface = true;
                    "d3d_vface".into()
                }
            }
            RegType::Output => format!("o{}", s.index),
            t => bail!("unsupported source register {t:?}"),
        })
    }

    fn swizzle(s: &Src) -> String {
        if s.swizzle == [0, 1, 2, 3] {
            String::new()
        } else {
            let mut out = String::from(".");
            out.extend(s.swizzle.iter().map(|&c| COMP[c as usize]));
            out
        }
    }

    /// Full vec4 expression for a float source (swizzle and modifier applied).
    fn src(&mut self, s: &Src) -> Result<String> {
        let e = format!("{}{}", self.reg_expr(s)?, Self::swizzle(s));
        Ok(match s.modifier {
            SrcMod::None => e,
            SrcMod::Neg => format!("(-{e})"),
            SrcMod::Abs => format!("abs({e})"),
            SrcMod::AbsNeg => format!("(-abs({e}))"),
            SrcMod::Bias => format!("({e} - vec4<f32>(0.5))"),
            SrcMod::BiasNeg => format!("(vec4<f32>(0.5) - {e})"),
            SrcMod::Sign => format!("({e} * 2.0 - vec4<f32>(1.0))"),
            SrcMod::SignNeg => format!("(vec4<f32>(1.0) - {e} * 2.0)"),
            SrcMod::Comp => format!("(vec4<f32>(1.0) - {e})"),
            SrcMod::X2 => format!("({e} * 2.0)"),
            SrcMod::X2Neg => format!("({e} * -2.0)"),
            m => bail!("unsupported source modifier {m:?}"),
        })
    }

    /// Scalar from a source with a replicate swizzle (first selected component).
    fn src_scalar(&mut self, s: &Src) -> Result<String> {
        Ok(format!("{}.x", self.src(s)?))
    }

    fn bool_src(&mut self, s: &Src) -> Result<String> {
        let e = match s.reg_type {
            RegType::ConstBool => match self.bool_defs.get(&s.index) {
                Some(b) => b.to_string(),
                None => format!("(d3d_c.b[{}].{} != 0u)", s.index / 4, COMP[(s.index % 4) as usize]),
            },
            RegType::Predicate => {
                self.uses_p0 = true;
                format!("p0.{}", COMP[s.swizzle[0] as usize])
            }
            t => bail!("unsupported condition register {t:?}"),
        };
        Ok(if s.modifier == SrcMod::Not { format!("(!{e})") } else { e })
    }

    fn int_src(&mut self, s: &Src) -> Result<String> {
        if s.reg_type != RegType::ConstInt {
            bail!("loop count must be an integer constant");
        }
        Ok(if self.int_defs.contains_key(&s.index) { format!("D3D_I{}", s.index) } else { format!("d3d_c.i[{}]", s.index) })
    }

    fn dst_name(&mut self, d: &Dst) -> Result<String> {
        if d.relative.is_some() {
            bail!("relative destination addressing is not supported");
        }
        Ok(match d.reg_type {
            RegType::Temp => {
                self.temps.insert(d.index);
                format!("r{}", d.index)
            }
            RegType::Output => format!("o{}", d.index),
            RegType::ColorOut => {
                self.t.color_outputs |= 1 << d.index;
                format!("oC{}", d.index)
            }
            RegType::DepthOut => {
                self.t.writes_depth = true;
                "oDepth".into()
            }
            RegType::AddrOrTexture if self.p.stage == Stage::Vertex => {
                self.uses_a0 = true;
                "a0".into()
            }
            RegType::Predicate => {
                self.uses_p0 = true;
                "p0".into()
            }
            t => bail!("unsupported destination register {t:?}"),
        })
    }

    /// Writes a vec4 float expression to the destination (mask, saturate, predicate).
    fn store(&mut self, i: &Instr, value: String) -> Result<()> {
        let d = i.dst.as_ref().unwrap();
        let name = self.dst_name(d)?;
        let mut v = value;
        if d.shift != 0 {
            v = format!("({v} * {})", float_lit(2f32.powi(d.shift as i32)));
        }
        if d.saturate {
            v = format!("clamp({v}, vec4<f32>(0.0), vec4<f32>(1.0))");
        }
        let (cast, scalar_target) = match d.reg_type {
            RegType::AddrOrTexture => ("i32", false),
            RegType::DepthOut => ("", true),
            _ => ("", false),
        };
        let pred = match &i.predicate {
            Some(p) => {
                self.uses_p0 = true;
                Some((p.swizzle, p.modifier == SrcMod::Not))
            }
            None => None,
        };
        self.line(&format!("{{ let t = {v};"));
        self.indent += 1;
        for c in 0..4 {
            if d.write_mask & (1 << c) == 0 {
                continue;
            }
            let rhs = if cast.is_empty() { format!("t.{}", COMP[c]) } else { format!("{cast}(round(t.{}))", COMP[c]) };
            let stmt = if scalar_target { format!("{name} = {rhs};") } else { format!("{name}.{} = {rhs};", COMP[c]) };
            match pred {
                Some((sw, not)) => {
                    let cond = format!("{}p0.{}", if not { "!" } else { "" }, COMP[sw[c] as usize]);
                    self.line(&format!("if ({cond}) {{ {stmt} }}"));
                }
                None => self.line(&stmt),
            }
        }
        self.indent -= 1;
        self.line("}");
        Ok(())
    }

    fn sample(&mut self, i: &Instr) -> Result<String> {
        let coord = self.src(&i.src[0])?;
        let s = &i.src[1];
        if s.reg_type != RegType::Sampler {
            bail!("texture instruction without sampler");
        }
        let ty = *self.samplers.get(&s.index).unwrap_or(&TextureType::Tex2D);
        let (tex, smp) = (format!("d3d_t{}", s.index), format!("d3d_s{}", s.index));
        let uv = match ty {
            TextureType::Tex2D | TextureType::Unknown => format!("{coord}.xy"),
            _ => format!("{coord}.xyz"),
        };
        let fragment = self.p.stage == Stage::Pixel;
        let implicit_ok = fragment && self.uniform_flow();
        let expr = match &i.op {
            Op::Tex(control) => {
                let uv = if *control == 1 && matches!(ty, TextureType::Tex2D | TextureType::Unknown) {
                    format!("({coord}.xy / {coord}.w)")
                } else {
                    uv
                };
                if !implicit_ok {
                    self.t.nonuniform_samples += 1;
                    format!("textureSampleLevel({tex}, {smp}, {uv}, 0.0)")
                } else if *control == 2 {
                    format!("textureSampleBias({tex}, {smp}, {uv}, {coord}.w)")
                } else {
                    format!("textureSample({tex}, {smp}, {uv})")
                }
            }
            Op::TexLdl => format!("textureSampleLevel({tex}, {smp}, {uv}, {coord}.w)"),
            Op::TexLdd => {
                let dx = self.src(&i.src[2])?;
                let dy = self.src(&i.src[3])?;
                let sw = if matches!(ty, TextureType::Tex2D | TextureType::Unknown) { "xy" } else { "xyz" };
                format!("textureSampleGrad({tex}, {smp}, {uv}, {dx}.{sw}, {dy}.{sw})")
            }
            _ => unreachable!(),
        };
        Ok(format!("{expr}{}", Self::swizzle(s)))
    }

    fn matrix(&mut self, i: &Instr, rows: u32, dims: usize) -> Result<String> {
        let a = self.src(&i.src[0])?;
        let mut parts = Vec::new();
        for k in 0..rows {
            let mut row = i.src[1];
            row.index += k;
            let r = self.src(&row)?;
            parts.push(if dims == 4 { format!("dot({a}, {r})") } else { format!("dot({a}.xyz, {r}.xyz)") });
        }
        while parts.len() < 4 {
            parts.push("0.0".into());
        }
        Ok(format!("vec4<f32>({})", parts.join(", ")))
    }

    fn cmp_op(c: Cmp) -> &'static str {
        match c {
            Cmp::Gt => ">",
            Cmp::Eq => "==",
            Cmp::Ge => ">=",
            Cmp::Lt => "<",
            Cmp::Ne => "!=",
            Cmp::Le => "<=",
        }
    }

    fn instr(&mut self, i: &Instr) -> Result<()> {
        let splat = |e: String| format!("vec4<f32>({e})");
        match &i.op {
            Op::Nop | Op::Dcl(_) | Op::Def(_) | Op::DefI(_) | Op::DefB(_) => {}
            Op::Mov => {
                let a = self.src(&i.src[0])?;
                self.store(i, a)?;
            }
            Op::Mova => {
                let a = self.src(&i.src[0])?;
                self.store(i, a)?;
            }
            Op::Add | Op::Sub | Op::Mul | Op::Min | Op::Max | Op::Slt | Op::Sge => {
                let a = self.src(&i.src[0])?;
                let b = self.src(&i.src[1])?;
                let v = match i.op {
                    Op::Add => format!("({a} + {b})"),
                    Op::Sub => format!("({a} - {b})"),
                    Op::Mul => format!("({a} * {b})"),
                    Op::Min => format!("min({a}, {b})"),
                    Op::Max => format!("max({a}, {b})"),
                    Op::Slt => format!("select(vec4<f32>(0.0), vec4<f32>(1.0), {a} < {b})"),
                    _ => format!("select(vec4<f32>(0.0), vec4<f32>(1.0), {a} >= {b})"),
                };
                self.store(i, v)?;
            }
            Op::Mad => {
                let (a, b, c) = (self.src(&i.src[0])?, self.src(&i.src[1])?, self.src(&i.src[2])?);
                self.store(i, format!("({a} * {b} + {c})"))?;
            }
            Op::Lrp => {
                let (a, b, c) = (self.src(&i.src[0])?, self.src(&i.src[1])?, self.src(&i.src[2])?);
                self.store(i, format!("mix({c}, {b}, {a})"))?;
            }
            Op::Cmp => {
                let (a, b, c) = (self.src(&i.src[0])?, self.src(&i.src[1])?, self.src(&i.src[2])?);
                self.store(i, format!("select({c}, {b}, {a} >= vec4<f32>(0.0))"))?;
            }
            Op::Cnd => {
                let (a, b, c) = (self.src(&i.src[0])?, self.src(&i.src[1])?, self.src(&i.src[2])?);
                self.store(i, format!("select({c}, {b}, {a} > vec4<f32>(0.5))"))?;
            }
            Op::Rcp => {
                let a = self.src_scalar(&i.src[0])?;
                self.store(i, splat(format!("1.0 / {a}")))?;
            }
            Op::Rsq => {
                let a = self.src_scalar(&i.src[0])?;
                self.store(i, splat(format!("inverseSqrt(abs({a}))")))?;
            }
            Op::Exp | Op::ExpP => {
                let a = self.src_scalar(&i.src[0])?;
                self.store(i, splat(format!("exp2({a})")))?;
            }
            Op::Log | Op::LogP => {
                let a = self.src_scalar(&i.src[0])?;
                self.store(i, splat(format!("log2(abs({a}))")))?;
            }
            Op::Pow => {
                let a = self.src_scalar(&i.src[0])?;
                let b = self.src_scalar(&i.src[1])?;
                self.store(i, splat(format!("pow(abs({a}), {b})")))?;
            }
            Op::Frc => {
                let a = self.src(&i.src[0])?;
                self.store(i, format!("fract({a})"))?;
            }
            Op::Abs => {
                let a = self.src(&i.src[0])?;
                self.store(i, format!("abs({a})"))?;
            }
            Op::Sgn => {
                let a = self.src(&i.src[0])?;
                self.store(i, format!("sign({a})"))?;
            }
            Op::Dp3 => {
                let (a, b) = (self.src(&i.src[0])?, self.src(&i.src[1])?);
                self.store(i, splat(format!("dot({a}.xyz, {b}.xyz)")))?;
            }
            Op::Dp4 => {
                let (a, b) = (self.src(&i.src[0])?, self.src(&i.src[1])?);
                self.store(i, splat(format!("dot({a}, {b})")))?;
            }
            Op::Dp2Add => {
                let (a, b, c) = (self.src(&i.src[0])?, self.src(&i.src[1])?, self.src(&i.src[2])?);
                self.store(i, splat(format!("dot({a}.xy, {b}.xy) + {c}.x")))?;
            }
            Op::Nrm => {
                let a = self.src(&i.src[0])?;
                self.store(i, format!("({a} * inverseSqrt(dot({a}.xyz, {a}.xyz)))"))?;
            }
            Op::Crs => {
                let (a, b) = (self.src(&i.src[0])?, self.src(&i.src[1])?);
                self.store(i, format!("vec4<f32>(cross({a}.xyz, {b}.xyz), 0.0)"))?;
            }
            Op::SinCos => {
                let a = self.src_scalar(&i.src[0])?;
                self.store(i, format!("vec4<f32>(cos({a}), sin({a}), 0.0, 0.0)"))?;
            }
            Op::Lit => {
                let a = self.src(&i.src[0])?;
                self.store(
                    i,
                    format!(
                        "vec4<f32>(1.0, max({a}.x, 0.0), select(0.0, pow(max({a}.y, 0.0), clamp({a}.w, -128.0, 128.0)), {a}.x > 0.0), 1.0)"
                    ),
                )?;
            }
            Op::Dst => {
                let (a, b) = (self.src(&i.src[0])?, self.src(&i.src[1])?);
                self.store(i, format!("vec4<f32>(1.0, {a}.y * {b}.y, {a}.z, {b}.w)"))?;
            }
            Op::M4x4 => {
                let v = self.matrix(i, 4, 4)?;
                self.store(i, v)?;
            }
            Op::M4x3 => {
                let v = self.matrix(i, 3, 4)?;
                self.store(i, v)?;
            }
            Op::M3x4 => {
                let v = self.matrix(i, 4, 3)?;
                self.store(i, v)?;
            }
            Op::M3x3 => {
                let v = self.matrix(i, 3, 3)?;
                self.store(i, v)?;
            }
            Op::M3x2 => {
                let v = self.matrix(i, 2, 3)?;
                self.store(i, v)?;
            }
            Op::Dsx | Op::Dsy => {
                let a = self.src(&i.src[0])?;
                let v = if self.p.stage == Stage::Pixel && self.uniform_flow() {
                    format!("{}({a})", if i.op == Op::Dsx { "dpdx" } else { "dpdy" })
                } else {
                    self.t.nonuniform_samples += 1;
                    "vec4<f32>(0.0)".into()
                };
                self.store(i, v)?;
            }
            Op::Tex(_) | Op::TexLdl | Op::TexLdd => {
                let v = self.sample(i)?;
                self.store(i, v)?;
            }
            Op::TexKill => {
                let d = i.dst.as_ref().unwrap();
                let s = Src { reg_type: d.reg_type, index: d.index, swizzle: [0, 1, 2, 3], modifier: SrcMod::None, relative: None };
                let e = self.reg_expr(&s)?;
                self.line(&format!("if (any({e}.xyz < vec3<f32>(0.0))) {{ discard; }}"));
            }
            Op::Setp(c) => {
                let (a, b) = (self.src(&i.src[0])?, self.src(&i.src[1])?);
                let d = i.dst.as_ref().unwrap();
                self.uses_p0 = true;
                for k in 0..4 {
                    if d.write_mask & (1 << k) != 0 {
                        self.line(&format!("p0.{0} = {a}.{0} {1} {b}.{0};", COMP[k], Self::cmp_op(*c)));
                    }
                }
            }
            Op::If => {
                let cond = self.bool_src(&i.src[0])?;
                let uniform = i.src[0].reg_type == RegType::ConstBool;
                self.line(&format!("if ({cond}) {{"));
                self.indent += 1;
                self.blocks.push(Block::If { uniform });
            }
            Op::Ifc(c) => {
                let (a, b) = (self.src_scalar(&i.src[0])?, self.src_scalar(&i.src[1])?);
                self.line(&format!("if ({a} {} {b}) {{", Self::cmp_op(*c)));
                self.indent += 1;
                self.blocks.push(Block::If { uniform: false });
            }
            Op::Else => {
                self.indent -= 1;
                self.line("} else {");
                self.indent += 1;
            }
            Op::EndIf => {
                self.indent -= 1;
                self.line("}");
                match self.blocks.pop() {
                    Some(Block::If { .. }) => {}
                    _ => bail!("unbalanced endif"),
                }
            }
            Op::Rep => {
                let count = self.int_src(&i.src[0])?;
                let n = self.loop_counter;
                self.loop_counter += 1;
                let uniform = !self.nonuniform_loops.contains(&self.pc);
                self.line(&format!("for (var k{n}: i32 = 0; k{n} < {count}.x; k{n} = k{n} + 1) {{"));
                self.indent += 1;
                self.blocks.push(Block::Loop { uniform, counter: None, step: String::new() });
            }
            Op::Loop => {
                // loop aL, i#: i#.x = count, .y = start, .z = step.
                let count = self.int_src(&i.src[1])?;
                let n = self.loop_counter;
                self.loop_counter += 1;
                let uniform = !self.nonuniform_loops.contains(&self.pc);
                self.line(&format!("var aL{n}: i32 = {count}.y;"));
                self.line(&format!("for (var k{n}: i32 = 0; k{n} < {count}.x; k{n} = k{n} + 1) {{"));
                self.indent += 1;
                self.blocks.push(Block::Loop { uniform, counter: Some(n), step: format!("{count}.z") });
            }
            Op::EndRep | Op::EndLoop => {
                match self.blocks.pop() {
                    Some(Block::Loop { counter, step, .. }) => {
                        if let Some(n) = counter {
                            self.line(&format!("aL{n} = aL{n} + {step};"));
                        }
                    }
                    _ => bail!("unbalanced endloop"),
                }
                self.indent -= 1;
                self.line("}");
            }
            Op::Break => self.line("break;"),
            Op::Breakc(c) => {
                let (a, b) = (self.src_scalar(&i.src[0])?, self.src_scalar(&i.src[1])?);
                self.line(&format!("if ({a} {} {b}) {{ break; }}", Self::cmp_op(*c)));
            }
            Op::BreakP => {
                let cond = self.bool_src(&i.src[0])?;
                self.line(&format!("if ({cond}) {{ break; }}"));
            }
            op => bail!("unsupported instruction {op:?}"),
        }
        Ok(())
    }
}

/// Finds loops containing a break that depends on per-pixel values (which
/// makes the rest of the loop body non-uniform).
fn find_nonuniform_loops(p: &Program) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    // Stack of (instruction index, is_loop, uniform condition).
    let mut stack: Vec<(usize, bool, bool)> = Vec::new();
    for (pc, i) in p.instrs.iter().enumerate() {
        match &i.op {
            Op::Rep | Op::Loop => stack.push((pc, true, true)),
            Op::If => stack.push((pc, false, i.src[0].reg_type == RegType::ConstBool)),
            Op::Ifc(_) => stack.push((pc, false, false)),
            Op::EndIf | Op::EndRep | Op::EndLoop => {
                stack.pop();
            }
            Op::Breakc(_) | Op::BreakP | Op::Break => {
                let dependent = !matches!(i.op, Op::Break) || stack.iter().rev().take_while(|b| !b.1).any(|b| !b.2);
                if dependent {
                    if let Some(&(start, _, _)) = stack.iter().rev().find(|b| b.1) {
                        out.insert(start);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// What a vertex shader output holds before the shader writes it.
pub const VS_OUTPUT_DEFAULT: &str = "vec4<f32>(0.0, 0.0, 0.0, 1.0)";

pub fn translate(p: &Program, opts: &Options) -> Result<Translation> {
    let mut g = Gen {
        p,
        t: Translation::default(),
        body: String::new(),
        indent: 1,
        blocks: Vec::new(),
        loop_counter: 0,
        temps: BTreeSet::new(),
        float_defs: BTreeMap::new(),
        int_defs: BTreeMap::new(),
        bool_defs: BTreeMap::new(),
        samplers: BTreeMap::new(),
        uses_a0: false,
        uses_p0: false,
        uses_crel: false,
        nonuniform_loops: find_nonuniform_loops(p),
        pc: 0,
    };
    // Declarations and definitions first: they apply to the whole program.
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for i in &p.instrs {
        let d = i.dst.as_ref();
        match &i.op {
            Op::Def(v) => {
                g.float_defs.insert(d.unwrap().index, *v);
            }
            Op::DefI(v) => {
                g.int_defs.insert(d.unwrap().index, *v);
            }
            Op::DefB(b) => {
                g.bool_defs.insert(d.unwrap().index, *b);
            }
            Op::Dcl(Decl::Sampler(ty)) => {
                g.samplers.insert(d.unwrap().index, *ty);
            }
            Op::Dcl(Decl::Semantic { usage, index }) => {
                let d = d.unwrap();
                match d.reg_type {
                    RegType::Input => {
                        let location = if p.stage == Stage::Vertex {
                            Some(inputs.len() as u32)
                        } else {
                            varying_location(*usage, *index)
                        };
                        if location.is_none() {
                            bail!("no location for input {usage:?}{index}");
                        }
                        inputs.push(Io { usage: *usage, usage_index: *index, register: d.index, location, flags: 0 });
                    }
                    RegType::Output => {
                        let location = if *usage == Usage::Position && *index == 0 {
                            None
                        } else {
                            match varying_location(*usage, *index) {
                                Some(l) => Some(l),
                                // Point size and the like have no pixel shader counterpart.
                                None if *usage == Usage::PSize => continue,
                                None => bail!("no location for output {usage:?}{index}"),
                            }
                        };
                        outputs.push(Io { usage: *usage, usage_index: *index, register: d.index, location, flags: 0 });
                    }
                    RegType::MiscType if d.index == 0 => g.t.uses_vpos = true,
                    RegType::MiscType => g.t.uses_vface = true,
                    t => bail!("unsupported declaration register {t:?}"),
                }
            }
            Op::Dcl(Decl::Plain) => {
                let d = d.unwrap();
                match d.reg_type {
                    RegType::MiscType if d.index == 0 => g.t.uses_vpos = true,
                    RegType::MiscType => g.t.uses_vface = true,
                    t => bail!("plain declaration of {t:?}"),
                }
            }
            _ => {}
        }
    }
    for (pc, i) in p.instrs.iter().enumerate() {
        g.pc = pc;
        if let Op::Other(op) = i.op {
            bail!("unsupported opcode {op}");
        }
        g.instr(i)?;
    }
    if !g.blocks.is_empty() {
        bail!("unterminated control flow");
    }

    // Module.
    let gr = &opts.group;
    let mut m = String::new();
    let _ = writeln!(m, "// Translated from D3D9 {}_{}_{} bytecode.", if p.stage == Stage::Vertex { "vs" } else { "ps" }, p.major, p.minor);
    m += "struct D3dConstants {\n    f: array<vec4<f32>, 256>,\n    i: array<vec4<i32>, 16>,\n    b: array<vec4<u32>, 4>,\n}\n";
    let _ = writeln!(m, "@group({gr}) @binding({}) var<uniform> d3d_c: D3dConstants;", opts.constants_binding);
    for (&n, &ty) in &g.samplers {
        let (base, kind) = match ty {
            TextureType::Cube => (opts.sampler_base_cube, "texture_cube<f32>"),
            TextureType::Volume => (opts.sampler_base_3d, "texture_3d<f32>"),
            _ => (opts.sampler_base_2d, "texture_2d<f32>"),
        };
        let _ = writeln!(m, "@group({gr}) @binding({}) var d3d_t{n}: {kind};", base + 2 * n);
        let _ = writeln!(m, "@group({gr}) @binding({}) var d3d_s{n}: sampler;", base + 2 * n + 1);
        g.t.samplers.push((n, ty));
    }
    for (&n, v) in &g.float_defs {
        let _ = writeln!(m, "var<private> D3D_C{n}: vec4<f32> = {};", vec4_lit(*v));
        g.t.float_defs.push((n, *v));
    }
    for (&n, v) in &g.int_defs {
        let _ = writeln!(m, "var<private> D3D_I{n}: vec4<i32> = vec4<i32>({}i, {}i, {}i, {}i);", v[0], v[1], v[2], v[3]);
    }
    for io in &inputs {
        let _ = writeln!(m, "var<private> v{}: vec4<f32>;", io.register);
    }
    // Components a vertex shader leaves unwritten reach the pixel shader as
    // (0, 0, 0, 1) on D3D9 hardware, and some shaders rely on it: decals
    // keep their pixels while a fade factor lies between an output's z and
    // its never-written w.
    for io in &outputs {
        let _ = writeln!(m, "var<private> o{}: vec4<f32> = {};", io.register, VS_OUTPUT_DEFAULT);
    }
    if p.stage == Stage::Pixel {
        for n in 0..4 {
            if g.t.color_outputs & (1 << n) != 0 {
                let _ = writeln!(m, "var<private> oC{n}: vec4<f32>;");
            }
        }
        if g.t.writes_depth {
            m += "var<private> oDepth: f32;\n";
        }
        if g.t.uses_vpos {
            m += "var<private> d3d_vpos: vec4<f32>;\n";
        }
        if g.t.uses_vface {
            m += "var<private> d3d_vface: vec4<f32>;\n";
        }
    }
    if g.uses_crel {
        m += "fn d3d_crel(i: i32) -> vec4<f32> {\n    switch i {\n";
        for &n in g.float_defs.keys() {
            let _ = writeln!(m, "        case {n}i: {{ return D3D_C{n}; }}");
        }
        m += "        default: { return d3d_c.f[clamp(i, 0i, 255i)]; }\n    }\n}\n";
    }
    if g.body.contains("d3d_vrel(") {
        m += "fn d3d_vrel(i: i32) -> vec4<f32> {\n    switch i {\n";
        for io in &inputs {
            let _ = writeln!(m, "        case {}i: {{ return v{}; }}", io.register, io.register);
        }
        m += "        default: { return vec4<f32>(0.0); }\n    }\n}\n";
    }
    m += "fn d3d_main() {\n";
    for &n in &g.temps {
        let _ = writeln!(m, "    var r{n}: vec4<f32> = vec4<f32>(0.0);");
    }
    if g.uses_a0 {
        m += "    var a0: vec4<i32> = vec4<i32>(0i);\n";
    }
    if g.uses_p0 {
        m += "    var p0: vec4<bool> = vec4<bool>(false);\n";
    }
    m += &g.body;
    m += "}\n";

    if opts.entry_point {
        match p.stage {
            Stage::Vertex => {
                m += "struct D3dVsOut {\n    @builtin(position) position: vec4<f32>,\n";
                for io in &outputs {
                    if let Some(l) = io.location {
                        let _ = writeln!(m, "    @location({l}) o{}: vec4<f32>,", io.register);
                    }
                }
                m += "}\n@vertex\nfn vs_main(";
                let args: Vec<String> =
                    inputs.iter().map(|io| format!("@location({}) in_v{}: vec4<f32>", io.location.unwrap(), io.register)).collect();
                m += &args.join(", ");
                m += ") -> D3dVsOut {\n";
                for io in &inputs {
                    let _ = writeln!(m, "    v{0} = in_v{0};", io.register);
                }
                m += "    d3d_main();\n    var out: D3dVsOut;\n";
                match outputs.iter().find(|io| io.location.is_none()) {
                    Some(pos) => {
                        let _ = writeln!(m, "    out.position = o{};", pos.register);
                    }
                    None => m += "    out.position = vec4<f32>(0.0, 0.0, 0.0, 1.0);\n",
                }
                for io in &outputs {
                    if io.location.is_some() {
                        let _ = writeln!(m, "    out.o{0} = o{0};", io.register);
                    }
                }
                m += "    return out;\n}\n";
            }
            Stage::Pixel => {
                m += "struct D3dPsOut {\n";
                for n in 0..4 {
                    if g.t.color_outputs & (1 << n) != 0 {
                        let _ = writeln!(m, "    @location({n}) c{n}: vec4<f32>,");
                    }
                }
                if g.t.writes_depth {
                    m += "    @builtin(frag_depth) depth: f32,\n";
                }
                if g.t.color_outputs == 0 && !g.t.writes_depth {
                    m += "    @location(0) c0: vec4<f32>,\n";
                }
                m += "}\n@fragment\nfn fs_main(@builtin(position) frag_pos: vec4<f32>, @builtin(front_facing) front: bool";
                for io in &inputs {
                    let _ = write!(m, ", @location({}) in_v{}: vec4<f32>", io.location.unwrap(), io.register);
                }
                m += ") -> D3dPsOut {\n";
                for io in &inputs {
                    let _ = writeln!(m, "    v{0} = in_v{0};", io.register);
                }
                if g.t.uses_vpos {
                    m += "    d3d_vpos = vec4<f32>(frag_pos.xy - vec2<f32>(0.5), 0.0, 1.0);\n";
                }
                if g.t.uses_vface {
                    m += "    d3d_vface = vec4<f32>(select(-1.0, 1.0, front));\n";
                }
                m += "    d3d_main();\n    var out: D3dPsOut;\n";
                for n in 0..4 {
                    if g.t.color_outputs & (1 << n) != 0 {
                        let _ = writeln!(m, "    out.c{n} = oC{n};");
                    }
                }
                if g.t.writes_depth {
                    m += "    out.depth = oDepth;\n";
                }
                if g.t.color_outputs == 0 && !g.t.writes_depth {
                    m += "    out.c0 = vec4<f32>(0.0);\n";
                }
                m += "    return out;\n}\n";
            }
        }
    }
    let (output_deps, input_roles) = dataflow(p);
    for io in &mut outputs {
        io.flags = output_deps.get(&io.register).copied().unwrap_or(0);
    }
    for io in &mut inputs {
        io.flags = input_roles.get(&io.register).copied().unwrap_or(0);
    }
    g.t.inputs = inputs;
    g.t.outputs = outputs;
    g.t.wgsl = m;
    Ok(g.t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp register reused for another input stops carrying the first.
    #[test]
    fn dataflow_follows_overwrites() {
        let reg = |t: u32, n: u32| (1u32 << 31) | ((t & 7) << 28) | ((t & 0x18) << 8) | n;
        let dst = |t: u32, n: u32, mask: u32| reg(t, n) | (mask << 16);
        let src = |t: u32, n: u32| reg(t, n) | (0xE4 << 16);
        let dcl = |usage: u32, index: u32, d: u32| [31 | (2 << 24), 0x8000_0000 | usage | (index << 16), d];
        let mov = |d: u32, s: u32| [1 | (2 << 24), d, s];
        let mut words = vec![0xFFFE_0300];
        words.extend(dcl(5, 0, dst(1, 1, 0xF))); // dcl_texcoord0 v1
        words.extend(dcl(3, 0, dst(1, 2, 0xF))); // dcl_normal0 v2
        words.extend(dcl(5, 0, dst(6, 1, 0xF))); // dcl_texcoord0 o1
        words.extend(dcl(5, 1, dst(6, 2, 0xF))); // dcl_texcoord1 o2
        words.extend(mov(dst(0, 0, 0xF), src(1, 2))); // mov r0, v2
        words.extend(mov(dst(6, 2, 0xF), src(0, 0))); // mov o2, r0
        words.extend(mov(dst(0, 0, 0x3), src(1, 1))); // mov r0.xy, v1
        words.extend(mov(dst(6, 1, 0x3), src(0, 0))); // mov o1.xy, r0
        words.push(0x0000_FFFF);
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let p = crate::d3d9::parse_program(&bytes).unwrap();
        let (outputs, _) = dataflow(&p);
        assert_eq!(outputs.get(&1), Some(&(1 << 1)), "o1 comes from the UV only");
        assert_eq!(outputs.get(&2), Some(&(1 << 2)), "o2 from the normal");
    }

    #[test]
    fn translates_sample_vs() {
        let p = crate::d3d9::parse_program(&crate::d3d9::tests::sample_vs()).unwrap();
        let t = translate(&p, &Options::default()).unwrap();
        assert!(t.wgsl.contains("fn vs_main("), "{}", t.wgsl);
        assert!(t.wgsl.contains("dot(v0, d3d_c.f[0])"), "{}", t.wgsl);
        assert!(t.wgsl.contains("D3D_C4"), "{}", t.wgsl);
        // Unwritten output components default as on D3D9 hardware.
        assert!(t.wgsl.contains(&format!("var<private> o0: vec4<f32> = {VS_OUTPUT_DEFAULT};")), "{}", t.wgsl);
        assert_eq!(t.outputs.len(), 1);
        assert_eq!(t.outputs[0].location, None);
    }
}
