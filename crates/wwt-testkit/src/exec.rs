//! Runs cases through the translator and wasmtime.

use anyhow::{anyhow, bail, Result};
use wasmtime::{
    Caller, Config, Engine, Extern, Func, Global, GlobalType, Instance, MemoryTypeBuilder, Module,
    Mutability, Ref, RefType, SharedMemory, Store, Table, TableType, Val, ValType,
};
use wwt::abi::{addr::NULL_LIMIT, cpu, cpu64, fault, flags as fl, store_map};

use crate::case::{hex, mem_diff, Case, Outcome};
use crate::layout::*;

#[derive(Default)]
struct State {
    fault: Option<(u32, u32, u32)>,
}

pub struct Executor {
    engine: Engine,
}

/// Result of running one case through the translator.
pub enum Run {
    Ok(Outcome),
    /// The translator could not lift the instruction.
    Unsupported(String),
}

/// Where the runtime tables and the CPU struct live. With a 64-bit memory
/// the lookup's first level has 8-byte entries, so everything after it
/// moves up.
struct Native {
    l1: u32,
    zero_l2: u32,
    store_map: u32,
    cpu: u32,
    pages: u64,
}

impl Native {
    fn new(mem64: bool) -> Native {
        if !mem64 {
            return Native {
                l1: L1,
                zero_l2: ZERO_L2,
                store_map: STORE_MAP,
                cpu: CPU,
                pages: MEMORY_PAGES,
            };
        }
        let zero_l2 = NATIVE_BASE + 0x80_0000;
        let store_map = zero_l2 + 0x4000;
        let cpu = store_map + 0x10_0000;
        Native {
            l1: NATIVE_BASE,
            zero_l2,
            store_map,
            cpu,
            pages: (cpu as u64 + 0x1_0000) / 65536 + 1,
        }
    }
}

fn mem_ptr(m: &SharedMemory) -> *mut u8 {
    m.data().as_ptr() as *mut u8
}

fn w32(m: &SharedMemory, addr: u32, v: u32) {
    unsafe { std::ptr::write_unaligned(mem_ptr(m).add(addr as usize) as *mut u32, v) }
}

fn r32(m: &SharedMemory, addr: u32) -> u32 {
    unsafe { std::ptr::read_unaligned(mem_ptr(m).add(addr as usize) as *const u32) }
}

fn w64(m: &SharedMemory, addr: u32, v: u64) {
    unsafe { std::ptr::write_unaligned(mem_ptr(m).add(addr as usize) as *mut u64, v) }
}

fn r64(m: &SharedMemory, addr: u32) -> u64 {
    unsafe { std::ptr::read_unaligned(mem_ptr(m).add(addr as usize) as *const u64) }
}

fn write_bytes(m: &SharedMemory, addr: u32, b: &[u8]) {
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), mem_ptr(m).add(addr as usize), b.len()) }
}

fn read_bytes(m: &SharedMemory, addr: u32, n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    unsafe { std::ptr::copy_nonoverlapping(mem_ptr(m).add(addr as usize), v.as_mut_ptr(), n) }
    v
}

impl Executor {
    pub fn new() -> Result<Executor> {
        let mut c = Config::new();
        c.wasm_threads(true);
        c.wasm_tail_call(true);
        c.wasm_simd(true);
        c.wasm_bulk_memory(true);
        c.wasm_memory64(true);
        Ok(Executor {
            engine: Engine::new(&c)?,
        })
    }

