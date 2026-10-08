//! Direct3D 9 shader bytecode (shader models 2 and 3) and the Dunia shader
//! object container (`engine\shaders\obj\h??\x????????.vso|.pso`).
//!
//! Container: `u32 magic (0x1608068D)`, `u32 bytecode offset`, `u32 info`
//! (`record count << 8 | 4`), then `count` 8-byte records binding constant
//! names to registers: `u32 CRC-32 of the name`, `u8 kind` (low 6 bits: kind,
//! high 2 bits: low bits of the register), `u8 register / 4`, `u8 rows` (0 when
//! this program doesn't use it), `u8 columns`. The D3D9 token stream follows at
//! the bytecode offset.
//!
//! The token stream is the standard D3D9 format: a version token, then
//! instructions (opcode token, optional destination and source parameter
//! tokens), comments, and an end token.

use std::fmt;

use anyhow::{bail, Context, Result};

pub const CONTAINER_MAGIC: u32 = 0x1608_068D;

/// One named constant or sampler binding from the container header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstantBinding {
    pub name_hash: u32,
    /// Low 6 bits of the kind byte (8 = float, 9 = matrix rows, 12 = sampler, ...).
    pub kind: u8,
    /// Low two bits of the register index.
    pub component: u8,
    /// Register index divided by four.
    pub register: u8,
    /// Registers used by this program (0 when unused).
    pub rows: u8,
    pub columns: u8,
}

impl ConstantBinding {
    /// First `c#` (or `s#`) register of the binding.
    pub fn first_register(&self) -> u32 {
        (self.register as u32) << 2 | self.component as u32
    }

    pub fn is_sampler(&self) -> bool {
        self.kind & 0x0C == 0x0C
    }
}

/// A Dunia shader object: named bindings plus D3D9 bytecode.
#[derive(Debug, Clone)]
pub struct ShaderObject {
    pub bindings: Vec<ConstantBinding>,
    pub program: Program,
}

pub fn parse_object(data: &[u8]) -> Result<ShaderObject> {
    let u32_at = |o: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(data.get(o..o + 4).context("shader object truncated")?.try_into().unwrap()))
    };
    if u32_at(0)? != CONTAINER_MAGIC {
        bail!("not a shader object");
    }
    let code = u32_at(4)? as usize;
    let count = (u32_at(8)? >> 8) as usize;
    if 12 + count * 8 > code || code > data.len() {
        bail!("bad shader object header");
    }
    let bindings = (0..count)
        .map(|i| {
            let o = 12 + i * 8;
            Ok(ConstantBinding {
                name_hash: u32_at(o)?,
                kind: data[o + 4] & 0x3F,
                component: data[o + 4] >> 6,
                register: data[o + 5],
                rows: data[o + 6],
                columns: data[o + 7],
            })
        })
        .collect::<Result<_>>()?;
    Ok(ShaderObject { bindings, program: parse_program(&data[code..])? })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Pixel,
}

/// Register files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegType {
    Temp,
    Input,
    Const,
    /// `a0` in vertex shaders, `t#` in ps 1.x/2.0 pixel shaders.
    AddrOrTexture,
    /// `oPos` (0), `oFog` (1), `oPts` (2).
    RastOut,
    /// `oD#` (vs < 3.0).
    AttrOut,
    /// `oT#` (vs < 3.0) or `o#` (vs 3.0).
    Output,
    ConstInt,
    ColorOut,
    DepthOut,
    Sampler,
    Const2,
    Const3,
    Const4,
    ConstBool,
    Loop,
    TempFloat16,
    /// `vPos` (0) and `vFace` (1).
    MiscType,
    Label,
    Predicate,
    Unknown(u8),
}

impl RegType {
    fn from_bits(t: u8) -> RegType {
        use RegType::*;
        match t {
            0 => Temp,
            1 => Input,
            2 => Const,
            3 => AddrOrTexture,
            4 => RastOut,
            5 => AttrOut,
            6 => Output,
            7 => ConstInt,
            8 => ColorOut,
            9 => DepthOut,
            10 => Sampler,
            11 => Const2,
            12 => Const3,
            13 => Const4,
            14 => ConstBool,
            15 => Loop,
            16 => TempFloat16,
            17 => MiscType,
            18 => Label,
            19 => Predicate,
            n => Unknown(n),
        }
    }
}

