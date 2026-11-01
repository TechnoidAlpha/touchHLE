/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <optional>
#include <unordered_map>
#include <vector>

#include "dynarmic/frontend/A32/a32_ir_emitter.h"
#include "dynarmic/frontend/A32/a32_location_descriptor.h"
#include "dynarmic/interface/A32/a32.h"
#include "dynarmic/interface/A32/config.h"
#include "dynarmic/interface/A32/coprocessor.h"
#include "dynarmic/interface/exclusive_monitor.h"
#include "dynarmic/ir/terminal.h"

namespace touchHLE::cpu {

using VAddr = std::uint32_t;

// Types and functions defined in Rust
extern "C" {
struct touchHLE_Mem;
std::uint8_t touchHLE_cpu_read_u8(touchHLE_Mem *mem, VAddr addr, bool *error);
std::uint16_t touchHLE_cpu_read_u16(touchHLE_Mem *mem, VAddr addr, bool *error);
std::uint32_t touchHLE_cpu_read_u32(touchHLE_Mem *mem, VAddr addr, bool *error);
std::uint64_t touchHLE_cpu_read_u64(touchHLE_Mem *mem, VAddr addr, bool *error);
bool touchHLE_cpu_write_u8(touchHLE_Mem *mem, VAddr addr, std::uint8_t value);
bool touchHLE_cpu_write_u16(touchHLE_Mem *mem, VAddr addr, std::uint16_t value);
bool touchHLE_cpu_write_u32(touchHLE_Mem *mem, VAddr addr, std::uint32_t value);
bool touchHLE_cpu_write_u64(touchHLE_Mem *mem, VAddr addr, std::uint64_t value);
// A write to a watched page (see src/cpu.rs). Returns 0 if done, 1 on a
// memory error, 2 if done and execution should halt so the write can be
// logged with its PC.
std::int32_t touchHLE_cpu_watch_write(void *watches, touchHLE_Mem *mem,
                                      VAddr addr, std::uint32_t size,
                                      std::uint64_t value, bool may_halt);
struct touchHLE_DynarmicContext {
  std::array<std::uint32_t, 16> regs;
  std::array<std::uint32_t, 64> extregs;
  std::uint32_t cpsr;
  std::uint32_t fpscr;
};
}

const auto HaltReasonSvc = Dynarmic::HaltReason::UserDefined1;
const auto HaltReasonUndefinedInstruction = Dynarmic::HaltReason::UserDefined2;
const auto HaltReasonBreakpoint = Dynarmic::HaltReason::UserDefined3;
// The instruments (see src/cpu.rs) halt with one of these together with
// MemoryAbort: dynarmic checks that bit after each memory access that goes
// through a callback, and then stores the exact PC of the instruction and
// returns, so the state is the one just before (a hook) or in the middle
// of (a watched write) that instruction.
const auto HaltReasonHook = Dynarmic::HaltReason::UserDefined4;
const auto HaltReasonWatch = Dynarmic::HaltReason::UserDefined5;

// A hooked address starts a block whose first IR operation writes the
// address to this one, which is in the null page, so the write goes through
// MemoryWrite32 (the null page has no page table entry) and is followed by
// the MemoryAbort check.
const VAddr HookProbeAddr = 0x00000ff0;
// Which instruction sets a hook is for.
const std::uint8_t HookArm = 1;
const std::uint8_t HookThumb = 2;

class Environment final : public Dynarmic::A32::UserCallbacks {
public:
  Dynarmic::A32::Jit *cpu = nullptr;
  touchHLE_Mem *mem = nullptr;
  std::uint64_t ticks_remaining;
  uint32_t halting_svc;

  // Address hooks: the hooked addresses, each with HookArm and/or
  // HookThumb. Empty unless the Rust side adds some.
  std::unordered_map<VAddr, std::uint8_t> hooks;
  // The hook to let through once (after it was logged), and whether that
  // happened.
  std::optional<VAddr> hook_skip;
  bool hook_skip_used = false;
  // The hook that halted execution last.
  VAddr hook_hit = 0;

