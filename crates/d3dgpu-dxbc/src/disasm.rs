//! A readable listing of a decoded program (close to the syntax `fxc` and
//! vkd3d print), for debugging and test failure messages.

use crate::decode::*;

pub fn disassemble(p: &Program) -> String {
    let stage = match p.ty {
        ProgramType::Pixel => "ps",
        ProgramType::Vertex => "vs",
        ProgramType::Geometry => "gs",
        ProgramType::Hull => "hs",
        ProgramType::Domain => "ds",
        ProgramType::Compute => "cs",
    };
    let mut out = format!("{stage}_{}_{}\n", p.major, p.minor);
    let mut depth = 0usize;
    for ins in &p.instructions {
        if matches!(
            ins.op,
            Opcode::Else | Opcode::EndIf | Opcode::EndLoop | Opcode::EndSwitch | Opcode::Case | Opcode::Default
        ) {
            depth = depth.saturating_sub(1);
        }
        out += &"    ".repeat(depth);
        out += &instruction(ins);
        out.push('\n');
        if matches!(ins.op, Opcode::If | Opcode::Else | Opcode::Loop | Opcode::Switch | Opcode::Case | Opcode::Default)
        {
            depth += 1;
        }
    }
    out
}

pub fn instruction(ins: &Instruction) -> String {
    let mut s = ins.op.name().to_string();
    if matches!(
        ins.op,
        Opcode::If | Opcode::BreakC | Opcode::ContinueC | Opcode::RetC | Opcode::Discard | Opcode::CallC
    ) {
        s += if ins.test_nz { "_nz" } else { "_z" };
    }
    if ins.saturate && !ins.op.name().starts_with("dcl") {
        s += "_sat";
    }
    if ins.offsets != [0; 3] {
        s += &format!("({},{},{})", ins.offsets[0], ins.offsets[1], ins.offsets[2]);
    }
    if ins.op == Opcode::CustomData {
        return format!("{s} ({} dwords)", ins.data.len());
    }
    let ops: Vec<String> = ins.operands.iter().map(operand).collect();
    if !ops.is_empty() {
        s.push(' ');
        s += &ops.join(", ");
    }
    if !ins.extra.is_empty() {
        s += &format!(" {:?}", ins.extra);
    }
    s
}

const COMP: [char; 4] = ['x', 'y', 'z', 'w'];

pub fn operand(o: &Operand) -> String {
    let base = match o.ty {
        RegType::Immediate32 => {
            let v: Vec<String> = o
                .imm
                .iter()
                .map(|b| {
                    let f = f32::from_bits(*b);
                    if *b < 0x10000 || f.is_nan() || f.abs() > 1e20 {
                        format!("{b:#x}")
                    } else {
                        format!("{f:?}")
                    }
                })
                .collect();
            return format!("l({})", v.join(", "));
        }
        RegType::Temp => "r",
        RegType::Input => "v",
        RegType::Output => "o",
        RegType::IndexableTemp => "x",
        RegType::Sampler => "s",
        RegType::Resource => "t",
        RegType::ConstantBuffer => "cb",
        RegType::ImmediateConstantBuffer => "icb",
        RegType::Label => "l",
        RegType::Uav => "u",
        RegType::ThreadGroupSharedMemory => "g",
        RegType::Null => return "null".into(),
        RegType::OutputDepth => "oDepth",
        RegType::OutputCoverageMask => "oMask",
        RegType::InputPrimitiveId => "vPrim",
        RegType::InputThreadId => "vThreadID",
        RegType::InputThreadGroupId => "vThreadGroupID",
        RegType::InputThreadIdInGroup => "vThreadIDInGroup",
        RegType::InputThreadIdInGroupFlattened => "vThreadIDInGroupFlattened",
        RegType::InputCoverageMask => "vCoverage",
        _ => "?",
    };
    let mut s = base.to_string();
    for (i, idx) in o.indices.iter().enumerate() {
        let inner = match &idx.rel {
            Some(r) if idx.imm != 0 => format!("{} + {}", operand(r), idx.imm),
            Some(r) => operand(r),
            None => idx.imm.to_string(),
        };
        if i == 0 && idx.rel.is_none() && !matches!(o.ty, RegType::ImmediateConstantBuffer) {
            s += &inner;
        } else {
            s += &format!("[{inner}]");
        }
    }
    match o.select {
        Select::Mask(m) if m != 0xf => {
            s.push('.');
            s.extend((0..4).filter(|i| m & (1 << i) != 0).map(|i| COMP[i]));
        }
        Select::Swizzle(sw) => {
            s.push('.');
            s.extend(sw.iter().map(|c| COMP[*c as usize]));
        }
        Select::Scalar(c) if o.comps == 4 => {
            s.push('.');
            s.push(COMP[c as usize]);
        }
        _ => {}
    }
    match o.modifier {
        Modifier::None => s,
        Modifier::Neg => format!("-{s}"),
        Modifier::Abs => format!("|{s}|"),
        Modifier::AbsNeg => format!("-|{s}|"),
    }
}
