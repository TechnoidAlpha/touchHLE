/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Selector-filtered message trace (`--trace-selectors=`).
//!
//! When at least one selector is chosen, every call of a chosen selector
//! writes one tab-separated line to `trace.log` in the user data directory.
//! Selectors come from the `--trace-selectors=` option and from the file
//! `trace_selectors.txt` in the user data directory (one per line, `#` starts
//! a comment); the two add up. With no selectors the trace is off and the only
//! cost per message is one `Option::is_some` check.
//!
//! Two kinds of call are traced:
//!
//! - Message sends through `objc_msgSend` and its variants (variant `send`,
//!   `stret` or `super2`).
//! - Calls through an IMP fetched with `methodForSelector:` or
//!   `instanceMethodForSelector:` (variant `imp`). For a traced selector those
//!   return a trampoline that writes the line and then continues in the real
//!   method (see [trace_wrap_imp]). This is how cocos2d's scheduler calls the
//!   selectors it was given, so scheduled timers show up. Methods that return
//!   a struct or a union are not wrapped (see [trace_wrap_imp]).
//!
//! Each line records the presented frame number, the milliseconds since
//! startup, the thread, whether the call came from the guest or from host
//! code, the variant, the receiver's class, the selector, the first two
//! argument words (as hex and as a float from the same bits), the first four
//! words on the stack, the link register and, last, the receiver's address
//! (the `self` column).

use super::{id, nil, Class, ClassHostObject, ObjC, IMP, SEL};
use crate::abi::GuestFunction;
use crate::cpu::Cpu;
use crate::dyld::HostFunction;
use crate::mem::{ConstPtr, Ptr};
use crate::Environment;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::Hash;
use std::io::{BufRead, BufReader, Write};

/// File in the user data directory listing selectors to trace.
pub const TRACE_SELECTORS_FILE: &str = "trace_selectors.txt";
/// File in the user data directory the trace is written to.
pub const TRACE_LOG_FILE: &str = "trace.log";

/// Which `objc_msgSend` variant a send came through. This decides where the
/// receiver, selector and arguments sit in the registers.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(super) enum SendVariant {
    /// `objc_msgSend` and friends: r0 = receiver, r1 = selector, arguments
    /// from r2.
    Normal,
    /// `objc_msgSend_stret`: r0 = struct return pointer, r1 = receiver,
    /// r2 = selector, arguments from r3.
    Stret,
    /// `objc_msgSendSuper2`: like [SendVariant::Normal] once r0 has been
    /// rewritten to the receiver; the method is looked up on the superclass.
    Super2,
}

pub struct MsgTrace {
    selectors: HashSet<String>,
    /// Whether each (uniqued) selector pointer is traced, so the string
    /// comparison happens once per selector.
    cache: HashMap<SEL, bool>,
    /// Stubs handed out by `methodForSelector:` for traced selectors, see
    /// [trace_wrap_imp].
    trampolines: TrampolineCache<(Class, SEL, u32), (SEL, GuestFunction)>,
    /// Receiver classes and selectors whose IMP was not wrapped because the
    /// method returns a struct, so that is logged once each.
    not_wrapped: HashSet<(Class, SEL)>,
    out: File,
    /// Incremented once per presented frame.
    frame: u32,
}

/// The trampolines handed out for traced selectors, keyed both ways: by what
/// they stand for (receiver class, selector, real IMP), so each is made once,
/// and by the stub's address, so the stub can find where to go. Generic over
/// the key and value so the bookkeeping can be tested without a CPU.
struct TrampolineCache<K, V> {
    by_target: HashMap<K, u32>,
    by_stub: HashMap<u32, V>,
}

impl<K: Hash + Eq, V: Copy> TrampolineCache<K, V> {
    fn new() -> Self {
        TrampolineCache {
            by_target: HashMap::new(),
            by_stub: HashMap::new(),
        }
    }

    /// The stub already made for `key`, if any.
    fn stub_for(&self, key: &K) -> Option<u32> {
        self.by_target.get(key).copied()
    }

    /// Record a new stub (its address without the Thumb bit).
    fn insert(&mut self, key: K, stub: u32, value: V) {
        let old = self.by_stub.insert(stub, value);
        assert!(old.is_none());
        self.by_target.insert(key, stub);
    }

    /// What the stub at `stub` stands for.
    fn target_of(&self, stub: u32) -> Option<V> {
        self.by_stub.get(&stub).copied()
    }
}