  // Coverage: when on, each instruction address the JIT translates, with
  // bit 0 set for Thumb, until the Rust side drains them.
  bool coverage_on = false;
  std::vector<std::uint32_t> coverage;

  // Watchpoints: how many watches each page has (empty if none was ever
  // set), and the Rust state the watched writes go to.
  std::vector<std::uint16_t> watch_counts;
  void *watches = nullptr;

  void WriteWord32(VAddr vaddr, std::uint32_t value, bool may_halt) {
    if (IsWatched(vaddr, 4)) {
      WatchedWrite(vaddr, 4, value, may_halt);
      return;
    }
    if (touchHLE_cpu_write_u32(mem, vaddr, value)) {
      cpu->HaltExecution(Dynarmic::HaltReason::MemoryAbort);
    }
  }

private:
  bool IsWatched(VAddr vaddr, std::uint32_t size) const {
    if (watch_counts.empty()) {
      return false;
    }
    const VAddr last = vaddr + (size - 1);
    const auto page_bits = Dynarmic::A32::UserConfig::PAGE_BITS;
    return watch_counts[vaddr >> page_bits] != 0 ||
           watch_counts[last >> page_bits] != 0;
  }
  void WatchedWrite(VAddr vaddr, std::uint32_t size, std::uint64_t value,
                    bool may_halt) {
    switch (
        touchHLE_cpu_watch_write(watches, mem, vaddr, size, value, may_halt)) {
    case 0:
      break;
    case 1:
      cpu->HaltExecution(Dynarmic::HaltReason::MemoryAbort);
      break;
    default:
      cpu->HaltExecution(Dynarmic::HaltReason::MemoryAbort | HaltReasonWatch);
      break;
    }
  }

  void HookProbe(VAddr pc) {
    if (hook_skip && *hook_skip == pc) {
      hook_skip.reset();
      hook_skip_used = true;
      return;
    }
    hook_hit = pc;
    cpu->HaltExecution(Dynarmic::HaltReason::MemoryAbort | HaltReasonHook);
  }

  bool PreCodeReadHook(bool is_thumb, VAddr pc,
                       Dynarmic::A32::IREmitter &ir) override {
    if (hooks.empty()) {
      return true;
    }
    const auto hook = hooks.find(pc);
    if (hook == hooks.end() ||
        (hook->second & (is_thumb ? HookThumb : HookArm)) == 0) {
      return true;
    }
    // A hooked address always starts a block: the block it would have been
    // in ends before it and links to it.
    if (Dynarmic::A32::LocationDescriptor{ir.block.Location()}.PC() != pc) {
      ir.SetTerm(Dynarmic::IR::Term::LinkBlock{ir.current_location});
      return false;
    }
    ir.WriteMemory32(ir.Imm32(HookProbeAddr), ir.Imm32(pc),
                     Dynarmic::IR::AccType::NORMAL);
    return true;
  }

  void PreCodeTranslationHook(bool is_thumb, VAddr pc,
                              Dynarmic::A32::IREmitter &) override {
    if (coverage_on) {
      coverage.push_back(pc | (is_thumb ? 1 : 0));
    }
  }

  std::uint8_t MemoryRead8(VAddr vaddr) override {
    bool error;
    auto value = touchHLE_cpu_read_u8(mem, vaddr, &error);
    if (error) {
      cpu->HaltExecution(Dynarmic::HaltReason::MemoryAbort);
    }
    return value;
  }
  std::uint16_t MemoryRead16(VAddr vaddr) override {
    bool error;
    auto value = touchHLE_cpu_read_u16(mem, vaddr, &error);
    if (error) {
      cpu->HaltExecution(Dynarmic::HaltReason::MemoryAbort);
    }
    return value;
  }
  std::uint32_t MemoryRead32(VAddr vaddr) override {
    bool error;
    auto value = touchHLE_cpu_read_u32(mem, vaddr, &error);
    if (error) {
      cpu->HaltExecution(Dynarmic::HaltReason::MemoryAbort);
    }
    return value;
  }
  std::uint64_t MemoryRead64(VAddr vaddr) override {
    bool error;
    auto value = touchHLE_cpu_read_u64(mem, vaddr, &error);
    if (error) {
      cpu->HaltExecution(Dynarmic::HaltReason::MemoryAbort);
    }
    return value;
  }

