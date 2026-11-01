/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! CPU emulation.
//!
//! Implemented using the C++ library dynarmic, which is a dynamic recompiler.
//!
//! iPhone OS apps used either ARMv6 or ARMv7-A, which are both 32-bit ISAs.
//! For the moment, only ARMv6 has been tested.
//!
//! The message trace's instruments reach into the CPU here (see
//! src/objc/trace.rs for what they write):
//!
//! - Address hooks ([Cpu::set_hook]): a hooked address always starts a
//!   block of JIT code, whose first IR operation is a write to a probe
//!   address in the null page. That write goes through the C++ memory
//!   callback, which halts the CPU, and dynarmic's check for a halt after a
//!   callback stores the exact PC; so the CPU stops before the hooked
//!   instruction with every register as it was ([CpuState::Hook]). After
//!   the row is written, [Cpu::step_past_hook] runs that one instruction
//!   with the probe let through once, and execution carries on.
//! - Coverage ([Cpu::set_coverage]): each instruction address the JIT
//!   translates.
//! - Watchpoints ([Cpu::watch]): the page holding a watched range is taken
//!   out of dynarmic's page table, so every access to it goes through the
//!   memory callbacks. A write that touches the range is made, recorded
//!   with the range's old and new bytes, and halts the CPU the same way,
//!   with the PC of the writing instruction ([CpuState::Watch]); then
//!   [Cpu::step_past_watch] runs the rest of that instruction.

mod watch;

pub use watch::WatchHit;

use crate::abi::GuestFunction;
use crate::mem::{ConstPtr, ConstVoidPtr, GuestUSize, Mem, MutPtr, Ptr, SafeRead, SafeWrite};
use std::collections::BTreeSet;
use watch::Watches;

// Import functions from C++
use touchHLE_dynarmic_wrapper::*;

type VAddr = u32;
pub type CpuContext = touchHLE_DynarmicContext;

fn touchHLE_cpu_read_impl<T: SafeRead + Default>(
    mem: *mut touchHLE_Mem,
    addr: VAddr,
    error: *mut bool,
) -> T {
    // If a panic occurs (probably due to a null-pointer access), we can't let
    // it keep unwinding as it will hit non-Rust stack frames (dynarmic).
    // Instead we catch the unwind and then tell the C++ code a problem occurred
    // so it can immediately halt CPU execution and then panic itself, now
    // with only Rust stack frames to worry about and with CPU state information
    // available that's useful for debugging.
    //
    // TODO: Disable this in debug mode? This relies on dynarmic's
    // check_halt_on_memory_access option which surely has a significant
    // performance impact.
    //
    // I'm not sure if this actually is unwind-safe, but considering
    // the emulator will crash anyway, maybe this is okay.
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mem = unsafe { &mut *mem.cast::<Mem>() };
        let ptr: ConstPtr<T> = Ptr::from_bits(addr);
        mem.read(ptr)
    }));
    unsafe {
        error.write(res.is_err());
    }
    res.unwrap_or_default()
}

fn touchHLE_cpu_write_impl<T: SafeWrite>(mem: *mut touchHLE_Mem, addr: VAddr, value: T) -> bool {
    // See comments above about catch_unwind
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mem = unsafe { &mut *mem.cast::<Mem>() };
        let ptr: MutPtr<T> = Ptr::from_bits(addr);
        mem.write(ptr, value)
    }));
    res.is_err()
}

