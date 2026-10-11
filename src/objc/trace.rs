/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Selector-filtered message trace (`--trace-selectors=`), with frame
//! capture (`--capture-frames=`) and a touch log.
//!
//! When at least one selector is chosen, every call of a chosen selector
//! writes one tab-separated line to `trace.log` in the user data directory.
//! Selectors come from the `--trace-selectors=` option and from the file
//! `trace_selectors.txt` in the user data directory (one per line, `#` starts
//! a comment); the two add up. The trace is on when there is at least one
//! selector or `--capture-frames=` is set. When it is off, the only cost per
//! message is one `Option::is_some` check.
//!
//! A line of `trace_selectors.txt` may add marker words after the selector,
//! separated by spaces (see [parse_selector_line]). The only one is `png`:
//! `powerFire png` saves the first frame presented after each call of
//! `powerFire`, the frame that shows its result. `--capture-frames=N` saves
//! every Nth frame as well. Frames are read back on the CAEAGLLayer fast
//! path only (see [trace_present_frame]) and written by a worker thread as
//! `frames/f{frame:06}_{ms}.png` (see [crate::frame_capture]), where
//! `frame` is the frame column of the trace lines whose drawing the picture
//! shows and `ms` the whole milliseconds since startup when it was
//! presented.
//!
//! A line may also carry peeks, in any order with the markers (see [peek]
//! for the grammar): fields of the receiver or of a pointer argument, read
//! from guest memory when the call is logged and written after the `self`
//! column, one column per peek in the order listed. With
//! `dmg: self.hp:f32 self.expectedHp:f32`, every `dmg:` row has two more
//! columns. Rows of other selectors keep the usual columns, so the column
//! count differs per selector; the header lists each selector's peeks on a
//! line `# peeks <selector> <spec> <spec>...`. Peeks are read at call
//! entry, before the method runs: the state after a call is in the peeks of
//! the next traced call. A read never panics and never calls the guest (see
//! [peek_columns]); one that fails writes `-` and is logged once per
//! selector and peek. An item of `--trace-selectors=` takes the same words
//! as a line, with `;` between them (`dmg:;self.hp:f32`).
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
//! words on the stack, the link register, the receiver's address (the
//! `self` column) and then the selector's peeks, if it has any.
//!
//! While the trace is on, every touch event the app receives is also written
//! to `input.log` in the user data directory (see [trace_touches]), on the
//! same clock and with the same frame numbers as `trace.log`, and every line
//! the app prints with `NSLog` becomes a `trace.log` row with origin `nslog`
//! (see [trace_app_log]).

mod peek;

use super::{id, nil, Class, ClassHostObject, ObjC, IMP, SEL};
use crate::abi::GuestFunction;
use crate::cpu::Cpu;
use crate::dyld::HostFunction;
use crate::frame_capture::{FrameCapture, FRAMES_DIR};
use crate::mem::{ConstPtr, ConstVoidPtr, GuestUSize, Mem, Ptr};
use crate::window::{Coords, FingerId};
use crate::Environment;
use peek::{PeekBase, PeekSpec, PeekStep, PeekType};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::Hash;
use std::io::{BufRead, BufReader, Write};

/// File in the user data directory listing selectors to trace.
pub const TRACE_SELECTORS_FILE: &str = "trace_selectors.txt";
/// File in the user data directory the trace is written to.
pub const TRACE_LOG_FILE: &str = "trace.log";
/// File in the user data directory the touch log is written to, see
/// [trace_touches].
pub const INPUT_LOG_FILE: &str = "input.log";

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

/// What a line of `trace_selectors.txt` asks for besides tracing the
/// selector, see [parse_selector_line].
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
struct Markers {
    /// `png`: save the first frame presented after each call.
    png: bool,
    /// The selector's peeks: an index into [MsgTrace::peek_sets], or [None]
    /// if it has none.
    peeks: Option<usize>,
}

/// The peeks of one traced selector, in the order they were listed.
struct PeekSet {
    selector: String,
    peeks: Vec<Peek>,
}