  std::optional<std::uint32_t> MemoryReadCode(VAddr vaddr) override {
    bool error;
    auto value = touchHLE_cpu_read_u32(mem, vaddr, &error);
    if (error) {
      return std::nullopt;
    } else {
      return value;
    }
  }

  void MemoryWrite8(VAddr vaddr, std::uint8_t value) override {
    if (IsWatched(vaddr, 1)) {
      WatchedWrite(vaddr, 1, value, true);
      return;
    }
    if (touchHLE_cpu_write_u8(mem, vaddr, value)) {
      cpu->HaltExecution(Dynarmic::HaltReason::MemoryAbort);
    }
  }
  void MemoryWrite16(VAddr vaddr, std::uint16_t value) override {
    if (IsWatched(vaddr, 2)) {
      WatchedWrite(vaddr, 2, value, true);
      return;
    }
    if (touchHLE_cpu_write_u16(mem, vaddr, value)) {
      cpu->HaltExecution(Dynarmic::HaltReason::MemoryAbort);
    }
  }
  void MemoryWrite32(VAddr vaddr, std::uint32_t value) override {
    if (vaddr == HookProbeAddr && hooks.count(value) != 0) {
      HookProbe(value);
      return;
    }
    WriteWord32(vaddr, value, true);
  }
  void MemoryWrite64(VAddr vaddr, std::uint64_t value) override {
    if (IsWatched(vaddr, 8)) {
      WatchedWrite(vaddr, 8, value, true);
      return;
    }
    if (touchHLE_cpu_write_u64(mem, vaddr, value)) {
      cpu->HaltExecution(Dynarmic::HaltReason::MemoryAbort);
    }
  }

  bool MemoryWriteExclusive8(VAddr, std::uint8_t, std::uint8_t) override {
    std::fprintf(stderr, "MemoryWriteExclusive8: TODO");
    abort();
  }
  bool MemoryWriteExclusive16(VAddr, std::uint16_t, std::uint16_t) override {
    std::fprintf(stderr, "MemoryWriteExclusive16: TODO");
    abort();
  }
  bool MemoryWriteExclusive32(VAddr addr, std::uint32_t value,
                              std::uint32_t expected) override {
    // As long as we stay single threaded on the host side,
    // this implementation is OK
    // TODO: revisit once (if) we switch to a host multi-threading
    if (MemoryRead32(addr) != expected) {
      // TODO: implement CAS mechanism or similar
      // (be aware that implementation may need to be platform specific!)
      std::fprintf(stderr, "MemoryWriteExclusive32: expected %u, got %u\n",
                   expected, MemoryRead32(addr));
      abort();
    }
    // A watched write here must not halt: running the strex again would
    // fail (the monitor is clear) and the guest would retry on the new
    // value.
    WriteWord32(addr, value, /* may_halt: */ false);
    return true;
  }
  bool MemoryWriteExclusive64(VAddr, std::uint64_t, std::uint64_t) override {
    std::fprintf(stderr, "MemoryWriteExclusive64: TODO");
    abort();
  }

  void InterpreterFallback(std::uint32_t, size_t) override {
    abort(); // TODO
  }
  void CallSVC(std::uint32_t svc) override {
    halting_svc = svc;
    cpu->HaltExecution(HaltReasonSvc);
  }
  void ExceptionRaised(VAddr pc, Dynarmic::A32::Exception exception) override {
    // MemoryReadCode returned nullopt
    if (exception == Dynarmic::A32::Exception::NoExecuteFault) {
      cpu->HaltExecution(Dynarmic::HaltReason::MemoryAbort);
    } else if (exception == Dynarmic::A32::Exception::UndefinedInstruction) {
      cpu->HaltExecution(HaltReasonUndefinedInstruction);
    } else if (exception == Dynarmic::A32::Exception::Breakpoint) {
      cpu->HaltExecution(HaltReasonBreakpoint);
    } else {
      std::fprintf(stderr, "ExceptionRaised: unexpected exception %u at %x\n",
                   unsigned(exception), pc);
      abort();
    }
  }
  void AddTicks(std::uint64_t ticks) override {
    if (ticks > ticks_remaining) {
      ticks_remaining = 0;
      return;
    }
    ticks_remaining -= ticks;
  }
  std::uint64_t GetTicksRemaining() override { return ticks_remaining; }
};

class ArmDynarmicCP15 : public Dynarmic::A32::Coprocessor {
  std::uint32_t addr = 0;

public:
  using CoprocReg = Dynarmic::A32::CoprocReg;