fn reg_type(token: u32) -> RegType {
    RegType::from_bits((((token >> 28) & 7) | ((token >> 8) & 0x18)) as u8)
}

/// Source modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrcMod {
    None,
    Neg,
    Bias,
    BiasNeg,
    Sign,
    SignNeg,
    Comp,
    X2,
    X2Neg,
    Dz,
    Dw,
    Abs,
    AbsNeg,
    Not,
}

impl SrcMod {
    fn from_bits(b: u32) -> SrcMod {
        use SrcMod::*;
        match b {
            1 => Neg,
            2 => Bias,
            3 => BiasNeg,
            4 => Sign,
            5 => SignNeg,
            6 => Comp,
            7 => X2,
            8 => X2Neg,
            9 => Dz,
            10 => Dw,
            11 => Abs,
            12 => AbsNeg,
            13 => Not,
            _ => None,
        }
    }
}

/// Relative addressing: `register[addr.component]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelAddr {
    pub reg_type: RegType,
    pub index: u32,
    pub component: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Src {
    pub reg_type: RegType,
    pub index: u32,
    /// Component selected for each of x, y, z, w.
    pub swizzle: [u8; 4],
    pub modifier: SrcMod,
    pub relative: Option<RelAddr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dst {
    pub reg_type: RegType,
    pub index: u32,
    /// Bit i set when component i is written.
    pub write_mask: u8,
    pub saturate: bool,
    pub partial_precision: bool,
    pub centroid: bool,
    /// Signed shift scale (ps < 2.0).
    pub shift: i8,
    pub relative: Option<RelAddr>,
}

/// Declaration usage (`dcl_texcoord1` etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Usage {
    Position,
    BlendWeight,
    BlendIndices,
    Normal,
    PSize,
    TexCoord,
    Tangent,
    Binormal,
    TessFactor,
    PositionT,
    Color,
    Fog,
    Depth,
    Sample,
    Unknown(u8),
}

impl Usage {
    /// The D3DDECLUSAGE value.
    pub fn code(self) -> u8 {
        use Usage::*;
        match self {
            Position => 0,
            BlendWeight => 1,
            BlendIndices => 2,
            Normal => 3,
            PSize => 4,
            TexCoord => 5,
            Tangent => 6,
            Binormal => 7,
            TessFactor => 8,
            PositionT => 9,
            Color => 10,
            Fog => 11,
            Depth => 12,
            Sample => 13,
            Unknown(n) => n,
        }
    }

    pub fn from_code(c: u8) -> Usage {
        Usage::from_bits(c as u32)
    }