impl MsgTrace {
    /// Gather the selectors from the option and the file, and open the log if
    /// there are any. Returns [None] when the trace is off.
    fn open(option_selectors: &[String], bundle_id: &str) -> Option<MsgTrace> {
        let base = crate::paths::user_data_base_path();
        let mut list: Vec<String> = option_selectors.to_vec();
        if let Ok(file) = File::open(base.join(TRACE_SELECTORS_FILE)) {
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                let line = line.split('#').next().unwrap().trim();
                if !line.is_empty() {
                    list.push(line.to_string());
                }
            }
        }
        let mut selectors = HashSet::new();
        list.retain(|s| selectors.insert(s.clone()));
        if selectors.is_empty() {
            return None;
        }

        let path = base.join(TRACE_LOG_FILE);
        let mut out = match File::create(&path) {
            Ok(out) => out,
            Err(e) => {
                log!(
                    "Warning: could not create {}, message trace is off: {}",
                    path.display(),
                    e
                );
                return None;
            }
        };
        let header = format!(
            "# touchHLE {}{}{} message trace\n\
             # app: {}\n\
             # selectors: {}\n\
             # arg0/arg1 are r2/r3 (r3/stack0 for stret sends); sp0-sp3 are the words at sp\n\
             # self is the receiver's address\n\
             # no imp lines for methods that return a struct or a union\n\
             frame\tms\tthread\torigin\tvariant\tclass\tselector\targ0\targ0_f32\targ1\targ1_f32\tsp0\tsp1\tsp2\tsp3\tlr\tself\n",
            crate::branding(),
            if crate::branding().is_empty() { "" } else { " " },
            crate::VERSION,
            bundle_id,
            list.join(","),
        );
        if let Err(e) = out.write_all(header.as_bytes()) {
            log!("Warning: could not write {}: {}", path.display(), e);
            return None;
        }
        log!(
            "Message trace on, {} selector(s), writing to {}",
            list.len(),
            path.display()
        );
        Some(MsgTrace {
            selectors,
            cache: HashMap::new(),
            trampolines: TrampolineCache::new(),
            not_wrapped: HashSet::new(),
            out,
            frame: 0,
        })
    }
}

impl ObjC {
    /// Turn the message trace on if any selectors were chosen.
    pub fn start_trace(&mut self, option_selectors: &[String], bundle_id: &str) {
        self.trace = MsgTrace::open(option_selectors, bundle_id);
    }

    /// Count a presented frame for the message trace.
    pub fn trace_note_frame_presented(&mut self) {
        if let Some(trace) = self.trace.as_mut() {
            trace.frame = trace.frame.wrapping_add(1);
        }
    }
}

/// Whether `selector` is one of the traced ones. The trace must be on.
fn selector_is_traced(env: &mut Environment, selector: SEL) -> bool {
    let mem = &*env.mem;
    let Some(MsgTrace {
        selectors, cache, ..
    }) = env.objc.trace.as_mut()
    else {
        return false;
    };
    *cache.entry(selector).or_insert_with(|| {
        !selector.is_null()
            && selector
                .try_as_str(mem)
                .is_some_and(|s| selectors.contains(s))
    })
}

/// The class name as written in the trace, with `+` for a metaclass.
fn class_name_for_trace(env: &Environment, class: Class) -> String {
    let is_metaclass = env
        .objc
        .get_host_object(class)
        .and_then(|o| o.as_any().downcast_ref::<ClassHostObject>())
        .is_some_and(|c| c.is_metaclass);
    format!(
        "{}{}",
        if is_metaclass { "+" } else { "" },
        env.objc.try_get_class_name(class).unwrap_or("?")
    )
}

/// Called at the top of `objc_msgSend_inner` when the trace is on, before the
/// message type info is taken. `receiver` is never nil here.
pub(super) fn trace_send(
    env: &mut Environment,
    receiver: id,
    selector: SEL,
    super2: Option<Class>,
    variant: SendVariant,
) {
    if !selector_is_traced(env, selector) {
        return;
    }

    let origin = if env.objc.message_type_info.is_some() {
        "host"
    } else {
        "guest"
    };
    let variant_name = match variant {
        SendVariant::Normal => "send",
        SendVariant::Stret => "stret",
        SendVariant::Super2 => "super2",
    };

    // For a super call, the method is looked up on the superclass of the class
    // in the objc_super struct, so name that.
    let class = match super2 {
        Some(class) => env
            .objc
            .get_host_object(class)
            .and_then(|o| o.as_any().downcast_ref::<ClassHostObject>())
            .map_or(class, |c| c.superclass),
        None => ObjC::read_isa(receiver, &env.mem),
    };
    let class_name = class_name_for_trace(env, class);
    write_line(
        env,
        origin,
        variant_name,
        &class_name,
        selector,
        variant,
        receiver.to_bits(),
    );
}