// Export functions for use by C++
#[no_mangle]
extern "C" fn touchHLE_cpu_read_u8(mem: *mut touchHLE_Mem, addr: VAddr, error: *mut bool) -> u8 {
    touchHLE_cpu_read_impl(mem, addr, error)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_read_u16(mem: *mut touchHLE_Mem, addr: VAddr, error: *mut bool) -> u16 {
    touchHLE_cpu_read_impl(mem, addr, error)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_read_u32(mem: *mut touchHLE_Mem, addr: VAddr, error: *mut bool) -> u32 {
    touchHLE_cpu_read_impl(mem, addr, error)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_read_u64(mem: *mut touchHLE_Mem, addr: VAddr, error: *mut bool) -> u64 {
    touchHLE_cpu_read_impl(mem, addr, error)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_write_u8(mem: *mut touchHLE_Mem, addr: VAddr, value: u8) -> bool {
    touchHLE_cpu_write_impl(mem, addr, value)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_write_u16(mem: *mut touchHLE_Mem, addr: VAddr, value: u16) -> bool {
    touchHLE_cpu_write_impl(mem, addr, value)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_write_u32(mem: *mut touchHLE_Mem, addr: VAddr, value: u32) -> bool {
    touchHLE_cpu_write_impl(mem, addr, value)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_write_u64(mem: *mut touchHLE_Mem, addr: VAddr, value: u64) -> bool {
    touchHLE_cpu_write_impl(mem, addr, value)
}

/// The `len` (at most 8) bytes at `addr` as a little-endian number, or 0 if
/// they cannot be read.
pub(crate) fn read_le(mem: &Mem, addr: VAddr, len: u32) -> u64 {
    mem.get_bytes_fallible(ConstVoidPtr::from_bits(addr), len)
        .map_or(0, |bytes| {
            bytes
                .iter()
                .rev()
                .fold(0u64, |value, &byte| (value << 8) | u64::from(byte))
        })
}

/// A write of `size` bytes (1, 2, 4 or 8) of `value` at `addr` that touches
/// a watched page (see the module doc). Called by the C++ memory callbacks
/// instead of the plain write. Makes the write, records it if it touched a
/// watched range, and returns 0 if done, 1 on a memory error (like the
/// plain write's error flag), 2 if the CPU should halt so the writing
/// instruction can be found. `may_halt` is false for a store-exclusive.
#[no_mangle]
extern "C" fn touchHLE_cpu_watch_write(
    watches: *mut std::ffi::c_void,
    mem: *mut touchHLE_Mem,
    addr: VAddr,
    size: u32,
    value: u64,
    may_halt: bool,
) -> i32 {
    // See touchHLE_cpu_read_impl about catch_unwind.
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mem = unsafe { &mut *mem.cast::<Mem>() };
        let watches = unsafe { &mut *watches.cast::<Watches>() };
        let touched = watches.touched(addr, size);
        let old: Vec<u64> = touched
            .iter()
            .map(|&(_, start, len)| read_le(mem, start, len))
            .collect();
        match size {
            1 => mem.write(MutPtr::<u8>::from_bits(addr), value as u8),
            2 => mem.write(MutPtr::<u16>::from_bits(addr), value as u16),
            4 => mem.write(MutPtr::<u32>::from_bits(addr), value as u32),
            _ => mem.write(MutPtr::<u64>::from_bits(addr), value),
        }
        let touched: Vec<(usize, u64, u64)> = touched
            .iter()
            .zip(old)
            .map(|(&(watch, start, len), old)| (watch, old, read_le(mem, start, len)))
            .collect();
        watches.record(addr, size, &touched, may_halt)
    }));
    match res {
        Err(_) => 1,
        Ok(false) => 0,
        Ok(true) => 2,
    }
}

pub struct Cpu {
    dynarmic_wrapper: *mut touchHLE_DynarmicWrapper,
    /// Copy of the direct memory access pointer used to check it has not
    /// changed. If this is null, direct memory access is not in use.
    direct_memory_access_ptr: *const std::ffi::c_void,
    /// The watchpoints (see [Cpu::watch]), made at the first one. The C++
    /// side holds this pointer and hands it to [touchHLE_cpu_watch_write]
    /// while the CPU runs; it is freed when the CPU is.
    watches: *mut Watches,
    /// Every instruction address the JIT has translated since coverage was
    /// turned on, with bit 0 set for Thumb (see [Cpu::coverage]). [None]
    /// while it is off.
    coverage: Option<BTreeSet<u32>>,
}

impl Drop for Cpu {
    fn drop(&mut self) {
        unsafe { touchHLE_DynarmicWrapper_delete(self.dynarmic_wrapper) }
        if !self.watches.is_null() {
            drop(unsafe { Box::from_raw(self.watches) });
        }
    }
}

/// Why CPU execution ended.
#[derive(Debug)]
pub enum CpuState {
    /// Execution halted due to using up all remaining ticks (normal execution)
    /// or after the single instruction was executed (step execution).
    Normal,
    /// SVC instruction encountered.
    Svc(u32),
    /// An error was encountered.
    Error(CpuError),
    /// Execution reached a hooked address (see [Cpu::set_hook]), which is
    /// given. The PC is that address and the hooked instruction has not run.
    Hook(VAddr),
    /// An instruction wrote to a watched range (see [Cpu::watch]). The PC
    /// is that instruction's, and the write is done, but the rest of the
    /// instruction (another word of an `stm`, the base register's update)
    /// may not be: see [Cpu::step_past_watch].
    Watch,
}

/// A reason that can cause CPU execution to be interrupted.
#[derive(Debug, Clone, PartialEq)]
pub enum CpuError {
    /// Memory error during execution (probably a null page access).
    MemoryError,
    /// Undefined instruction (perhaps from a GDB software breakpoint).
    UndefinedInstruction,
    /// Breakpoint (`bkpt` instruction).
    Breakpoint,
}

impl Cpu {
    /// The register number of the stack pointer.
    pub const SP: usize = 13;
    /// The register number of the link register.
    #[allow(unused)]
    pub const LR: usize = 14;
    /// The register number of the program counter.
    pub const PC: usize = 15;

    /// When this bit is set in CPSR, the CPU is in Thumb mode.
    pub const CPSR_THUMB: u32 = 0x00000020;

    /// When this bit is set in CPSR, the CPU is in user mode.
    pub const CPSR_USER_MODE: u32 = 0x00000010;

    /// Construct a new CPU instance. If a mutable reference to a [Mem] instance
    /// is provided, direct memory access is enabled, and the CPU instance
    /// becomes bound to that [Mem] instance (subsequent calls must use the same
    /// one).
    ///
    /// `precise_halts` must be set if watchpoints will be used (see the
    /// module doc): execution then resumes from the middle of a block of JIT
    /// code, which needs every register written back as it changes.
    pub fn new(direct_memory_access: Option<&mut Mem>, precise_halts: bool) -> Cpu {
        // Null page count is in pages rather than bytes. Mem ensures it is
        // page aligned.
        let null_page_count: usize = direct_memory_access
            .as_ref()
            .map_or(0, |mem| mem.null_segment_size() / 0x1000)
            .try_into()
            .unwrap();
        // Safety: the direct memory access pointer will be retained directly by
        // the dynarmic wrapper and indirectly by cached JIT code, so we must
        // ensure we only execute the CPU while holding a &mut on the Mem object
        // to which that pointer belongs.
        let direct_memory_access_ptr = direct_memory_access
            .map_or(std::ptr::null_mut(), |mem| unsafe {
                mem.direct_memory_access_ptr()
            });
        let dynarmic_wrapper = unsafe {
            touchHLE_DynarmicWrapper_new(direct_memory_access_ptr, null_page_count, precise_halts)
        };
        Cpu {
            dynarmic_wrapper,
            direct_memory_access_ptr,
            watches: std::ptr::null_mut(),
            coverage: None,
        }
    }

    pub fn regs(&self) -> &[u32; 16] {
        unsafe {
            let ptr = touchHLE_DynarmicWrapper_regs_const(self.dynarmic_wrapper);
            &*(ptr as *const [u32; 16])
        }
    }
    pub fn regs_mut(&mut self) -> &mut [u32; 16] {
        unsafe {
            let ptr = touchHLE_DynarmicWrapper_regs_mut(self.dynarmic_wrapper);
            &mut *(ptr as *mut [u32; 16])
        }
    }

    /// Dump the registers of the current cpu to the log output.
    /// Silently ignores panics.
    pub fn dump_regs(&self) {
        let regs = self.regs();
        Self::echo_regs(regs);
    }

    pub fn echo_regs(regs: &[u32; 16]) {
        // Silently ignore panics so it's safe to use in contexts where we
        // can't panic.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for row in 0..4 {
                use std::fmt::Write;
                let mut line = String::new();
                for col in 0..4 {
                    let reg_idx = row * 4 + col;
                    match reg_idx {
                        Self::SP => write!(&mut line, "\t SP: "),
                        Self::LR => write!(&mut line, "\t LR: "),
                        Self::PC => write!(&mut line, "\t PC: "),
                        _ if reg_idx <= 9 => write!(&mut line, "\t R{reg_idx}: "),
                        _ => write!(&mut line, "\tR{reg_idx}: "),
                    }
                    .unwrap();
                    write!(&mut line, "{:#010x}", regs[reg_idx]).unwrap();
                }
                echo!("{}", line);
            }
        }));
    }

    pub fn cpsr(&self) -> u32 {
        unsafe { touchHLE_DynarmicWrapper_cpsr(self.dynarmic_wrapper) }
    }
    pub fn set_cpsr(&mut self, cpsr: u32) {
        unsafe { touchHLE_DynarmicWrapper_set_cpsr(self.dynarmic_wrapper, cpsr) }
    }

    /// Swap the current state of the CPU (registers etc) with the state stored
    /// in the context object.
    pub fn swap_context(&mut self, context: &mut CpuContext) {
        unsafe { touchHLE_DynarmicWrapper_swap_context(self.dynarmic_wrapper, context) }
    }

    /// Get PC with the Thumb bit appropriately set.
    pub fn pc_with_thumb_bit(&self) -> GuestFunction {
        let pc = self.regs()[Self::PC];
        let thumb = (self.cpsr() & Self::CPSR_THUMB) == Self::CPSR_THUMB;
        GuestFunction::from_addr_and_thumb_flag(pc, thumb)
    }

    /// Set PC and the Thumb flag for executing a guest function. Note that this
    /// does not touch LR.
    pub fn branch(&mut self, new_pc: GuestFunction) {
        self.regs_mut()[Self::PC] = new_pc.addr_without_thumb_bit();
        let cpsr_without_thumb = self.cpsr() & (!Self::CPSR_THUMB);
        self.set_cpsr(cpsr_without_thumb | ((new_pc.is_thumb() as u32) * Self::CPSR_THUMB))
    }

    /// Set the PC and Thumb flag (like [Self::branch]), but also set the LR,
    /// and return the original PC and LR.
    pub fn branch_with_link(
        &mut self,
        new_pc: GuestFunction,
        new_lr: GuestFunction,
    ) -> (GuestFunction, GuestFunction) {
        let old_pc = self.pc_with_thumb_bit();
        let old_lr = GuestFunction::from_addr_with_thumb_bit(self.regs()[Self::LR]);
        self.branch(new_pc);
        self.regs_mut()[Self::LR] = new_lr.addr_with_thumb_bit();
        (old_pc, old_lr)
    }

    /// Clear dynarmic's instruction cache for some range of addresses.
    /// This is of interest to the dynamic linker, which will sometimes rewrite
    /// code.
    pub fn invalidate_cache_range(&mut self, base: VAddr, size: GuestUSize) {
        unsafe {
            touchHLE_DynarmicWrapper_invalidate_cache_range(self.dynarmic_wrapper, base, size)
        }
    }

    /// Hook the instruction at `addr` (without the Thumb bit) when it runs as
    /// Arm code (`arm`) and/or as Thumb code (`thumb`): execution stops there
    /// with [CpuState::Hook] (see the module doc). Neither removes the hook.
    /// Returns false, and does nothing, if hooks cannot work: with direct
    /// memory access and no null page, the probe would be an ordinary write.
    pub fn set_hook(&mut self, addr: VAddr, arm: bool, thumb: bool) -> bool {
        if !unsafe { touchHLE_DynarmicWrapper_hooks_usable(self.dynarmic_wrapper) } {
            return false;
        }
        let modes = u8::from(arm) | (u8::from(thumb) << 1);
        unsafe { touchHLE_DynarmicWrapper_set_hook(self.dynarmic_wrapper, addr, modes) };
        true
    }

    /// After [CpuState::Hook] for `addr`, run the hooked instruction (a
    /// single step) without stopping at the hook again, and return what the
    /// step returned. The step is what lets the hook through, so no other
    /// thread can take that pass (a thread switch can only happen between
    /// runs).
    #[must_use]
    pub fn step_past_hook(&mut self, mem: &mut Mem, addr: VAddr) -> CpuState {
        unsafe { touchHLE_DynarmicWrapper_begin_hook_skip(self.dynarmic_wrapper, addr) };
        let state = self.run_or_step(mem, None);
        let used = unsafe { touchHLE_DynarmicWrapper_end_hook_skip(self.dynarmic_wrapper) };
        if !used {
            log_dbg!(
                "The hook at {:#x} was not passed by the step after it ({:?})",
                addr,
                state
            );
        }
        state
    }

    /// Turn coverage (every instruction address the JIT translates) on or
    /// off. Turning it off forgets what was gathered.
    pub fn set_coverage(&mut self, on: bool) {
        unsafe { touchHLE_DynarmicWrapper_set_coverage(self.dynarmic_wrapper, on) };
        self.coverage = on.then(BTreeSet::new);
    }

    /// The instruction addresses translated since coverage was turned on,
    /// each once, with bit 0 set for Thumb; [None] if it is off. Code that is
    /// translated again (after its cache was cleared) is counted once.
    pub fn coverage(&mut self) -> Option<&BTreeSet<u32>> {
        let set = self.coverage.as_mut()?;
        let mut buffer = vec![0u32; 4096];
        loop {
            let count = unsafe {
                touchHLE_DynarmicWrapper_coverage_drain(
                    self.dynarmic_wrapper,
                    buffer.as_mut_ptr(),
                    buffer.len(),
                )
            };
            if count == 0 {
                break;
            }
            set.extend(&buffer[..count]);
        }
        Some(set)
    }

    /// Watch writes to the `len` bytes (at most 8) at `start`. Returns the
    /// watch's number, for [Cpu::unwatch] and [WatchHit::watch]. Only the
    /// pages the range is on slow down. The CPU must have been made with
    /// `precise_halts` (see [Cpu::new]).
    pub fn watch(&mut self, start: VAddr, len: GuestUSize) -> usize {
        if self.watches.is_null() {
            self.watches = Box::into_raw(Box::default());
            unsafe {
                touchHLE_DynarmicWrapper_set_watches(self.dynarmic_wrapper, self.watches.cast())
            };
        }
        let watch = unsafe { &mut *self.watches }.add(start, len);
        for page in watch::pages_of(start, len) {
            unsafe { touchHLE_DynarmicWrapper_watch_page(self.dynarmic_wrapper, page, true) };
        }
        watch
    }

    /// Stop watching `watch`. Its pages go back to fast access once no
    /// other watch is on them.
    pub fn unwatch(&mut self, watch: usize) {
        if self.watches.is_null() {
            return;
        }
        if let Some((start, len)) = unsafe { &mut *self.watches }.remove(watch) {
            for page in watch::pages_of(start, len) {
                unsafe { touchHLE_DynarmicWrapper_watch_page(self.dynarmic_wrapper, page, false) };
            }
        }
    }

    /// The writes to watched ranges recorded since the last call.
    pub fn take_watch_hits(&mut self) -> Vec<WatchHit> {
        if self.watches.is_null() {
            return Vec::new();
        }
        unsafe { &mut *self.watches }.take_hits()
    }

    /// After [CpuState::Watch], run the rest of the writing instruction (a
    /// single step of the whole instruction, which makes its writes again)
    /// and return what the step returned. `logged` are the writes (address
    /// and size) already taken with [Cpu::take_watch_hits]: each is skipped
    /// once, and any other watched write the instruction makes is recorded
    /// with its address, without halting.
    #[must_use]
    pub fn step_past_watch(&mut self, mem: &mut Mem, logged: Vec<(VAddr, u32)>) -> CpuState {
        let pc = self.regs()[Self::PC];
        if !self.watches.is_null() {
            unsafe { &mut *self.watches }.begin_step(pc, logged);
        }
        let state = self.run_or_step(mem, None);
        if !self.watches.is_null() {
            unsafe { &mut *self.watches }.end_step();
        }
        state
    }

    /// Start CPU execution.
    ///
    /// If `ticks` is [Some], it is used as an abstract time limit. The value
    /// will be reduced proportionately with the amount of ticks expended.
    ///
    /// If `ticks` is [None], the CPU executes only a single instruction. This
    /// is also known as "stepping".
    ///
    /// This will return either because the CPU ran out of time, or because
    /// something else happened which requires attention from the host.
    #[must_use]
    pub fn run_or_step(&mut self, mem: &mut Mem, ticks: Option<&mut u64>) -> CpuState {
        // See ::new() for why this is done.
        if !self.direct_memory_access_ptr.is_null() {
            assert!(self.direct_memory_access_ptr == unsafe { mem.direct_memory_access_ptr() });
        }

        let res = unsafe {
            touchHLE_DynarmicWrapper_run_or_step(
                self.dynarmic_wrapper,
                mem as *mut Mem as *mut touchHLE_Mem,
                ticks,
            )
        };
        match res {
            -1 => CpuState::Normal,
            -2 => CpuState::Error(CpuError::MemoryError),
            -3 => CpuState::Error(CpuError::UndefinedInstruction),
            -4 => CpuState::Error(CpuError::Breakpoint),
            -5 => {
                CpuState::Hook(unsafe { touchHLE_DynarmicWrapper_hook_hit(self.dynarmic_wrapper) })
            }
            -6 => CpuState::Watch,
            _ if res < -6 => panic!("Unexpected CPU execution result"),
            svc => CpuState::Svc(svc as u32),
        }
    }
}