    fn from_bits(b: u32) -> Usage {
        use Usage::*;
        match b {
            0 => Position,
            1 => BlendWeight,
            2 => BlendIndices,
            3 => Normal,
            4 => PSize,
            5 => TexCoord,
            6 => Tangent,
            7 => Binormal,
            8 => TessFactor,
            9 => PositionT,
            10 => Color,
            11 => Fog,
            12 => Depth,
            13 => Sample,
            n => Unknown(n as u8),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureType {
    Unknown,
    Tex2D,
    Cube,
    Volume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decl {
    /// Input or output register with a semantic.
    Semantic { usage: Usage, index: u8 },
    Sampler(TextureType),
    /// `dcl` without a semantic (ps 2.0 `v#`/`t#`, or `vPos`/`vFace`).
    Plain,
}

/// Comparison for `ifc`, `breakc` and `setp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Gt,
    Eq,
    Ge,
    Lt,
    Ne,
    Le,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Nop,
    Mov,
    Add,
    Sub,
    Mad,
    Mul,
    Rcp,
    Rsq,
    Dp3,
    Dp4,
    Min,
    Max,
    Slt,
    Sge,
    Exp,
    Log,
    Lit,
    Dst,
    Lrp,
    Frc,
    M4x4,
    M4x3,
    M3x4,
    M3x3,
    M3x2,
    Call,
    CallNz,
    Loop,
    Ret,
    EndLoop,
    Label,
    Dcl(Decl),
    Pow,
    Crs,
    Sgn,
    Abs,
    Nrm,
    SinCos,
    Rep,
    EndRep,
    If,
    Ifc(Cmp),
    Else,
    EndIf,
    Break,
    Breakc(Cmp),
    Mova,
    DefB(bool),
    DefI([i32; 4]),
    Def([f32; 4]),
    TexKill,
    /// `texld` (0), `texldp` (1) or `texldb` (2).
    Tex(u8),
    ExpP,
    LogP,
    Cnd,
    Cmp,
    Dp2Add,
    Dsx,
    Dsy,
    TexLdd,
    Setp(Cmp),
    TexLdl,
    BreakP,
    /// Opcodes this translator does not support (ps 1.x texture ops etc.).
    Other(u16),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Instr {
    pub op: Op,
    pub dst: Option<Dst>,
    pub src: Vec<Src>,
    /// Predicate register source when the instruction is predicated.
    pub predicate: Option<Src>,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub stage: Stage,
    pub major: u8,
    pub minor: u8,
    pub instrs: Vec<Instr>,
}

fn cmp_from(control: u32) -> Result<Cmp> {
    Ok(match control {
        1 => Cmp::Gt,
        2 => Cmp::Eq,
        3 => Cmp::Ge,
        4 => Cmp::Lt,
        5 => Cmp::Ne,
        6 => Cmp::Le,
        c => bail!("bad comparison {c}"),
    })
}

struct Tokens<'a> {
    t: &'a [u32],
    pos: usize,
}

impl Tokens<'_> {
    fn next(&mut self) -> Result<u32> {
        let v = *self.t.get(self.pos).context("bytecode truncated")?;
        self.pos += 1;
        Ok(v)
    }
}

fn parse_rel(tokens: &mut Tokens, major: u8, stage: Stage) -> Result<RelAddr> {
    if major >= 3 || (stage == Stage::Vertex && major >= 2) {
        let t = tokens.next()?;
        Ok(RelAddr { reg_type: reg_type(t), index: t & 0x7FF, component: ((t >> 16) & 3) as u8 })
    } else {
        Ok(RelAddr { reg_type: RegType::AddrOrTexture, index: 0, component: 0 })
    }
}

fn parse_src(tokens: &mut Tokens, major: u8, stage: Stage) -> Result<Src> {
    let t = tokens.next()?;
    let sw = (t >> 16) & 0xFF;
    let relative = if t & (1 << 13) != 0 { Some(parse_rel(tokens, major, stage)?) } else { None };
    Ok(Src {
        reg_type: reg_type(t),
        index: t & 0x7FF,
        swizzle: [(sw & 3) as u8, ((sw >> 2) & 3) as u8, ((sw >> 4) & 3) as u8, ((sw >> 6) & 3) as u8],
        modifier: SrcMod::from_bits((t >> 24) & 0xF),
        relative,
    })
}

fn parse_dst(tokens: &mut Tokens, major: u8, stage: Stage) -> Result<Dst> {
    let t = tokens.next()?;
    let relative = if t & (1 << 13) != 0 { Some(parse_rel(tokens, major, stage)?) } else { None };
    let shift = ((t >> 24) & 0xF) as i8;
    Ok(Dst {
        reg_type: reg_type(t),
        index: t & 0x7FF,
        write_mask: ((t >> 16) & 0xF) as u8,
        saturate: t & (1 << 20) != 0,
        partial_precision: t & (1 << 21) != 0,
        centroid: t & (1 << 22) != 0,
        shift: if shift >= 8 { shift - 16 } else { shift },
        relative,
    })
}

/// Parses a D3D9 token stream (from its version token).
pub fn parse_program(code: &[u8]) -> Result<Program> {
    if code.len() < 8 || code.len() % 4 != 0 {
        bail!("bytecode length {} is not a whole number of tokens", code.len());
    }
    let words: Vec<u32> = code.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    let version = words[0];
    let stage = match version >> 16 {
        0xFFFE => Stage::Vertex,
        0xFFFF => Stage::Pixel,
        v => bail!("bad version token {v:#x}"),
    };
    let (major, minor) = (((version >> 8) & 0xFF) as u8, (version & 0xFF) as u8);
    if major < 2 {
        bail!("shader model {major}.{minor} is not supported");
    }
    let mut tokens = Tokens { t: &words, pos: 1 };
    let mut instrs = Vec::new();
    loop {
        let token = tokens.next()?;
        let opcode = (token & 0xFFFF) as u16;
        if opcode == 0xFFFF {
            break;
        }
        if opcode == 0xFFFE {
            tokens.pos += ((token >> 16) & 0x7FFF) as usize;
            continue;
        }
        let length = ((token >> 24) & 0xF) as usize;
        let control = (token >> 16) & 0xFF;
        let predicated = token & (1 << 28) != 0;
        let start = tokens.pos;
        let mut dst = None;
        let mut src = Vec::new();
        let mut predicate = None;
        // Operand layout per opcode: (has destination, source count).
        let (op, has_dst, nsrc): (Op, bool, usize) = match opcode {
            0 => (Op::Nop, false, 0),
            1 => (Op::Mov, true, 1),
            2 => (Op::Add, true, 2),
            3 => (Op::Sub, true, 2),
            4 => (Op::Mad, true, 3),
            5 => (Op::Mul, true, 2),
            6 => (Op::Rcp, true, 1),
            7 => (Op::Rsq, true, 1),
            8 => (Op::Dp3, true, 2),
            9 => (Op::Dp4, true, 2),
            10 => (Op::Min, true, 2),
            11 => (Op::Max, true, 2),
            12 => (Op::Slt, true, 2),
            13 => (Op::Sge, true, 2),
            14 => (Op::Exp, true, 1),
            15 => (Op::Log, true, 1),
            16 => (Op::Lit, true, 1),
            17 => (Op::Dst, true, 2),
            18 => (Op::Lrp, true, 3),
            19 => (Op::Frc, true, 1),
            20 => (Op::M4x4, true, 2),
            21 => (Op::M4x3, true, 2),
            22 => (Op::M3x4, true, 2),
            23 => (Op::M3x3, true, 2),
            24 => (Op::M3x2, true, 2),
            25 => (Op::Call, false, 1),
            26 => (Op::CallNz, false, 2),
            27 => (Op::Loop, false, 2),
            28 => (Op::Ret, false, 0),
            29 => (Op::EndLoop, false, 0),
            30 => (Op::Label, false, 1),
            31 => {
                let usage = tokens.next()?;
                let d = parse_dst(&mut tokens, major, stage)?;
                let decl = if d.reg_type == RegType::Sampler {
                    Decl::Sampler(match (usage >> 27) & 0xF {
                        2 => TextureType::Tex2D,
                        3 => TextureType::Cube,
                        4 => TextureType::Volume,
                        _ => TextureType::Unknown,
                    })
                } else if major >= 3 || stage == Stage::Vertex {
                    Decl::Semantic { usage: Usage::from_bits(usage & 0x1F), index: ((usage >> 16) & 0xF) as u8 }
                } else {
                    Decl::Plain
                };
                dst = Some(d);
                (Op::Dcl(decl), false, 0)
            }
            32 => (Op::Pow, true, 2),
            33 => (Op::Crs, true, 2),
            34 => (Op::Sgn, true, 3),
            35 => (Op::Abs, true, 1),
            36 => (Op::Nrm, true, 1),
            37 => (Op::SinCos, true, if major >= 3 { 1 } else { 3 }),
            38 => (Op::Rep, false, 1),
            39 => (Op::EndRep, false, 0),
            40 => (Op::If, false, 1),
            41 => (Op::Ifc(cmp_from(control)?), false, 2),
            42 => (Op::Else, false, 0),
            43 => (Op::EndIf, false, 0),
            44 => (Op::Break, false, 0),
            45 => (Op::Breakc(cmp_from(control)?), false, 2),
            46 => (Op::Mova, true, 1),
            47 => {
                dst = Some(parse_dst(&mut tokens, major, stage)?);
                (Op::DefB(tokens.next()? != 0), false, 0)
            }
            48 => {
                dst = Some(parse_dst(&mut tokens, major, stage)?);
                let mut v = [0i32; 4];
                for x in &mut v {
                    *x = tokens.next()? as i32;
                }
                (Op::DefI(v), false, 0)
            }
            65 => (Op::TexKill, true, 0),
            66 => (Op::Tex(control as u8), true, 2),
            78 => (Op::ExpP, true, 1),
            79 => (Op::LogP, true, 1),
            80 => (Op::Cnd, true, 3),
            81 => {
                dst = Some(parse_dst(&mut tokens, major, stage)?);
                let mut v = [0f32; 4];
                for x in &mut v {
                    *x = f32::from_bits(tokens.next()?);
                }
                (Op::Def(v), false, 0)
            }
            88 => (Op::Cmp, true, 3),
            90 => (Op::Dp2Add, true, 3),
            91 => (Op::Dsx, true, 1),
            92 => (Op::Dsy, true, 1),
            93 => (Op::TexLdd, true, 4),
            94 => (Op::Setp(cmp_from(control)?), true, 2),
            95 => (Op::TexLdl, true, 2),
            96 => (Op::BreakP, false, 1),
            other => {
                // Skip unsupported instructions by their encoded length.
                tokens.pos = start + length;
                instrs.push(Instr { op: Op::Other(other), dst: None, src: vec![], predicate: None });
                continue;
            }
        };
        if has_dst {
            dst = Some(parse_dst(&mut tokens, major, stage)?);
        }
        if predicated {
            predicate = Some(parse_src(&mut tokens, major, stage)?);
        }
        for _ in 0..nsrc {
            src.push(parse_src(&mut tokens, major, stage)?);
        }
        if length != 0 && tokens.pos != start + length {
            bail!("operand count mismatch for opcode {opcode} ({} tokens read, {length} encoded)", tokens.pos - start);
        }
        instrs.push(Instr { op, dst, src, predicate });
    }
    Ok(Program { stage, major, minor, instrs })
}

// ------------------------------------------------------------ disassembly ---

fn reg_name(t: RegType, index: u32, stage: Stage) -> String {
    use RegType::*;
    match t {
        Temp => format!("r{index}"),
        Input => format!("v{index}"),
        Const => format!("c{index}"),
        Const2 => format!("c{}", index + 2048),
        Const3 => format!("c{}", index + 4096),
        Const4 => format!("c{}", index + 6144),
        AddrOrTexture => {
            if stage == Stage::Vertex {
                format!("a{index}")
            } else {
                format!("t{index}")
            }
        }
        RastOut => ["oPos", "oFog", "oPts"].get(index as usize).unwrap_or(&"oRast?").to_string(),
        AttrOut => format!("oD{index}"),
        Output => format!("o{index}"),
        ConstInt => format!("i{index}"),
        ColorOut => format!("oC{index}"),
        DepthOut => "oDepth".into(),
        Sampler => format!("s{index}"),
        ConstBool => format!("b{index}"),
        Loop => "aL".into(),
        TempFloat16 => format!("half{index}"),
        MiscType => if index == 0 { "vPos" } else { "vFace" }.into(),
        Label => format!("l{index}"),
        Predicate => format!("p{index}"),
        Unknown(n) => format!("?{n}_{index}"),
    }
}

const COMP: [char; 4] = ['x', 'y', 'z', 'w'];

impl Program {
    fn fmt_src(&self, s: &Src) -> String {
        let mut r = reg_name(s.reg_type, s.index, self.stage);
        if let Some(rel) = s.relative {
            r = format!("{}[{}.{} + {}]", &r[..1], reg_name(rel.reg_type, rel.index, self.stage), COMP[rel.component as usize], s.index);
        }
        if s.swizzle != [0, 1, 2, 3] {
            r.push('.');
            if s.swizzle.iter().all(|&c| c == s.swizzle[0]) {
                r.push(COMP[s.swizzle[0] as usize]);
            } else {
                r.extend(s.swizzle.iter().map(|&c| COMP[c as usize]));
            }
        }
        match s.modifier {
            SrcMod::None => r,
            SrcMod::Neg => format!("-{r}"),
            SrcMod::Abs => format!("{r}_abs"),
            SrcMod::AbsNeg => format!("-{r}_abs"),
            SrcMod::Not => format!("!{r}"),
            m => format!("{r}_{m:?}"),
        }
    }

    fn fmt_dst(&self, d: &Dst) -> String {
        let mut r = reg_name(d.reg_type, d.index, self.stage);
        if d.write_mask != 0xF {
            r.push('.');
            r.extend((0..4).filter(|i| d.write_mask & (1 << i) != 0).map(|i| COMP[i]));
        }
        r
    }
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = if self.stage == Stage::Vertex { "vs" } else { "ps" };
        writeln!(f, "{prefix}_{}_{}", self.major, self.minor)?;
        for i in &self.instrs {
            let mut name = format!("{:?}", i.op).to_ascii_lowercase();
            if let Op::Dcl(d) = &i.op {
                name = match d {
                    Decl::Semantic { usage, index } => format!("dcl_{:?}{index}", usage).to_ascii_lowercase(),
                    Decl::Sampler(t) => format!("dcl_{:?}", t).to_ascii_lowercase(),
                    Decl::Plain => "dcl".into(),
                };
            }
            if let Some(d) = &i.dst {
                if d.saturate {
                    name += "_sat";
                }
            }
            let mut ops: Vec<String> = Vec::new();
            if let Some(d) = &i.dst {
                ops.push(self.fmt_dst(d));
            }
            ops.extend(i.src.iter().map(|s| self.fmt_src(s)));
            let pred = i.predicate.as_ref().map(|p| format!("({}) ", self.fmt_src(p))).unwrap_or_default();
            writeln!(f, "    {pred}{name} {}", ops.join(", "))?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encodes a small vs_3_0 program by hand: `dcl_position v0`,
    /// `dcl_position o0`, `def c4, 1, 0, 0, 1`, `dp4 o0.x, v0, c0`, `mov o0.yzw, c4`.
    pub fn sample_vs() -> Vec<u8> {
        let reg = |t: u32, n: u32| (1u32 << 31) | ((t & 7) << 28) | ((t & 0x18) << 8) | n;
        let dst = |t: u32, n: u32, mask: u32| reg(t, n) | (mask << 16);
        let src = |t: u32, n: u32| reg(t, n) | (0xE4 << 16);
        let words: Vec<u32> = vec![
            0xFFFE_0300,
            31 | (2 << 24),
            0x8000_0000, // usage position 0
            dst(1, 0, 0xF),
            31 | (2 << 24),
            0x8000_0000,
            dst(6, 0, 0xF),
            81 | (5 << 24),
            dst(2, 4, 0xF),
            1.0f32.to_bits(),
            0,
            0,
            1.0f32.to_bits(),
            9 | (3 << 24),
            dst(6, 0, 0x1),
            src(1, 0),
            src(2, 0),
            1 | (2 << 24),
            dst(6, 0, 0xE),
            src(2, 4),
            0x0000_FFFF,
        ];
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn parses_tokens() {
        let p = parse_program(&sample_vs()).unwrap();
        assert_eq!(p.stage, Stage::Vertex);
        assert_eq!(p.major, 3);
        assert_eq!(p.instrs.len(), 5);
        assert_eq!(p.instrs[0].op, Op::Dcl(Decl::Semantic { usage: Usage::Position, index: 0 }));
        assert_eq!(p.instrs[2].op, Op::Def([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(p.instrs[3].op, Op::Dp4);
        assert_eq!(p.instrs[3].dst.unwrap().write_mask, 1);
        assert_eq!(p.instrs[3].src[1].reg_type, RegType::Const);
        let text = p.to_string();
        assert!(text.contains("dp4 o0.x, v0, c0"), "{text}");
    }

    #[test]
    fn parses_container() {
        let mut d = Vec::new();
        d.extend_from_slice(&CONTAINER_MAGIC.to_le_bytes());
        d.extend_from_slice(&20u32.to_le_bytes());
        d.extend_from_slice(&((1u32 << 8) | 4).to_le_bytes());
        d.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        d.extend_from_slice(&[0x09, 0, 4, 4]);
        d.extend_from_slice(&sample_vs());
        let o = parse_object(&d).unwrap();
        assert_eq!(o.bindings[0], ConstantBinding { name_hash: 0xDEAD_BEEF, kind: 9, component: 0, register: 0, rows: 4, columns: 4 });
        assert_eq!(o.program.instrs.len(), 5);
    }
}