/// Write one trace line from the current registers. The arguments are taken
/// from where `variant` puts them. `receiver` is written last, in the `self`
/// column.
fn write_line(
    env: &mut Environment,
    origin: &str,
    variant_name: &str,
    class_name: &str,
    selector: SEL,
    variant: SendVariant,
    receiver: u32,
) {
    let selector_name = selector.try_as_str(&env.mem).unwrap_or("?");

    let regs = *env.cpu.regs();
    let sp = regs[Cpu::SP];
    let mut stack = [0u32; 4];
    if sp <= u32::MAX - 15 {
        for (i, word) in stack.iter_mut().enumerate() {
            *word = env.mem.read(ConstPtr::<u32>::from_bits(sp + 4 * i as u32));
        }
    }
    let (arg0, arg1) = match variant {
        SendVariant::Stret => (regs[3], stack[0]),
        SendVariant::Normal | SendVariant::Super2 => (regs[2], regs[3]),
    };

    let elapsed_ms = env.startup_time.elapsed().as_secs_f64() * 1000.0;
    let trace = env.objc.trace.as_mut().unwrap();
    let line = format!(
        "{}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{:#010x}\t{}\t{:#010x}\t{}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}\n",
        trace.frame,
        elapsed_ms,
        env.current_thread,
        origin,
        variant_name,
        class_name,
        selector_name,
        arg0,
        fmt_f32(arg0),
        arg1,
        fmt_f32(arg1),
        stack[0],
        stack[1],
        stack[2],
        stack[3],
        regs[Cpu::LR],
        receiver,
    );
    // One unbuffered write per line, so the log is complete up to the last
    // send even if the emulator stops abruptly.
    let _ = trace.out.write_all(line.as_bytes());
}

/// The host side of every trampoline (see [trace_wrap_imp]).
const TRAMPOLINE_FUNCTION: HostFunction = &(trace_imp_trampoline as fn(&mut Environment));

/// Called from `methodForSelector:` and `instanceMethodForSelector:` with the
/// IMP they found. When the trace is on and `selector` is traced, returns a
/// trampoline in its place: a guest-callable stub that writes a trace line and
/// then continues in the real IMP. Otherwise returns `imp` unchanged.
///
/// This is how calls that never pass through `objc_msgSend` get traced.
/// cocos2d's `CCTimer` asks for the IMP of a scheduled selector once and then
/// calls it directly on every tick.
///
/// One trampoline is made per receiver class, selector and real IMP, and kept
/// for the rest of the run.
///
/// The trampoline reads the receiver from r0 and the arguments from r2 and r3.
/// A method that returns a struct may instead be called with the return
/// buffer in r0 and everything else one register later, and nothing at the
/// call says which, so such methods (and any whose type encoding is unknown)
/// are returned unwrapped and their IMP calls are not traced.
pub fn trace_wrap_imp(env: &mut Environment, class: Class, selector: SEL, imp: IMP) -> IMP {
    let IMP::Guest(real) = imp else {
        return imp;
    };
    if env.objc.trace.is_none()
        || real.addr_with_thumb_bit() == 0
        || !selector_is_traced(env, selector)
    {
        return IMP::Guest(real);
    }

    let returns_plain_value = env
        .objc
        .class_get_method_signature(class, selector)
        .is_some_and(|&types| !returns_struct(env.mem.cstr_at(types)));
    if !returns_plain_value {
        let first_time = env
            .objc
            .trace
            .as_mut()
            .unwrap()
            .not_wrapped
            .insert((class, selector));
        if first_time {
            log!(
                "Message trace: calls of [{} {}] through its IMP are not traced (it returns a struct, or its types are unknown)",
                class_name_for_trace(env, class),
                selector.try_as_str(&env.mem).unwrap_or("?")
            );
        }
        return IMP::Guest(real);
    }

    let key = (class, selector, real.addr_with_thumb_bit());
    let trampolines = &env.objc.trace.as_ref().unwrap().trampolines;
    if let Some(stub) = trampolines.stub_for(&key) {
        return IMP::Guest(GuestFunction::from_addr_with_thumb_bit(stub));
    }

    let stub = env.dyld.create_guest_function(
        &mut env.mem,
        "touchHLE_trace_imp_trampoline",
        TRAMPOLINE_FUNCTION,
    );
    let stub_addr = stub.addr_without_thumb_bit();
    env.cpu.invalidate_cache_range(stub_addr, 8);
    let trampolines = &mut env.objc.trace.as_mut().unwrap().trampolines;
    trampolines.insert(key, stub_addr, (selector, real));
    IMP::Guest(stub)
}