/// One peek (see [peek]) and what it has learnt while running.
struct Peek {
    spec: PeekSpec,
    /// The offset of the ivar named at step `n` of the path, per class of
    /// the object it was looked up on: [None] if that class chain has no
    /// such ivar. Each (step, class) is looked up once.
    ivar_offsets: HashMap<(usize, Class), Option<GuestUSize>>,
    /// Whether a failed read has been logged (once per selector and peek).
    failure_logged: bool,
}

pub struct MsgTrace {
    /// The traced selectors and their markers.
    selectors: HashMap<String, Markers>,
    /// The peeks of each selector that has any, see [Markers::peeks].
    peek_sets: Vec<PeekSet>,
    /// The markers of each (uniqued) selector pointer, or [None] if it is not
    /// traced, so the string comparison happens once per selector.
    cache: HashMap<SEL, Option<Markers>>,
    /// Stubs handed out by `methodForSelector:` for traced selectors, see
    /// [trace_wrap_imp].
    trampolines: TrampolineCache<(Class, SEL, u32), (SEL, GuestFunction)>,
    /// Receiver classes and selectors whose IMP was not wrapped because the
    /// method returns a struct, so that is logged once each.
    not_wrapped: HashSet<(Class, SEL)>,
    out: File,
    /// Incremented once per presented frame.
    frame: u32,
    /// The touch log, see [trace_touches]. [None] if it could not be
    /// created.
    input: Option<File>,
    /// The frame capture worker, if frames are to be saved and it started.
    capture: Option<FrameCapture>,
    /// `--capture-frames=N`: save every Nth frame. 0 is off.
    capture_every: u32,
    /// A selector with the `png` marker was called since the last frame.
    capture_pending: bool,
    /// Whether the "frames are not captured on this path" message has been
    /// logged.
    uncaptured_path_logged: bool,
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

/// What one line of `trace_selectors.txt` (or one item of
/// `--trace-selectors=`) asks for, see [parse_selector_line].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SelectorLine {
    selector: String,
    /// The `png` marker.
    png: bool,
    /// The peek specs, in the order given.
    peeks: Vec<PeekSpec>,
    /// Words that are neither a marker nor a peek: ignored, logged once.
    unknown: Vec<String>,
    /// Peeks that did not parse, with the reason: ignored, logged.
    bad_peeks: Vec<(String, String)>,
}

/// One line of `trace_selectors.txt`: the selector, then optional marker
/// words and peek specs in any order, separated by spaces or tabs; `#`
/// starts a comment. Returns [None] for a blank or comment-only line. The
/// only marker is `png`; a word starting with `self`, `arg0` or `arg1` is a
/// peek (see [peek]); any other word is ignored, and logged once by the
/// caller.
fn parse_selector_line(line: &str) -> Option<SelectorLine> {
    let line = line.split('#').next().unwrap();
    let mut words = line.split_whitespace();
    let mut parsed = SelectorLine {
        selector: words.next()?.to_string(),
        ..SelectorLine::default()
    };
    for word in words {
        if word == "png" {
            parsed.png = true;
        } else if peek::looks_like_peek(word) {
            match peek::parse_peek(word) {
                Ok(spec) => parsed.peeks.push(spec),
                Err(e) => parsed.bad_peeks.push((word.to_string(), e)),
            }
        } else {
            parsed.unknown.push(word.to_string());
        }
    }
    Some(parsed)
}

/// `--capture-frames=` in words, for the trace header and the log.
fn periodic_capture(capture_every: u32) -> String {
    match capture_every {
        0 => "no periodic capture".to_string(),
        1 => "every frame".to_string(),
        n => format!("every {n}th frame"),
    }
}