    /// Translates and runs a batch of cases. The cases must all be 32-bit
    /// or all x86-64; the translator's mode follows them.
    pub fn run(&self, cases: &[Case], cfg: &wwt::Config) -> Result<Vec<Run>> {
        let x64 = cases.first().is_some_and(|c| c.x64);
        if cases.iter().any(|c| c.x64 != x64) {
            bail!("a batch mixes 32-bit and 64-bit cases");
        }
        let mode = if x64 {
            wwt::ir::Mode::X64
        } else {
            wwt::ir::Mode::X86
        };
        let cfg = &cfg.clone().with_mode(mode);
        let mem64 = cfg.codegen.mem64;
        // x86-64 code on a 64-bit memory has 64-bit code addresses.
        let code64 = x64 && mem64;
        let nat = Native::new(mem64);
        let cpu_base = nat.cpu;
        // Translate each case; unsupported ones are reported, not run.
        let mut funcs = vec![];
        let mut slot = vec![None; cases.len()];
        let mut results: Vec<Option<Run>> = (0..cases.len()).map(|_| None).collect();
        for (i, c) in cases.iter().enumerate() {
            let (f, unsupported) =
                wwt::translate::translate_snippet(&c.code_bytes(), INS as u64, cfg);
            if let Some((_, why)) = unsupported.first() {
                results[i] = Some(Run::Unsupported(why.clone()));
                continue;
            }
            slot[i] = Some(funcs.len());
            funcs.push(f);
        }
        let wasm = wwt::translate::build_module(&funcs, cfg);
        let module = Module::new(&self.engine, &wasm)?;

        let memory = SharedMemory::new(
            &self.engine,
            MemoryTypeBuilder::new()
                .min(nat.pages)
                .max(Some(nat.pages))
                .shared(true)
                .memory64(mem64)
                .build()?,
        )?;
        let mut store = Store::new(&self.engine, State::default());
        let table = Table::new(
            &mut store,
            TableType::new(RefType::FUNCREF, 1 + funcs.len() as u32, None),
            Ref::Func(None),
        )?;
        // Table slot 0: a lookup miss just reports the target address. The
        // host functions take the CPU pointer and native addresses as i64
        // with a 64-bit memory.
        let miss = {
            let mem = memory.clone();
            if code64 {
                Func::wrap(&mut store, move |cpu_ptr: i64| -> i64 {
                    r64(&mem, cpu_ptr as u32 + cpu64::RIP) as i64
                })
            } else if mem64 {
                Func::wrap(&mut store, move |cpu_ptr: i64| -> i32 {
                    r32(&mem, cpu_ptr as u32 + cpu::EIP) as i32
                })
            } else {
                Func::wrap(&mut store, move |cpu_ptr: i32| -> i32 {
                    r32(&mem, cpu_ptr as u32 + cpu::EIP) as i32
                })
            }
        };
        table.set(&mut store, 0, Ref::Func(Some(miss)))?;
        // The fault import returns where to continue; the tests stop at the
        // first fault instead.
        let on_fault = |mut caller: Caller<'_, State>, code: i32, eip: i32, info: i32| {
            caller.data_mut().fault = Some((code as u32, eip as u32, info as u32));
            anyhow!("guest fault")
        };
        let fault_fn = if code64 {
            Func::wrap(
                &mut store,
                move |caller: Caller<'_, State>,
                      _cpu: i64,
                      code: i32,
                      eip: i64,
                      info: i64|
                      -> Result<i64> {
                    Err(on_fault(caller, code, eip as i32, info as i32))
                },
            )
        } else if mem64 {
            Func::wrap(
                &mut store,
                move |caller: Caller<'_, State>,
                      _cpu: i64,
                      code: i32,
                      eip: i32,
                      info: i32|
                      -> Result<i32> { Err(on_fault(caller, code, eip, info)) },
            )
        } else {
            Func::wrap(
                &mut store,
                move |caller: Caller<'_, State>,
                      _cpu: i32,
                      code: i32,
                      eip: i32,
                      info: i32|
                      -> Result<i32> { Err(on_fault(caller, code, eip, info)) },
            )
        };
        let math = Func::wrap(&mut store, |op: i32, a: f64, b: f64| -> f64 {
            host_math(op as u32, a, b)
        });
        let sin = Func::wrap(&mut store, |a: f64| -> f64 { a.sin() });
        let cos = Func::wrap(&mut store, |a: f64| -> f64 { a.cos() });
        // One thread: a loop's slice deadline never passes (PREEMPT_AT is
        // u32::MAX against a tick that stays 0), and nothing switches.
        let preempt = match (mem64, code64) {
            (_, true) => Func::wrap(&mut store, |_cpu: i64, _eip: i64| -> i32 { 0 }),
            (true, false) => Func::wrap(&mut store, |_cpu: i64, _eip: i32| -> i32 { 0 }),
            _ => Func::wrap(&mut store, |_cpu: i32, _eip: i32| -> i32 { 0 }),
        };
        // Address-typed globals (i64 with a 64-bit memory).
        let g = |store: &mut Store<State>, v: u32, wide: bool| {
            let (ty, val) = if wide {
                (ValType::I64, Val::I64(v as i64))
            } else {
                (ValType::I32, Val::I32(v as i32))
            };
            Global::new(store, GlobalType::new(ty, Mutability::Const), val)
        };
        // The lookup's first level points every page at an empty second level.
        for p in 0..(1u32 << 20) {
            if mem64 {
                w64(&memory, nat.l1 + p * 8, nat.zero_l2 as u64);
            } else {
                w32(&memory, nat.l1 + p * 4, nat.zero_l2);
            }
        }
        // Stores to the null region and from the last guest page up take
        // the precise check.
        let edge = |p: u32| !(NULL_LIMIT >> 12..(NATIVE_BASE >> 12) - 1).contains(&p);
        let map: Vec<u8> = (0..1u32 << 20)
            .map(|p| if edge(p) { store_map::EDGE } else { 0 })
            .collect();
        write_bytes(&memory, nat.store_map, &map);
        let imports: Vec<Extern> = module
            .imports()
            .map(|imp| -> Result<Extern> {
                Ok(match imp.name() {
                    "memory" => memory.clone().into(),
                    "table" => table.into(),
                    "table_base" => g(&mut store, 1, false)?.into(),
                    "lookup_l1" => g(&mut store, nat.l1, mem64)?.into(),
                    "guest_limit" => g(&mut store, NATIVE_BASE - 0x10000 - 16, mem64)?.into(),
                    "store_map" => g(&mut store, nat.store_map, mem64)?.into(),
                    "zero_l2" => g(&mut store, nat.zero_l2, mem64)?.into(),
                    "code_pages" => g(&mut store, (1 << 20) - 1, true)?.into(),
                    "fault" => fault_fn.into(),
                    "math" => math.into(),
                    "sin" => sin.into(),
                    "cos" => cos.into(),
                    "preempt" => preempt.into(),
                    "tick" => g(&mut store, nat.zero_l2, mem64)?.into(),
                    n => return Err(anyhow!("unexpected import {n}")),
                })
            })
            .collect::<Result<_>>()?;
        Instance::new(&mut store, &module, &imports)?;