  CallbackOrAccessOneWord CompileSendOneWord(bool two, unsigned opc1,
                                             CoprocReg CRn, CoprocReg CRm,
                                             unsigned opc2) override {
    // Corresponds to `coproc_moveto_Data_Memory_Barrier(0)` according
    // to the Ghidra or `mcr p15,0x0,lr,cr7,cr10,0x5` in the assembly
    if (!two && CRn == CoprocReg::C7 && opc1 == 0 && CRm == CoprocReg::C10 &&
        opc2 == 5) {
      // just return the address to be used as storage
      return &addr;
    }
    return CallbackOrAccessOneWord{};
  }

  // TODO
  std::optional<Callback> CompileInternalOperation(bool, unsigned, CoprocReg,
                                                   CoprocReg, CoprocReg,
                                                   unsigned) override {
    return std::nullopt;
  }
  CallbackOrAccessTwoWords CompileSendTwoWords(bool, unsigned,
                                               CoprocReg) override {
    return CallbackOrAccessTwoWords{};
  }
  CallbackOrAccessOneWord CompileGetOneWord(bool, unsigned, CoprocReg,
                                            CoprocReg, unsigned) override {
    return CallbackOrAccessOneWord{};
  }
  CallbackOrAccessTwoWords CompileGetTwoWords(bool, unsigned,
                                              CoprocReg) override {
    return CallbackOrAccessTwoWords{};
  }
  std::optional<Callback>
  CompileLoadWords(bool, bool, CoprocReg,
                   std::optional<std::uint8_t>) override {
    return std::nullopt;
  }
  std::optional<Callback>
  CompileStoreWords(bool, bool, CoprocReg,
                    std::optional<std::uint8_t>) override {
    return std::nullopt;
  }
};

class DynarmicWrapper {
  Environment env;
  std::unique_ptr<Dynarmic::A32::Jit> cpu;
  std::unique_ptr<Dynarmic::ExclusiveMonitor> mon;
  std::array<std::uint8_t *, Dynarmic::A32::UserConfig::NUM_PAGE_TABLE_ENTRIES>
      page_table;
  std::uint8_t *direct_memory_access_base = nullptr;
  size_t null_pages = 0;

public:
  DynarmicWrapper(void *direct_memory_access_ptr, size_t null_page_count,
                  bool precise_halts) {
    Dynarmic::A32::UserConfig user_config;
    user_config.callbacks = &env;
    user_config.coprocessors[15] = std::make_shared<ArmDynarmicCP15>();
    mon = std::make_unique<Dynarmic::ExclusiveMonitor>(1);
    user_config.global_monitor = mon.get();
    // TODO: only do this in debug builds? it's probably expensive
    user_config.check_halt_on_memory_access = true;
    if (precise_halts) {
      // A watched write halts in the middle of a block, and execution
      // resumes from the state dynarmic stored. On x64 that state is
      // exact because dynarmic skips this pass when it checks for halts on
      // memory accesses; the arm64 back end runs it anyway, so it is
      // turned off here.
      user_config.optimizations &=
          ~Dynarmic::OptimizationFlag::GetSetElimination;
    }
    direct_memory_access_base = (std::uint8_t *)direct_memory_access_ptr;
    null_pages = null_page_count;
    if (direct_memory_access_ptr) {
      // Allow fast accesses to all pages other than the null page, which will
      // fall back to a memory callback, which will then abort execution.
      // TODO: Eventually we should use dynarmic's true fastmem mode, but that
      // requires using mmap/mprotect/etc on the host OS so we can still catch
      // null pointer accesses.
      page_table.fill((std::uint8_t *)direct_memory_access_ptr);
      // Note that the null page size is also defined in src/mem.rs.
      static_assert(1 << Dynarmic::A32::UserConfig::PAGE_BITS == 0x1000);

      if (null_page_count > page_table.size()) {
        printf("Too many null pages, %zu requested but maximum is %zu.",
               null_page_count, page_table.size());
        abort();
      }
      for (size_t i = 0; i < null_page_count; i++) {
        page_table[i] = nullptr;
      }
      user_config.page_table = &page_table;
      user_config.absolute_offset_page_table = true;
    }
    cpu = std::make_unique<Dynarmic::A32::Jit>(user_config);
    env.cpu = cpu.get();
  }

