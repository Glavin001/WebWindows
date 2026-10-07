//! Runs cases through the translator and wasmtime.

use anyhow::{anyhow, Result};
use wasmtime::{
    Caller, Config, Engine, Extern, Func, Global, GlobalType, Instance, MemoryType, Module,
    Mutability, Ref, RefType, SharedMemory, Store, Table, TableType, Val, ValType,
};
use wwt::abi::{cpu, fault, flags as fl};

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

fn mem_ptr(m: &SharedMemory) -> *mut u8 {
    m.data().as_ptr() as *mut u8
}

fn w32(m: &SharedMemory, addr: u32, v: u32) {
    unsafe { std::ptr::write_unaligned(mem_ptr(m).add(addr as usize) as *mut u32, v) }
}

fn r32(m: &SharedMemory, addr: u32) -> u32 {
    unsafe { std::ptr::read_unaligned(mem_ptr(m).add(addr as usize) as *const u32) }
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
        Ok(Executor {
            engine: Engine::new(&c)?,
        })
    }

    /// Translates and runs a batch of cases.
    pub fn run(&self, cases: &[Case], cfg: &wwt::Config) -> Result<Vec<Run>> {
        // Translate each case; unsupported ones are reported, not run.
        let mut funcs = vec![];
        let mut slot = vec![None; cases.len()];
        let mut results: Vec<Option<Run>> = (0..cases.len()).map(|_| None).collect();
        for (i, c) in cases.iter().enumerate() {
            let (f, unsupported) = wwt::translate::translate_snippet(&c.code_bytes(), INS, cfg);
            if let Some((_, why)) = unsupported.first() {
                results[i] = Some(Run::Unsupported(why.clone()));
                continue;
            }
            slot[i] = Some(funcs.len());
            funcs.push(f);
        }
        let wasm = wwt::translate::build_module(&funcs, cfg);
        let module = Module::new(&self.engine, &wasm)?;

        let memory = SharedMemory::new(&self.engine, MemoryType::shared(MEMORY_PAGES as u32, MEMORY_PAGES as u32))?;
        let mut store = Store::new(&self.engine, State::default());
        let table = Table::new(
            &mut store,
            TableType::new(RefType::FUNCREF, 1 + funcs.len() as u32, None),
            Ref::Func(None),
        )?;
        // Table slot 0: a lookup miss just reports the target address.
        let miss = {
            let mem = memory.clone();
            Func::wrap(&mut store, move |cpu_ptr: i32| -> i32 {
                r32(&mem, cpu_ptr as u32 + cpu::EIP) as i32
            })
        };
        table.set(&mut store, 0, Ref::Func(Some(miss)))?;
        let fault_fn = Func::wrap(
            &mut store,
            |mut caller: Caller<'_, State>, _cpu: i32, code: i32, eip: i32, info: i32| -> Result<()> {
                caller.data_mut().fault = Some((code as u32, eip as u32, info as u32));
                Err(anyhow!("guest fault"))
            },
        );
        let code_write = Func::wrap(&mut store, |_cpu: i32, _addr: i32| {});
        let g = |store: &mut Store<State>, v: u32| {
            Global::new(store, GlobalType::new(ValType::I32, Mutability::Const), Val::I32(v as i32))
        };
        // The lookup's first level points every page at an empty second level.
        for p in 0..(1u32 << 20) {
            w32(&memory, L1 + p * 4, ZERO_L2);
        }
        let imports: Vec<Extern> = module
            .imports()
            .map(|imp| -> Result<Extern> {
                Ok(match imp.name() {
                    "memory" => memory.clone().into(),
                    "table" => table.into(),
                    "table_base" => g(&mut store, 1)?.into(),
                    "lookup_l1" => g(&mut store, L1)?.into(),
                    "guest_limit" => g(&mut store, NATIVE_BASE - 0x10000 - 16)?.into(),
                    "code_bitmap" => g(&mut store, CODE_BITMAP)?.into(),
                    "fault" => fault_fn.into(),
                    "code_write" => code_write.into(),
                    n => return Err(anyhow!("unexpected import {n}")),
                })
            })
            .collect::<Result<_>>()?;
        Instance::new(&mut store, &module, &imports)?;

        for (i, c) in cases.iter().enumerate() {
            let Some(k) = slot[i] else { continue };
            let window = c.window();
            write_bytes(&memory, MEM_BASE, &vec![0u8; 0x1000]);
            write_bytes(&memory, MEM_BASE, &window);
            write_bytes(&memory, CPU, &[0u8; cpu::SIZE as usize]);
            for (r, v) in c.regs.iter().enumerate() {
                w32(&memory, CPU + cpu::gpr(r as u32), *v);
            }
            w32(&memory, CPU + cpu::FK, fl::EXPLICIT);
            w32(&memory, CPU + cpu::FR, c.eflags & fl::ARITH);
            w32(&memory, CPU + cpu::DF, c.eflags >> 10 & 1);
            w32(&memory, CPU + cpu::EFLAGS_SYS, 0x200);
            let fx = c.fx_bytes();
            load_fx(&memory, &fx);
            store.data_mut().fault = None;
            let func = table
                .get(&mut store, 1 + k as u64)
                .and_then(|r| r.as_func().flatten().copied())
                .ok_or_else(|| anyhow!("missing table entry"))?;
            let typed = func.typed::<i32, i32>(&store)?;
            let r = typed.call(&mut store, CPU as i32);
            let mut regs = [0u32; 8];
            for (n, reg) in regs.iter_mut().enumerate() {
                *reg = r32(&memory, CPU + cpu::gpr(n as u32));
            }
            let st = |o| r32(&memory, CPU + o);
            let arith = wwt::flags::eflags_of_state(st(cpu::FK), st(cpu::FR), st(cpu::FA), st(cpu::FB), st(cpu::FC));
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
                            fault::ACCESS_VIOLATION | fault::PRIVILEGED_INSTRUCTION => "AV",
                            fault::UNSUPPORTED => "UNSUPPORTED",
                            _ => "??",
                        }
                        .to_string(),
                    ),
                    None => (0, format!("trap: {e}")),
                },
            };
            let mem = read_bytes(&memory, MEM_BASE, MEM_SIZE);
            results[i] = Some(Run::Ok(Outcome {
                regs,
                eflags,
                eip,
                fault: fault_kind,
                mem: mem_diff(&window, &mem),
                fx: c.fx.as_ref().map(|_| hex(&save_fx(&memory))),
            }));
        }
        Ok(results.into_iter().map(|r| r.unwrap()).collect())
    }
}