        let cpu_size = if x64 { cpu64::SIZE } else { cpu::SIZE };
        for (i, c) in cases.iter().enumerate() {
            let Some(k) = slot[i] else { continue };
            let window = c.window();
            write_bytes(&memory, MEM_BASE, &vec![0u8; 0x1000]);
            write_bytes(&memory, MEM_BASE, &window);
            write_bytes(&memory, cpu_base, &vec![0u8; cpu_size as usize]);
            for (r, &v) in c.regs.iter().enumerate() {
                if x64 {
                    w64(&memory, cpu_base + cpu64::gpr(r as u32), v);
                } else {
                    w32(&memory, cpu_base + cpu::gpr(r as u32), v as u32);
                }
            }
            w32(&memory, cpu_base + cpu::FK, fl::EXPLICIT);
            if x64 {
                w64(&memory, cpu_base + cpu64::FR, (c.eflags & fl::ARITH) as u64);
            } else {
                w32(&memory, cpu_base + cpu::FR, c.eflags & fl::ARITH);
            }
            w32(&memory, cpu_base + cpu::DF, c.eflags >> 10 & 1);
            w32(&memory, cpu_base + cpu::EFLAGS_SYS, 0x200);
            w32(&memory, cpu_base + cpu::PREEMPT_AT, u32::MAX);
            let fx = c.fx_bytes();
            load_fx(&memory, cpu_base, &fx, x64);
            let simd = is_simd_case(c);
            store.data_mut().fault = None;
            let func = table
                .get(&mut store, 1 + k as u64)
                .and_then(|r| r.as_func().flatten().copied())
                .ok_or_else(|| anyhow!("missing table entry"))?;
            let r = if code64 {
                func.typed::<i64, i64>(&store)?
                    .call(&mut store, cpu_base as i64)
                    .map(|n| n as i32)
            } else if mem64 {
                func.typed::<i64, i32>(&store)?
                    .call(&mut store, cpu_base as i64)
            } else {
                func.typed::<i32, i32>(&store)?
                    .call(&mut store, cpu_base as i32)
            };
            let st = |o| r32(&memory, cpu_base + o);
            let (regs, arith) = if x64 {
                let st64 = |o| r64(&memory, cpu_base + o);
                let regs = (0..16).map(|n| st64(cpu64::gpr(n))).collect();
                let arith = wwt::flags::eflags_of_state64(
                    st(cpu::FK),
                    st64(cpu64::FR),
                    st64(cpu64::FA),
                    st64(cpu64::FB),
                    st64(cpu64::FC),
                );
                (regs, arith)
            } else {
                let regs = (0..8).map(|n| st(cpu::gpr(n)) as u64).collect();
                let arith = wwt::flags::eflags_of_state(
                    st(cpu::FK),
                    st(cpu::FR),
                    st(cpu::FA),
                    st(cpu::FB),
                    st(cpu::FC),
                );
                (regs, arith)
            };
            let eflags = arith | st(cpu::DF) << 10 | st(cpu::EFLAGS_SYS) | 2;
            let (eip, fault_kind) = match r {
                Ok(next) => (next as u32, String::new()),
                Err(e) => match store.data().fault {
                    Some((code, _, _)) => (
                        0,
                        match code {
                            fault::INTEGER_DIVIDE_BY_ZERO | fault::INTEGER_OVERFLOW => "DE",
                            fault::ILLEGAL_INSTRUCTION => "UD",
                            fault::BREAKPOINT => "BP",
                            fault::ACCESS_VIOLATION
                            | fault::ACCESS_VIOLATION_WRITE
                            | fault::ACCESS_VIOLATION_EXECUTE
                            | fault::GENERAL_PROTECTION
                            | fault::PRIVILEGED_INSTRUCTION => "AV",
                            fault::UNSUPPORTED => "UNSUPPORTED",
                            _ => "??",
                        }
                        .to_string(),
                    ),
                    None => (0, format!("trap: {e:#}")),
                },
            };
            let mem = read_bytes(&memory, MEM_BASE, MEM_SIZE);
            results[i] = Some(Run::Ok(Outcome {
                regs,
                eflags,
                eip,
                fault: fault_kind,
                mem: mem_diff(&window, &mem),
                fx: c
                    .fx
                    .as_ref()
                    .map(|_| hex(&save_fx(&memory, cpu_base, simd, x64))),
            }));
        }
        Ok(results.into_iter().map(|r| r.unwrap()).collect())
    }
}

