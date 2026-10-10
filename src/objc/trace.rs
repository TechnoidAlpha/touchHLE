/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Selector-filtered message trace (`--trace-selectors=`).
//!
//! When at least one selector is chosen, every message send of a chosen
//! selector writes one tab-separated line to `trace.log` in the user data
//! directory. Selectors come from the `--trace-selectors=` option and from the
//! file `trace_selectors.txt` in the user data directory (one per line, `#`
//! starts a comment); the two add up. With no selectors the trace is off and
//! the only cost per message is one `Option::is_some` check.
//!
//! Each line records the presented frame number, the milliseconds since
//! startup, the thread, whether the send came from the guest or from host code,
//! the receiver's class, the selector, the first two argument words (as hex and
//! as a float from the same bits), the first four words on the stack and the
//! link register.

use super::{id, Class, ClassHostObject, ObjC, SEL};
use crate::cpu::Cpu;
use crate::mem::ConstPtr;
use crate::Environment;
use std::collections::{HashMap, HashSet};
use std::fs::File;
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
    out: File,
    /// Incremented once per presented frame.
    frame: u32,
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
             frame\tms\tthread\torigin\tvariant\tclass\tselector\targ0\targ0_f32\targ1\targ1_f32\tsp0\tsp1\tsp2\tsp3\tlr\n",
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

/// Called at the top of `objc_msgSend_inner` when the trace is on, before the
/// message type info is taken. `receiver` is never nil here.
pub(super) fn trace_send(
    env: &mut Environment,
    receiver: id,
    selector: SEL,
    super2: Option<Class>,
    variant: SendVariant,
) {
    let traced = {
        let mem = &*env.mem;
        let Some(MsgTrace {
            selectors, cache, ..
        }) = env.objc.trace.as_mut()
        else {
            return;
        };
        *cache.entry(selector).or_insert_with(|| {
            !selector.is_null()
                && selector
                    .try_as_str(mem)
                    .is_some_and(|s| selectors.contains(s))
        })
    };
    if !traced {
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
    let is_metaclass = env
        .objc
        .get_host_object(class)
        .and_then(|o| o.as_any().downcast_ref::<ClassHostObject>())
        .is_some_and(|c| c.is_metaclass);
    let class_name = format!(
        "{}{}",
        if is_metaclass { "+" } else { "" },
        env.objc.try_get_class_name(class).unwrap_or("?")
    );
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
        "{}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{:#010x}\t{}\t{:#010x}\t{}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}\n",
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
    );
    // One unbuffered write per line, so the log is complete up to the last
    // send even if the emulator stops abruptly.
    let _ = trace.out.write_all(line.as_bytes());
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