  const std::uint32_t *regs() const { return &cpu->Regs().front(); }
  std::uint32_t *regs() { return &cpu->Regs().front(); }

  std::uint32_t cpsr() const { return cpu->Cpsr(); }
  void set_cpsr(std::uint32_t cpsr) { cpu->SetCpsr(cpsr); }

  void invalidate_cache_range(VAddr start, std::uint32_t size) {
    cpu->InvalidateCacheRange(start, size);
  }

  // Hooks need the null page to have no page table entry (see
  // HookProbeAddr).
  bool hooks_usable() const {
    return !direct_memory_access_base || null_pages > 0;
  }
  void set_hook(VAddr pc, std::uint8_t modes) {
    if (modes == 0) {
      env.hooks.erase(pc);
    } else {
      env.hooks[pc] = modes;
    }
    // The block holding the address is translated again, so it ends
    // before the address (or no longer does).
    cpu->InvalidateCacheRange(pc, 4);
  }
  VAddr hook_hit() const { return env.hook_hit; }
  void begin_hook_skip(VAddr pc) {
    env.hook_skip = pc;
    env.hook_skip_used = false;
  }
  bool end_hook_skip() {
    env.hook_skip.reset();
    return env.hook_skip_used;
  }

  void set_coverage(bool on) { env.coverage_on = on; }
  size_t coverage_drain(std::uint32_t *out, size_t capacity) {
    const size_t count = std::min(capacity, env.coverage.size());
    const size_t start = env.coverage.size() - count;
    std::copy(env.coverage.begin() + start, env.coverage.end(), out);
    env.coverage.resize(start);
    return count;
  }

  void set_watches(void *watches) { env.watches = watches; }
  void watch_page(std::uint32_t page, bool watch) {
    if (page >= page_table.size()) {
      return;
    }
    if (env.watch_counts.empty()) {
      env.watch_counts.resize(page_table.size());
    }
    auto &count = env.watch_counts[page];
    if (watch) {
      count++;
    } else if (count > 0) {
      count--;
    }
    // Taking the page out of the table sends every access to it through
    // the callbacks, where a write is checked; the emitted code reads the
    // table at run time, so no code needs translating again. The null
    // pages never have an entry.
    if (direct_memory_access_base && page >= null_pages) {
      page_table[page] = count > 0 ? nullptr : direct_memory_access_base;
    }
  }

  void swap_context(touchHLE_DynarmicContext *context) {
    touchHLE_DynarmicContext tmp = {cpu->Regs(), cpu->ExtRegs(), cpu->Cpsr(),
                                    cpu->Fpscr()};
    cpu->Regs() = context->regs;
    cpu->ExtRegs() = context->extregs;
    cpu->SetCpsr(context->cpsr);
    cpu->SetFpscr(context->fpscr);
    *context = tmp;
  }