/// Whether a case exercises MMX/SSE (its x87 slots hold MMX registers).
pub fn is_simd_case(c: &Case) -> bool {
    let code = c.code_bytes();
    let mut d = iced_x86::Decoder::with_ip(
        c.bitness(),
        &code,
        INS as u64,
        iced_x86::DecoderOptions::NONE,
    );
    let i = d.decode();
    use iced_x86::CpuidFeature as F;
    i.cpuid_features()
        .iter()
        .any(|f| matches!(f, F::MMX | F::SSE | F::SSE2))
}

/// Converts an FXSAVE image into the translator's CPU state at `base`
/// (with xmm8-15 for x86-64).
fn load_fx(m: &SharedMemory, base: u32, fx: &[u8], x64: bool) {
    let fcw = u16::from_le_bytes([fx[0], fx[1]]);
    let fsw = u16::from_le_bytes([fx[2], fx[3]]);
    w32(m, base + cpu::FPU_CW, fcw as u32);
    // Status word without top; top kept separately.
    let top = (fsw >> 11) & 7;
    w32(m, base + cpu::FPU_SW, (fsw & !0x3800) as u32);
    w32(m, base + cpu::FPU_TOP, top as u32);
    // Abridged tag word: 1 bit per physical register (1 = valid).
    w32(m, base + cpu::FPU_TAG, fx[4] as u32);
    for i in 0..8 {
        // FXSAVE stores ST(i) relative to top; we keep physical registers.
        let st = &fx[32 + i * 16..32 + i * 16 + 10];
        let phys = (top as usize + i) & 7;
        let v = wwt::fpu::f80_to_f64(st);
        write_bytes(m, base + cpu::FPU_ST + phys as u32 * 8, &v.to_le_bytes());
        // MMX register `phys` is the mantissa of the same physical register.
        write_bytes(m, base + cpu::MMX + phys as u32 * 8, &st[0..8]);
    }
    w32(
        m,
        base + cpu::MXCSR,
        u32::from_le_bytes(fx[24..28].try_into().unwrap()),
    );
    for i in 0..if x64 { 16 } else { 8 } {
        write_bytes(
            m,
            base + xmm_home(i),
            &fx[160 + i as usize * 16..176 + i as usize * 16],
        );
    }
}