impl MsgTrace {
    /// Gather the selectors from the option and the file, and open the log if
    /// there are any or `capture_every` is not 0. Returns [None] when the
    /// trace is off.
    fn open(option_selectors: &[String], capture_every: u32, bundle_id: &str) -> Option<MsgTrace> {
        let base = crate::paths::user_data_base_path();
        // An item of the option is a line of the file with ';' between its
        // words (an options file line cannot hold spaces in one option).
        let mut lines: Vec<(&str, SelectorLine)> = option_selectors
            .iter()
            .filter_map(|s| parse_selector_line(&s.replace(';', " ")))
            .map(|line| ("--trace-selectors=", line))
            .collect();
        if let Ok(file) = File::open(base.join(TRACE_SELECTORS_FILE)) {
            lines.extend(
                BufReader::new(file)
                    .lines()
                    .map_while(Result::ok)
                    .filter_map(|line| parse_selector_line(&line))
                    .map(|line| (TRACE_SELECTORS_FILE, line)),
            );
        }
        // The same selector may be listed twice; its markers and peeks add
        // up.
        let mut unknown_logged: HashSet<String> = HashSet::new();
        let mut list: Vec<String> = Vec::new();
        let mut selectors: HashMap<String, Markers> = HashMap::new();
        let mut peek_sets: Vec<PeekSet> = Vec::new();
        for (source, line) in lines {
            for word in &line.unknown {
                if unknown_logged.insert(word.clone()) {
                    log!(
                        "Warning: {} has an unknown marker {:?} (after {}), ignored; the only marker is png, and a peek starts with self, arg0 or arg1",
                        source,
                        word,
                        line.selector
                    );
                }
            }
            for (word, reason) in &line.bad_peeks {
                log!(
                    "Warning: {} has a peek {:?} (after {}) that is not understood, ignored: {}",
                    source,
                    word,
                    line.selector,
                    reason
                );
            }
            let entry = selectors.entry(line.selector.clone()).or_insert_with(|| {
                list.push(line.selector.clone());
                Markers::default()
            });
            entry.png |= line.png;
            if line.peeks.is_empty() {
                continue;
            }
            let index = *entry.peeks.get_or_insert_with(|| {
                peek_sets.push(PeekSet {
                    selector: line.selector.clone(),
                    peeks: Vec::new(),
                });
                peek_sets.len() - 1
            });
            let set = &mut peek_sets[index];
            for spec in line.peeks {
                if set.peeks.iter().all(|p| p.spec != spec) {
                    set.peeks.push(Peek {
                        spec,
                        ivar_offsets: HashMap::new(),
                        failure_logged: false,
                    });
                }
            }
        }
        if selectors.is_empty() && capture_every == 0 {
            return None;
        }
        let png_after: Vec<&str> = list
            .iter()
            .filter(|s| selectors[*s].png)
            .map(String::as_str)
            .collect();

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
        let peek_lines: String = peek_sets
            .iter()
            .map(|set| {
                let specs: Vec<&str> = set.peeks.iter().map(|p| p.spec.text.as_str()).collect();
                format!("# peeks {} {}\n", set.selector, specs.join(" "))
            })
            .collect();
        let header = format!(
            "# touchHLE {}{}{} message trace\n\
             # app: {}\n\
             # selectors: {}\n\
             # arg0/arg1 are r2/r3 (r3/stack0 for stret sends); sp0-sp3 are the words at sp\n\
             # self is the receiver's address\n\
             # no imp lines for methods that return a struct or a union\n\
             # frame capture (frames/f<frame>_<ms>.png): {}; png after: {}\n\
             # origin nslog: a line the app printed with NSLog, in the selector column\n\
             # peeks: a row of a selector named in a '# peeks' line has one more column\n\
             # per peek after self, in that line's order; other rows end at self, so\n\
             # the column count differs per selector. Peeks are read when the call\n\
             # starts, before the method runs (the state after it is in the next\n\
             # traced row's peeks); '-' is a peek that could not be read\n\
             {}\
             frame\tms\tthread\torigin\tvariant\tclass\tselector\targ0\targ0_f32\targ1\targ1_f32\tsp0\tsp1\tsp2\tsp3\tlr\tself\n",
            crate::branding(),
            if crate::branding().is_empty() { "" } else { " " },
            crate::VERSION,
            bundle_id,
            list.join(","),
            periodic_capture(capture_every),
            if png_after.is_empty() {
                "none".to_string()
            } else {
                png_after.join(",")
            },
            peek_lines,
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
        let input = open_input_log(&base.join(INPUT_LOG_FILE), bundle_id);
        let capture = if capture_every > 0 || !png_after.is_empty() {
            let dir = base.join(FRAMES_DIR);
            match FrameCapture::start(dir.clone()) {
                Ok(capture) => {
                    log!(
                        "Frame capture on ({}, after {} png selector(s)), writing to {}",
                        periodic_capture(capture_every),
                        png_after.len(),
                        dir.display()
                    );
                    Some(capture)
                }
                Err(e) => {
                    log!("Warning: frame capture is off: {}", e);
                    None
                }
            }
        } else {
            None
        };
        Some(MsgTrace {
            selectors,
            peek_sets,
            cache: HashMap::new(),
            trampolines: TrampolineCache::new(),
            not_wrapped: HashSet::new(),
            out,
            frame: 0,
            input,
            capture,
            capture_every,
            capture_pending: false,
            uncaptured_path_logged: false,
        })
    }
}

/// Create the touch log and write its header. Returns [None] (and logs why)
/// if it cannot be written; the rest of the trace carries on without it.
fn open_input_log(path: &std::path::Path, bundle_id: &str) -> Option<File> {
    let header = format!(
        "# touchHLE {}{}{} touch log\n\
         # app: {}\n\
         # one row per finger per touch event the app receives, tab-separated\n\
         # frame and ms: the same clock and frame numbers as trace.log\n\
         # (ms since touchHLE started; frame = frames presented so far)\n\
         # phase: down, move or up, as the app receives it; finger: touchHLE's finger id\n\
         # x, y: guest points, unrotated, as the app's UITouch has them\n\
         # frame\tms\tphase\tfinger\tx\ty\n",
        crate::branding(),
        if crate::branding().is_empty() {
            ""
        } else {
            " "
        },
        crate::VERSION,
        bundle_id,
    );
    let result = File::create(path).and_then(|mut file| {
        file.write_all(header.as_bytes())?;
        Ok(file)
    });
    match result {
        Ok(file) => Some(file),
        Err(e) => {
            log!(
                "Warning: could not write {}, no touch log: {}",
                path.display(),
                e
            );
            None
        }
    }
}

/// One row of the touch log, see [trace_touches].
fn touch_row(frame: u32, ms: f64, phase: &str, finger: FingerId, (x, y): Coords) -> String {
    format!("{frame}\t{ms:.3}\t{phase}\t{finger:?}\t{x}\t{y}\n")
}

/// Called by UIKit for each touch event it hands to the app. When the trace
/// is on, writes one row per finger to `input.log`: the frame, the
/// milliseconds since startup, `phase` (`down`, `move` or `up`, the phase
/// the app receives the finger in, see `ui_touch::handle_event`), the finger
/// (`Mouse`, `Touch(n)`, `VirtualCursor`, `ButtonToTouch(button)`,
/// `StickToTouch` or `DpadToTouch`) and the position in guest points. Each
/// event is one unbuffered write, so a crash keeps every row before it.
pub fn trace_touches(env: &mut Environment, phase: &str, touches: &HashMap<FingerId, Coords>) {
    let elapsed_ms = env.startup_time.elapsed().as_secs_f64() * 1000.0;
    let Some(trace) = env.objc.trace.as_mut() else {
        return;
    };
    let frame = trace.frame;
    let Some(input) = trace.input.as_mut() else {
        return;
    };
    let mut fingers: Vec<_> = touches.iter().collect();
    fingers.sort_by_key(|(finger, _)| format!("{finger:?}"));
    let rows: String = fingers
        .into_iter()
        .map(|(&finger, &coords)| touch_row(frame, elapsed_ms, phase, finger, coords))
        .collect();
    let _ = input.write_all(rows.as_bytes());
}

impl ObjC {
    /// Turn the message trace on if any selectors were chosen or
    /// `capture_every` (`--capture-frames=`) is not 0.
    pub fn start_trace(
        &mut self,
        option_selectors: &[String],
        capture_every: u32,
        bundle_id: &str,
    ) {
        self.trace = MsgTrace::open(option_selectors, capture_every, bundle_id);
    }
}

/// A frame [trace_present_frame] chose to save: the trace frame number and
/// the whole milliseconds since startup that name the file.
#[derive(Copy, Clone, Debug)]
pub struct FrameCaptureTicket {
    frame: u32,
    ms: u64,
}

/// Whether the frame numbered `frame` is saved: every `every`th frame
/// (counting from frame 0; `every` = 0 is off), and the first frame after a
/// call of a selector with the `png` marker (`pending`).
fn capture_due(frame: u32, every: u32, pending: bool) -> bool {
    pending || (every != 0 && frame.is_multiple_of(every))
}

/// Called by `present_renderbuffer` (the CAEAGLLayer fast path) for each
/// frame it presents. Counts the frame for the trace's frame column and says
/// whether to read it back and save it.
///
/// The frame is numbered with the frame column of the trace lines written
/// while it was drawn, i.e. the count before this one: the first frame
/// presented is frame 0. So `f000042_*.png` shows the drawing done during
/// the lines with frame 42, and the frame saved for a `png` selector has the
/// frame number of the line that called it.
pub fn trace_present_frame(env: &mut Environment) -> Option<FrameCaptureTicket> {
    let ms = u64::try_from(env.startup_time.elapsed().as_millis()).unwrap_or(u64::MAX);
    let trace = env.objc.trace.as_mut()?;
    let frame = trace.frame;
    trace.frame = trace.frame.wrapping_add(1);
    let due = capture_due(frame, trace.capture_every, trace.capture_pending);
    if trace.capture.is_none() || !due {
        return None;
    }
    trace.capture_pending = false;
    Some(FrameCaptureTicket { frame, ms })
}

/// Hand a frame read back for `ticket` to the frame capture worker. `rgba`
/// is `width` by `height` RGBA8 pixels, bottom row first.
pub fn trace_capture_frame(
    env: &mut Environment,
    ticket: FrameCaptureTicket,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) {
    if let Some(capture) = env.objc.trace.as_mut().and_then(|t| t.capture.as_mut()) {
        capture.send(ticket.frame, ticket.ms, width, height, rgba);
    }
}

/// Called where a frame is presented without going through
/// `present_renderbuffer` (`path` names the way). Frames are not captured
/// there, and the trace's frame number does not advance, so when frame
/// capture is on this is logged once.
pub fn trace_note_uncaptured_present(env: &mut Environment, path: &str) {
    let Some(trace) = env.objc.trace.as_mut() else {
        return;
    };
    if trace.capture.is_none() || trace.uncaptured_path_logged {
        return;
    }
    trace.uncaptured_path_logged = true;
    log!(
        "Frame capture: a frame was presented through {}, where frames are not captured and the trace's frame number does not advance (only the CAEAGLLayer fast path is captured)",
        path
    );
}

/// The markers of `selector` if it is one of the traced ones. The trace must
/// be on.
fn selector_markers(env: &mut Environment, selector: SEL) -> Option<Markers> {
    let mem = &*env.mem;
    let MsgTrace {
        selectors, cache, ..
    } = env.objc.trace.as_mut()?;
    *cache.entry(selector).or_insert_with(|| {
        if selector.is_null() {
            return None;
        }
        selector
            .try_as_str(mem)
            .and_then(|s| selectors.get(s).copied())
    })
}

/// Whether `selector` is one of the traced ones. The trace must be on.
fn selector_is_traced(env: &mut Environment, selector: SEL) -> bool {
    selector_markers(env, selector).is_some()
}

/// Act on a traced call's markers once its line is written.
fn note_markers(env: &mut Environment, markers: Markers) {
    if let Some(trace) = env.objc.trace.as_mut() {
        trace.capture_pending |= markers.png;
    }
}

/// The class name as written in the trace, with `+` for a metaclass.
fn class_name_for_trace(objc: &ObjC, class: Class) -> String {
    let is_metaclass = objc
        .get_host_object(class)
        .and_then(|o| o.as_any().downcast_ref::<ClassHostObject>())
        .is_some_and(|c| c.is_metaclass);
    format!(
        "{}{}",
        if is_metaclass { "+" } else { "" },
        objc.try_get_class_name(class).unwrap_or("?")
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
    let Some(markers) = selector_markers(env, selector) else {
        return;
    };

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
    let class_name = class_name_for_trace(&env.objc, class);
    write_line(
        env,
        origin,
        variant_name,
        &class_name,
        selector,
        variant,
        receiver.to_bits(),
        markers.peeks,
    );
    note_markers(env, markers);
}

/// Write one trace line from the current registers. The arguments are taken
/// from where `variant` puts them. `receiver` is written in the `self`
/// column, and after it the columns of the selector's peeks (`peeks` is an
/// index into [MsgTrace::peek_sets]), if it has any.
#[allow(clippy::too_many_arguments)]
fn write_line(
    env: &mut Environment,
    origin: &str,
    variant_name: &str,
    class_name: &str,
    selector: SEL,
    variant: SendVariant,
    receiver: u32,
    peeks: Option<usize>,
) {
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
    let peeked = match peeks {
        Some(set) => peek_columns(env, set, [receiver, arg0, arg1]),
        None => String::new(),
    };

    let selector_name = selector.try_as_str(&env.mem).unwrap_or("?");
    let elapsed_ms = env.startup_time.elapsed().as_secs_f64() * 1000.0;
    let trace = env.objc.trace.as_mut().unwrap();
    let line = format!(
        "{}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{:#010x}\t{}\t{:#010x}\t{}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}{}\n",
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
        peeked,
    );
    // One unbuffered write per line, so the log is complete up to the last
    // send even if the emulator stops abruptly.
    let _ = trace.out.write_all(line.as_bytes());
}

/// The peek columns of a row: a tab and a value per peek of
/// `peek_sets[set]`, or a tab and `-` for one that cannot be read (logged
/// once per selector and peek). `words` is self, arg0 and arg1. Only reads
/// guest memory, through the non-panicking [Mem::get_bytes_fallible], and
/// the class table: nothing here calls the guest.
fn peek_columns(env: &mut Environment, set: usize, words: [u32; 3]) -> String {
    // Taken out while it is used, so the class table can be read alongside.
    let Some(mut trace) = env.objc.trace.take() else {
        return String::new();
    };
    let PeekSet { selector, peeks } = &mut trace.peek_sets[set];
    let mut columns = String::new();
    for peek in peeks.iter_mut() {
        columns.push('\t');
        match read_peek(&env.objc, &env.mem, peek, words) {
            Ok(value) => columns.push_str(&value),
            Err(why) => {
                columns.push('-');
                if !peek.failure_logged {
                    peek.failure_logged = true;
                    log!(
                        "Message trace: peek {} of {} could not be read ({}), its column shows - (logged once)",
                        peek.spec.text,
                        selector,
                        why
                    );
                }
            }
        }
    }
    env.objc.trace = Some(trace);
    columns
}

/// The `count` bytes at `addr`, or why not (the null page, or past the end
/// of guest memory). Never panics.
fn read_guest_bytes(mem: &Mem, addr: u32, count: GuestUSize) -> Result<&[u8], String> {
    mem.get_bytes_fallible(ConstVoidPtr::from_bits(addr), count)
        .ok_or_else(|| format!("{count} bytes at {addr:#x} are not readable"))
}

/// The little-endian word at `addr`, which need not be aligned.
fn read_guest_word(mem: &Mem, addr: u32) -> Result<u32, String> {
    let bytes = read_guest_bytes(mem, addr, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// The offset of the ivar `name` on the class chain starting at `class`:
/// [None] if `class` is not a class touchHLE knows (a bad pointer), otherwise
/// [Some] with the offset, or with [None] if no class on the chain has that
/// ivar. Like [ObjC::object_lookup_ivar], but it cannot panic.
fn ivar_offset(objc: &ObjC, mem: &Mem, class: Class, name: &str) -> Option<Option<GuestUSize>> {
    let mut current = class;
    // A bound, in case a broken chain loops.
    for depth in 0..64 {
        let Some(host) = objc
            .get_host_object(current)
            .and_then(|o| o.as_any().downcast_ref::<ClassHostObject>())
        else {
            return if depth == 0 { None } else { Some(None) };
        };
        if let Some(&(offset_ptr, _)) = host.ivars.get(name) {
            return Some(read_guest_word(mem, offset_ptr.to_bits()).ok());
        }
        if host.superclass == nil {
            break;
        }
        current = host.superclass;
    }
    Some(None)
}

/// Follow a peek's path from `words` (self, arg0, arg1) and read its value,
/// written as its column shows it, or say why it cannot be read.
fn read_peek(objc: &ObjC, mem: &Mem, peek: &mut Peek, words: [u32; 3]) -> Result<String, String> {
    let Peek {
        spec, ivar_offsets, ..
    } = peek;
    let base = words[match spec.base {
        PeekBase::Receiver => 0,
        PeekBase::Arg0 => 1,
        PeekBase::Arg1 => 2,
    }];
    // The object the path is in, and the address it has reached.
    let mut object = base;
    let mut addr = base;
    for (step_index, step) in spec.steps.iter().enumerate() {
        match step {
            PeekStep::Deref => {
                object = read_guest_word(mem, addr)?;
                addr = object;
            }
            PeekStep::Bytes(n) => {
                addr = addr
                    .checked_add(*n)
                    .ok_or_else(|| format!("{addr:#x} + {n:#x} is past the end"))?;
            }
            PeekStep::Ivar(name) => {
                if object == 0 {
                    return Err(format!("nil before .{name}"));
                }
                let isa = read_guest_word(mem, object)?;
                let class: Class = Ptr::from_bits(isa);
                let offset = match ivar_offsets.get(&(step_index, class)) {
                    Some(&offset) => offset,
                    None => {
                        let offset = ivar_offset(objc, mem, class, name).ok_or_else(|| {
                            format!("{object:#x} has no class we know (isa {isa:#x})")
                        })?;
                        ivar_offsets.insert((step_index, class), offset);
                        offset
                    }
                }
                .ok_or_else(|| {
                    format!("{} has no ivar {name}", class_name_for_trace(objc, class))
                })?;
                addr = object
                    .checked_add(offset)
                    .ok_or_else(|| format!("{object:#x} + {offset:#x} is past the end"))?;
            }
        }
    }
    let word = match (spec.steps.is_empty(), spec.ty) {
        (true, _) => base,
        (false, PeekType::U8) => read_guest_bytes(mem, addr, 1)?[0].into(),
        (false, ty) => {
            debug_assert_eq!(ty.size(), 4);
            read_guest_word(mem, addr)?
        }
    };
    let class = match spec.ty {
        PeekType::Id if word != 0 => read_guest_word(mem, word)
            .ok()
            .map(|isa| class_name_for_trace(objc, Ptr::from_bits(isa))),
        _ => None,
    };
    Ok(peek::fmt_peek_value(spec.ty, word, class.as_deref()))
}

/// One `trace.log` row for a line the app printed with `NSLog`: the frame,
/// ms and thread as for any row, origin `nslog`, `-` for the variant and the
/// class, the message in the selector column with each tab, carriage return
/// and newline turned into a space, and `-` in the ten columns after it.
fn nslog_row(frame: u32, ms: f64, thread: usize, message: &str) -> String {
    let message = message.replace(['\t', '\r', '\n'], " ");
    let rest = ["-"; 10].join("\t");
    format!("{frame}\t{ms:.3}\t{thread}\tnslog\t-\t-\t{message}\t{rest}\n")
}

/// Called by `NSLog` with each line it prints (after the
/// `touchHLE_log.txt` line is written). When the trace is on, the line also
/// goes into `trace.log` as a row with origin `nslog` (see [nslog_row]), so
/// the app's own messages sit on the trace's timeline. One unbuffered write,
/// like every other row.
pub fn trace_app_log(env: &mut Environment, message: &str) {
    let elapsed_ms = env.startup_time.elapsed().as_secs_f64() * 1000.0;
    let thread = env.current_thread;
    let Some(trace) = env.objc.trace.as_mut() else {
        return;
    };
    let row = nslog_row(trace.frame, elapsed_ms, thread, message);
    let _ = trace.out.write_all(row.as_bytes());
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
                class_name_for_trace(&env.objc, class),
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
        class_name_for_trace(&env.objc, ObjC::read_isa(receiver, &env.mem))
    };
    let markers = selector_markers(env, selector).unwrap_or_default();
    write_line(
        env,
        "guest",
        "imp",
        &class_name,
        selector,
        SendVariant::Normal,
        receiver.to_bits(),
        markers.peeks,
    );
    note_markers(env, markers);

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
    use super::{
        capture_due, nslog_row, parse_selector_line, returns_struct, touch_row, TrampolineCache,
    };
    use crate::window::FingerId;

    #[test]
    fn nslog_rows_fill_every_column() {
        let row = nslog_row(7, 1234.5, 0, "Wave\tspawner\r\nfinished\n");
        assert_eq!(
            row,
            "7\t1234.500\t0\tnslog\t-\t-\tWave spawner  finished \t\
             -\t-\t-\t-\t-\t-\t-\t-\t-\t-\n"
        );
        // As many columns as the header names.
        assert_eq!(row.trim_end_matches('\n').split('\t').count(), 17);
    }

    /// The selector, `png`, the peeks' texts and the unknown words of a
    /// line, for short comparisons.
    fn summary(line: &str) -> Option<(String, bool, Vec<String>, Vec<String>)> {
        parse_selector_line(line).map(|l| {
            (
                l.selector,
                l.png,
                l.peeks.into_iter().map(|p| p.text).collect(),
                l.unknown,
            )
        })
    }

    #[test]
    fn selector_lines_take_markers() {
        let s = |v: &[&str]| v.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(summary(""), None);
        assert_eq!(summary("   # only a comment"), None);
        assert_eq!(
            summary("tLoop:"),
            Some(("tLoop:".into(), false, s(&[]), s(&[])))
        );
        assert_eq!(
            summary("  powerFire png  # the tower's shot"),
            Some(("powerFire".into(), true, s(&[]), s(&[])))
        );
        assert_eq!(
            summary("schedule:interval:\tpng"),
            Some(("schedule:interval:".into(), true, s(&[]), s(&[])))
        );
        assert_eq!(
            summary("powerFire PNG jpg png"),
            Some(("powerFire".into(), true, s(&[]), s(&["PNG", "jpg"])))
        );
    }

    #[test]
    fn selector_lines_take_peeks_in_any_order() {
        let s = |v: &[&str]| v.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(
            summary("dmg: self.hp:f32 png self.expectedHp:f32 # a hit"),
            Some((
                "dmg:".into(),
                true,
                s(&["self.hp:f32", "self.expectedHp:f32"]),
                s(&[])
            ))
        );
        // An item of --trace-selectors= once ';' became a space.
        assert_eq!(
            summary(&"addCreeps;self.sharedData->waveNum:i32".replace(';', " ")),
            Some((
                "addCreeps".into(),
                false,
                s(&["self.sharedData->waveNum:i32"]),
                s(&[])
            ))
        );
        // A peek that does not parse is set aside with the reason.
        let line = parse_selector_line("fireTarget: arg0.hp self.rate:f32").unwrap();
        assert_eq!(line.peeks.len(), 1);
        assert_eq!(line.bad_peeks.len(), 1);
        assert_eq!(line.bad_peeks[0].0, "arg0.hp");
        assert!(line.bad_peeks[0].1.contains("no :type"));
        assert!(line.unknown.is_empty());
    }

    #[test]
    fn frames_are_due_every_n_and_after_a_marker() {
        assert!(!capture_due(5, 0, false));
        assert!(capture_due(5, 0, true));
        assert!(capture_due(0, 10, false));
        assert!(!capture_due(5, 10, false));
        assert!(capture_due(20, 10, false));
        assert!(capture_due(7, 1, false));
    }

    #[test]
    fn touch_rows_are_tab_separated() {
        assert_eq!(
            touch_row(12, 3456.789, "down", FingerId::Touch(0), (160.0, 240.5)),
            "12\t3456.789\tdown\tTouch(0)\t160\t240.5\n"
        );
        assert_eq!(
            touch_row(0, 1.0, "up", FingerId::Mouse, (-1.0, 0.0)),
            "0\t1.000\tup\tMouse\t-1\t0\n"
        );
    }

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