  std::int32_t run_or_step(touchHLE_Mem *mem, std::uint64_t *ticks) {
    env.mem = mem;
    Dynarmic::HaltReason hr;
    if (ticks) {
      env.ticks_remaining = *ticks;
      hr = cpu->Run();
    } else {
      hr = cpu->Step();
    }
    std::int32_t res;
    if ((!hr && ticks) || (hr == Dynarmic::HaltReason::Step && !ticks)) {
      res = -1;
    } else if (Dynarmic::Has(hr, HaltReasonHook)) {
      res = -5;
    } else if (Dynarmic::Has(hr, HaltReasonWatch)) {
      res = -6;
    } else if (Dynarmic::Has(hr, Dynarmic::HaltReason::MemoryAbort)) {
      res = -2;
    } else if (Dynarmic::Has(hr, HaltReasonUndefinedInstruction)) {
      res = -3;
    } else if (Dynarmic::Has(hr, HaltReasonBreakpoint)) {
      res = -4;
    } else if (Dynarmic::Has(hr, HaltReasonSvc)) {
      res = std::int32_t(env.halting_svc);
    } else {
      printf("unhandled halt reason %u\n", unsigned(hr));
      abort();
    }
    env.mem = nullptr;
    if (ticks) {
      *ticks = env.ticks_remaining;
    }
    return res;
  }
};

extern "C" {

DynarmicWrapper *touchHLE_DynarmicWrapper_new(void *direct_memory_access_ptr,
                                              size_t null_page_count,
                                              bool precise_halts) {
  return new DynarmicWrapper(direct_memory_access_ptr, null_page_count,
                             precise_halts);
}
void touchHLE_DynarmicWrapper_delete(DynarmicWrapper *cpu) { delete cpu; }

const std::uint32_t *
touchHLE_DynarmicWrapper_regs_const(const DynarmicWrapper *cpu) {
  return cpu->regs();
}
std::uint32_t *touchHLE_DynarmicWrapper_regs_mut(DynarmicWrapper *cpu) {
  return cpu->regs();
}

std::uint32_t touchHLE_DynarmicWrapper_cpsr(const DynarmicWrapper *cpu) {
  return cpu->cpsr();
}
void touchHLE_DynarmicWrapper_set_cpsr(DynarmicWrapper *cpu,
                                       std::uint32_t cpsr) {
  cpu->set_cpsr(cpsr);
}

void touchHLE_DynarmicWrapper_swap_context(DynarmicWrapper *cpu,
                                           touchHLE_DynarmicContext *context) {
  cpu->swap_context(context);
}

void touchHLE_DynarmicWrapper_invalidate_cache_range(DynarmicWrapper *cpu,
                                                     VAddr start,
                                                     std::uint32_t size) {
  cpu->invalidate_cache_range(start, size);
}

std::int32_t touchHLE_DynarmicWrapper_run_or_step(DynarmicWrapper *cpu,
                                                  touchHLE_Mem *mem,
                                                  std::uint64_t *ticks) {
  return cpu->run_or_step(mem, ticks);
}

bool touchHLE_DynarmicWrapper_hooks_usable(const DynarmicWrapper *cpu) {
  return cpu->hooks_usable();
}
void touchHLE_DynarmicWrapper_set_hook(DynarmicWrapper *cpu, VAddr pc,
                                       std::uint8_t modes) {
  cpu->set_hook(pc, modes);
}
VAddr touchHLE_DynarmicWrapper_hook_hit(const DynarmicWrapper *cpu) {
  return cpu->hook_hit();
}
void touchHLE_DynarmicWrapper_begin_hook_skip(DynarmicWrapper *cpu, VAddr pc) {
  cpu->begin_hook_skip(pc);
}
bool touchHLE_DynarmicWrapper_end_hook_skip(DynarmicWrapper *cpu) {
  return cpu->end_hook_skip();
}

void touchHLE_DynarmicWrapper_set_coverage(DynarmicWrapper *cpu, bool on) {
  cpu->set_coverage(on);
}
size_t touchHLE_DynarmicWrapper_coverage_drain(DynarmicWrapper *cpu,
                                               std::uint32_t *out,
                                               size_t capacity) {
  return cpu->coverage_drain(out, capacity);
}

void touchHLE_DynarmicWrapper_set_watches(DynarmicWrapper *cpu, void *watches) {
  cpu->set_watches(watches);
}
void touchHLE_DynarmicWrapper_watch_page(DynarmicWrapper *cpu,
                                         std::uint32_t page, bool watch) {
  cpu->watch_page(page, watch);
}
}

} // namespace touchHLE::cpu