/// Converts an FXSAVE image into the translator's CPU state.
fn load_fx(m: &SharedMemory, fx: &[u8]) {
    let fcw = u16::from_le_bytes([fx[0], fx[1]]);
    let fsw = u16::from_le_bytes([fx[2], fx[3]]);
    w32(m, CPU + cpu::FPU_CW, fcw as u32);
    // Status word without top; top kept separately.
    let top = (fsw >> 11) & 7;
    w32(m, CPU + cpu::FPU_SW, (fsw & !0x3800) as u32);
    w32(m, CPU + cpu::FPU_TOP, top as u32);
    // Abridged tag word: 1 bit per physical register (1 = valid).
    w32(m, CPU + cpu::FPU_TAG, fx[4] as u32);
    for i in 0..8 {
        // FXSAVE stores ST(i) relative to top; we keep physical registers.
        let st = &fx[32 + i * 16..32 + i * 16 + 10];
        let phys = (top as usize + i) & 7;
        let v = wwt::fpu::f80_to_f64(st);
        write_bytes(m, CPU + cpu::FPU_ST + phys as u32 * 8, &v.to_le_bytes());
    }
    w32(m, CPU + cpu::MXCSR, u32::from_le_bytes(fx[24..28].try_into().unwrap()));
    for i in 0..8 {
        write_bytes(m, CPU + cpu::XMM + i * 16, &fx[160 + i as usize * 16..176 + i as usize * 16]);
    }
}

/// Builds an FXSAVE image from the translator's CPU state.
fn save_fx(m: &SharedMemory) -> Vec<u8> {
    let mut fx = vec![0u8; 512];
    let top = r32(m, CPU + cpu::FPU_TOP) & 7;
    let fsw = (r32(m, CPU + cpu::FPU_SW) & !0x3800) | top << 11;
    fx[0..2].copy_from_slice(&(r32(m, CPU + cpu::FPU_CW) as u16).to_le_bytes());
    fx[2..4].copy_from_slice(&(fsw as u16).to_le_bytes());
    fx[4] = r32(m, CPU + cpu::FPU_TAG) as u8;
    for i in 0..8u32 {
        let phys = (top + i) & 7;
        let b = read_bytes(m, CPU + cpu::FPU_ST + phys * 8, 8);
        let v = f64::from_le_bytes(b.try_into().unwrap());
        fx[32 + i as usize * 16..42 + i as usize * 16].copy_from_slice(&wwt::fpu::f64_to_f80(v));
    }
    fx[24..28].copy_from_slice(&r32(m, CPU + cpu::MXCSR).to_le_bytes());
    fx[28..32].copy_from_slice(&0xffffu32.to_le_bytes());
    for i in 0..8u32 {
        let b = read_bytes(m, CPU + cpu::XMM + i * 16, 16);
        fx[160 + i as usize * 16..176 + i as usize * 16].copy_from_slice(&b);
    }
    fx
}