/// Runs when the guest calls a trampoline. Writes the trace line, then sets
/// the PC (and the Thumb state) to the real IMP, so the guest carries on
/// inside the real method with every register (r0-r3, SP, LR) as the caller
/// left it, and the method returns straight to the caller. The stub's own
/// return instruction never runs. Only methods that do not return a struct get
/// a trampoline, so the receiver is in r0 and the arguments start at r2.
fn trace_imp_trampoline(env: &mut Environment) {
    // The PC points at the instruction after the SVC, and the SVC is the
    // first instruction of the stub.
    let stub = env.cpu.regs()[Cpu::PC] - 4;
    let (selector, real) = env
        .objc
        .trace
        .as_ref()
        .and_then(|trace| trace.trampolines.target_of(stub))
        .expect("trace trampoline called with no target");

    let receiver: id = Ptr::from_bits(env.cpu.regs()[0]);
    let class_name = if receiver == nil {
        "nil".to_string()
    } else {
        class_name_for_trace(env, ObjC::read_isa(receiver, &env.mem))
    };
    write_line(
        env,
        "guest",
        "imp",
        &class_name,
        selector,
        SendVariant::Normal,
        receiver.to_bits(),
    );

    env.cpu.branch(real);
}

/// Whether an Objective-C method type encoding (return type first) returns a
/// struct or a union. Leading type qualifiers such as `r` (const) are
/// skipped.
fn returns_struct(types: &[u8]) -> bool {
    types
        .iter()
        .find(|&&c| !b"rnNoORV".contains(&c))
        .is_some_and(|&c| c == b'{' || c == b'(')
}

/// The word as a float, in plain notation for ordinary magnitudes and in
/// exponent notation otherwise (pointers read as floats are tiny or huge).
fn fmt_f32(bits: u32) -> String {
    let value = f32::from_bits(bits);
    let magnitude = value.abs();
    let is_zero = bits & 0x7fff_ffff == 0;
    if is_zero || !value.is_finite() || (1e-4..1e7).contains(&magnitude) {
        format!("{value}")
    } else {
        format!("{value:e}")
    }
}

#[cfg(test)]
mod tests {
    use super::{returns_struct, TrampolineCache};

    #[test]
    fn returns_struct_reads_the_return_type() {
        assert!(!returns_struct(b"v12@0:4@8"));
        assert!(!returns_struct(b"f8@0:4"));
        assert!(!returns_struct(b"@8@0:4"));
        assert!(returns_struct(b"{CGPoint=ff}8@0:4"));
        assert!(returns_struct(b"r{CGRect={CGPoint=ff}{CGSize=ff}}8@0:4"));
        assert!(returns_struct(b"(u=if)8@0:4"));
        assert!(!returns_struct(b"r*8@0:4"));
    }

    #[test]
    fn trampoline_cache_finds_both_ways() {
        let mut cache: TrampolineCache<(u32, u32, u32), (u32, u32)> = TrampolineCache::new();
        let key = (0x1000, 0x2000, 0x3001);
        assert_eq!(cache.stub_for(&key), None);
        cache.insert(key, 0x8000, (0x2000, 0x3001));
        assert_eq!(cache.stub_for(&key), Some(0x8000));
        assert_eq!(cache.target_of(0x8000), Some((0x2000, 0x3001)));
        assert_eq!(cache.target_of(0x8008), None);
        // The same selector and IMP on another class gets its own stub.
        let other = (0x1100, 0x2000, 0x3001);
        assert_eq!(cache.stub_for(&other), None);
        cache.insert(other, 0x8008, (0x2000, 0x3001));
        assert_eq!(cache.stub_for(&key), Some(0x8000));
        assert_eq!(cache.stub_for(&other), Some(0x8008));
    }
}