/// Offset of xmm`i` in the CPU struct.
fn xmm_home(i: u32) -> u32 {
    if i < 8 {
        cpu::XMM + i * 16
    } else {
        cpu64::XMM8 + (i - 8) * 16
    }
}

/// Builds an FXSAVE image from the translator's CPU state at `base`.
fn save_fx(m: &SharedMemory, base: u32, simd: bool, x64: bool) -> Vec<u8> {
    let mut fx = vec![0u8; 512];
    let top = r32(m, base + cpu::FPU_TOP) & 7;
    let fsw = (r32(m, base + cpu::FPU_SW) & !0x3800) | top << 11;
    fx[0..2].copy_from_slice(&(r32(m, base + cpu::FPU_CW) as u16).to_le_bytes());
    fx[2..4].copy_from_slice(&(fsw as u16).to_le_bytes());
    fx[4] = r32(m, base + cpu::FPU_TAG) as u8;
    for i in 0..8u32 {
        let phys = (top + i) & 7;
        let b = read_bytes(m, base + cpu::FPU_ST + phys * 8, 8);
        let v = f64::from_le_bytes(b.try_into().unwrap());
        fx[32 + i as usize * 16..42 + i as usize * 16].copy_from_slice(&wwt::fpu::f64_to_f80(v));
        if simd {
            let mm = read_bytes(m, base + cpu::MMX + phys * 8, 8);
            fx[32 + i as usize * 16..40 + i as usize * 16].copy_from_slice(&mm);
        }
    }
    fx[24..28].copy_from_slice(&r32(m, base + cpu::MXCSR).to_le_bytes());
    fx[28..32].copy_from_slice(&0xffffu32.to_le_bytes());
    for i in 0..if x64 { 16 } else { 8u32 } {
        let b = read_bytes(m, base + xmm_home(i), 16);
        fx[160 + i as usize * 16..176 + i as usize * 16].copy_from_slice(&b);
    }
    fx
}

/// The host math functions (`env.math`), as the browser runtime provides
/// them with `Math`.
pub fn host_math(op: u32, a: f64, b: f64) -> f64 {
    match op {
        0 => a.sin(),
        1 => a.cos(),
        2 => a.tan(),
        3 => a.atan2(b),
        4 => a.log2(),
        5 => a.exp2() - 1.0,
        6 => a % b,
        7 => ieee_remainder(a, b),
        8 => a * 2f64.powi(b.trunc().clamp(-3000.0, 3000.0) as i32),
        9 => a.ln_1p() / std::f64::consts::LN_2,
        _ => f64::NAN,
    }
}

/// IEEE 754 remainder (quotient rounded to nearest, ties to even), exact.
pub fn ieee_remainder(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() || a.is_infinite() || b == 0.0 {
        return f64::NAN;
    }
    if b.is_infinite() {
        return a;
    }
    let ab = b.abs();
    // r = |a| mod 2|b| (exact), then fold into [-|b|/2, |b|/2].
    let mut r = if ab < f64::MAX / 2.0 {
        a.abs() % (2.0 * ab)
    } else {
        a.abs()
    };
    let mut odd = false;
    if r >= ab {
        r -= ab;
        odd = true;
    }
    if r > ab - r || (r == ab - r && odd) {
        r -= ab;
    }
    if a.is_sign_negative() {
        -r
    } else {
        r
    }
}
