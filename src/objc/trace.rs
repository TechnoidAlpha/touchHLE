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
//! selector or `--capture-frames=` or `--dump-scene-every=` is set. When it
//! is off, the only cost per message is one `Option::is_some` check.
//!
//! A line of `trace_selectors.txt` may add marker words after the selector,
//! separated by spaces (see [parse_selector_line]). The only one is `png`:
//! `powerFire png` saves the first frame presented after each call of
//! `powerFire`, the frame that shows its result. `--capture-frames=N` saves
//! every Nth frame as well, up to `--capture-frames-max=` frames a run
//! (3000 by default). Frames are read back on the CAEAGLLayer fast
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
//! as a line, with `;` between them (`dmg:;self.hp:f32`). A `str` peek
//! writes what an `NSString` says, from touchHLE's own host object for it
//! (see [ns_string::host_string_text]), so `setString: arg0:str` shows the
//! text a label is given.
//!
//! A selector word may end with `@ClassName` (`setPosition:@CCLabelTTF`):
//! then only calls whose receiver's class, or one of its superclasses, is
//! named `ClassName` are traced (see [Traced]). A class method's receiver
//! is a class, whose class is its metaclass, which has the same name, so
//! `@CCLabelTTF` takes `+[CCLabelTTF labelWithString:...]` as well. The
//! same selector may be listed with several classes, and without one.
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
//!
//! A scene dump (see [scene]) writes what cocos2d has on screen, one line
//! per node, to `scene/s{frame:06}_{ms}.txt`, with a `mark` row in
//! `trace.log` and the frame's picture in `frames/`. It is asked for with
//! F11 (see [trace_request_scene_dump]), with the `scene` marker on a line
//! of `trace_selectors.txt`, and every Nth frame with
//! `--dump-scene-every=N`, and is taken when the next frame is presented
//! (see [trace_present_frame]).

mod peek;
mod scene;

use super::{id, nil, Class, ClassHostObject, ObjC, IMP, SEL};
use crate::abi::GuestFunction;
use crate::cpu::Cpu;
use crate::dyld::HostFunction;
use crate::frame_capture::{FrameCapture, FRAMES_DIR};
use crate::frameworks::foundation::ns_string;
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

/// What a traced call asks for besides its line, see [Traced::markers_for].
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
struct Markers {
    /// `png`: save the first frame presented after each call.
    png: bool,
    /// `scene`: dump the scene when the next frame is presented.
    scene: bool,
    /// The selector's peeks: an index into [MsgTrace::peek_sets], or [None]
    /// if it has none.
    peeks: Option<usize>,
}

/// The receivers one or more lines of a selector trace it for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Filter {
    /// `@ClassName`: receivers whose class, or a superclass of it, has that
    /// name. [None]: any receiver.
    class: Option<String>,
    /// `png` on any of those lines.
    png: bool,
    /// `scene` on any of those lines.
    scene: bool,
}

/// A traced selector: who it is traced for, and its peeks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Traced {
    /// One per receiver class it is listed with (and one for a line with
    /// no class).
    filters: Vec<Filter>,
    /// The selector's peeks, shared by all its lines whatever their class,
    /// see [Markers::peeks].
    peeks: Option<usize>,
}

impl Traced {
    /// Add a line of this selector with receiver class `class` and the
    /// `png` and `scene` markers if `png` and `scene`. A class listed twice
    /// gets one filter.
    fn add(&mut self, class: Option<&str>, png: bool, scene: bool) {
        match self
            .filters
            .iter_mut()
            .find(|f| f.class.as_deref() == class)
        {
            Some(filter) => {
                filter.png |= png;
                filter.scene |= scene;
            }
            None => self.filters.push(Filter {
                class: class.map(str::to_string),
                png,
                scene,
            }),
        }
    }

    /// Whether any line names a class, so the receiver's class matters.
    fn by_class(&self) -> bool {
        self.filters.iter().any(|f| f.class.is_some())
    }

    /// The markers of a call whose receiver's class chain (its class, then
    /// each superclass) has the names `chain`, or [None] if no filter takes
    /// it. `png` (`scene`) is set if a filter that takes it has `png`
    /// (`scene`).
    fn markers_for<S: AsRef<str>>(&self, chain: &[S]) -> Option<Markers> {
        let mut taken = false;
        let mut png = false;
        let mut scene = false;
        for filter in &self.filters {
            let takes = filter
                .class
                .as_deref()
                .is_none_or(|class| chain.iter().any(|name| name.as_ref() == class));
            if takes {
                taken = true;
                png |= filter.png;
                scene |= filter.scene;
            }
        }
        taken.then_some(Markers {
            png,
            scene,
            peeks: self.peeks,
        })
    }
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
    /// The traced selectors.
    traced: Vec<Traced>,
    /// Each traced selector's index in [MsgTrace::traced], by name.
    by_name: HashMap<String, usize>,
    /// The peeks of each selector that has any, see [Markers::peeks].
    peek_sets: Vec<PeekSet>,
    /// The index in [MsgTrace::traced] of each (uniqued) selector pointer, or
    /// [None] if it is not traced, so the string comparison happens once per
    /// selector.
    cache: HashMap<SEL, Option<usize>>,
    /// For a selector with a class filter, the markers per receiver class
    /// ([None]: not traced for that class), so each class chain is walked
    /// once per selector.
    class_cache: HashMap<(SEL, Class), Option<Markers>>,
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
    /// `--capture-frames-max=`, for a capture started by a scene dump.
    capture_max: u32,
    /// Set once the frame capture was started, or failed to start, or
    /// finished: a scene dump never starts it (and empties `frames/`)
    /// again, see [start_capture_for_scene].
    capture_no_restart: bool,
    /// The scene dumps, see [scene].
    scene: SceneDumps,
}

/// The state of the scene dumps (see [scene] and [trace_present_frame]).
#[derive(Default)]
struct SceneDumps {
    /// `--dump-scene-every=N`: dump the scene every Nth frame. 0 is off.
    every: u32,
    /// Why a dump was asked for since the last frame (F11, a `scene`
    /// selector), each reason once, in order.
    pending: Vec<String>,
    /// The `scene` folder once it is ready (earlier dumps removed), or
    /// `Err` if making it ready failed (then no dump is written).
    dir: Option<Result<std::path::PathBuf, ()>>,
    /// The address of the static variable that held the `CCDirector` last
    /// time, see [scene::find_singleton].
    director_slot: Option<u32>,
    /// The same for the `CCTextureCache`.
    texture_cache_slot: Option<u32>,
    /// Whether a missing director has been logged.
    no_director_logged: bool,
    /// Whether a missing texture cache has been logged.
    no_texture_cache_logged: bool,
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
    /// The first word as written, for the header.
    word: String,
    /// The selector, without `@ClassName`.
    selector: String,
    /// The `ClassName` of `@ClassName`, if the word ends with one.
    class: Option<String>,
    /// Why the first word is not a selector: the line is ignored, logged.
    error: Option<String>,
    /// The `png` marker.
    png: bool,
    /// The `scene` marker.
    scene: bool,
    /// The peek specs, in the order given.
    peeks: Vec<PeekSpec>,
    /// Words that are neither a marker nor a peek: ignored, logged once.
    unknown: Vec<String>,
    /// Peeks that did not parse, with the reason: ignored, logged.
    bad_peeks: Vec<(String, String)>,
}

/// One line of `trace_selectors.txt`: the selector, optionally followed by
/// `@ClassName` in the same word, then optional marker words and peek specs
/// in any order, separated by spaces or tabs; `#` starts a comment. Returns
/// [None] for a blank or comment-only line. The markers are `png` and
/// `scene`; a word starting with `self`, `arg0` or `arg1` is a peek (see
/// [peek]); any other word is ignored, and logged once by the caller.
fn parse_selector_line(line: &str) -> Option<SelectorLine> {
    let line = line.split('#').next().unwrap();
    let mut words = line.split_whitespace();
    let word = words.next()?;
    let (selector, class) = match word.split_once('@') {
        Some((selector, class)) => (selector, Some(class)),
        None => (word, None),
    };
    let error = if selector.is_empty() {
        Some("no selector before @".to_string())
    } else if class.is_some_and(|c| c.is_empty() || c.contains('@')) {
        Some("@ must be followed by one class name".to_string())
    } else {
        None
    };
    let mut parsed = SelectorLine {
        word: word.to_string(),
        selector: selector.to_string(),
        class: class.map(str::to_string),
        error,
        ..SelectorLine::default()
    };
    for word in words {
        if word == "png" {
            parsed.png = true;
        } else if word == "scene" {
            parsed.scene = true;
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

/// `--capture-frames-max=` in words, for the trace header and the log.
fn capture_cap(capture_max: u32) -> String {
    match capture_max {
        0 => "no cap".to_string(),
        n => format!("at most {n} frames"),
    }
}

/// `--dump-scene-every=` in words, for the trace header and the log.
fn periodic_scene(every: u32) -> String {
    match every {
        0 => "no periodic dump".to_string(),
        1 => "every frame".to_string(),
        n => format!("every {n}th frame"),
    }
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
    /// there are any or `capture_every` or `scene_every` is not 0. At most
    /// `capture_max` frames are saved (0: no cap). Returns [None] when the
    /// trace is off.
    fn open(
        option_selectors: &[String],
        capture_every: u32,
        capture_max: u32,
        scene_every: u32,
        bundle_id: &str,
    ) -> Option<MsgTrace> {
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
        // The same selector may be listed twice, with the same receiver
        // class or another; its markers and peeks add up.
        let mut unknown_logged: HashSet<String> = HashSet::new();
        // The selector words as written (with any @ClassName), for the
        // header, and those with png.
        let mut list: Vec<String> = Vec::new();
        let mut png_words: HashSet<String> = HashSet::new();
        let mut scene_words: HashSet<String> = HashSet::new();
        let mut traced: Vec<Traced> = Vec::new();
        let mut by_name: HashMap<String, usize> = HashMap::new();
        let mut peek_sets: Vec<PeekSet> = Vec::new();
        for (source, line) in lines {
            if let Some(error) = &line.error {
                log!(
                    "Warning: {} has a selector {:?} that is not understood, its line is ignored: {}",
                    source,
                    line.word,
                    error
                );
                continue;
            }
            for word in &line.unknown {
                if unknown_logged.insert(word.clone()) {
                    log!(
                        "Warning: {} has an unknown marker {:?} (after {}), ignored; the markers are png and scene, and a peek starts with self, arg0 or arg1",
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
            if !list.contains(&line.word) {
                list.push(line.word.clone());
            }
            if line.png {
                png_words.insert(line.word.clone());
            }
            if line.scene {
                scene_words.insert(line.word.clone());
            }
            let index = *by_name.entry(line.selector.clone()).or_insert_with(|| {
                traced.push(Traced::default());
                traced.len() - 1
            });
            let entry = &mut traced[index];
            entry.add(line.class.as_deref(), line.png, line.scene);
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
        if traced.is_empty() && capture_every == 0 && scene_every == 0 {
            return None;
        }
        let png_after: Vec<&str> = list
            .iter()
            .filter(|s| png_words.contains(*s))
            .map(String::as_str)
            .collect();
        let scene_after: Vec<&str> = list
            .iter()
            .filter(|s| scene_words.contains(*s))
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
             # selector@Class: only receivers of Class or a subclass, class methods included\n\
             # no imp lines for methods that return a struct or a union\n\
             # frame capture (frames/f<frame>_<ms>.png): {}; png after: {}; {}\n\
             # a captured frame is read back on the emulator thread, so the calls\n\
             # around it are timed a little slower than without capture\n\
             # origin nslog: a line the app printed with NSLog, in the selector column\n\
             # origin mark: a scene dump (scene/s<frame>_<ms>.txt, its picture in frames/),\n\
             # the reason in the selector column; dumps: {}; scene after: {}; F11\n\
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
            capture_cap(capture_max),
            periodic_scene(scene_every),
            if scene_after.is_empty() {
                "none".to_string()
            } else {
                scene_after.join(",")
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
            match FrameCapture::start(dir.clone(), capture_max) {
                Ok(capture) => {
                    log!(
                        "Frame capture on ({}, after {} png selector(s), {}), writing to {}",
                        periodic_capture(capture_every),
                        png_after.len(),
                        capture_cap(capture_max),
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
            traced,
            by_name,
            peek_sets,
            cache: HashMap::new(),
            class_cache: HashMap::new(),
            trampolines: TrampolineCache::new(),
            not_wrapped: HashSet::new(),
            out,
            frame: 0,
            input,
            capture,
            capture_every,
            capture_pending: false,
            uncaptured_path_logged: false,
            capture_max,
            capture_no_restart: capture_every > 0 || !png_after.is_empty(),
            scene: SceneDumps {
                every: scene_every,
                ..SceneDumps::default()
            },
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
    /// `capture_every` (`--capture-frames=`) or `scene_every`
    /// (`--dump-scene-every=`) is not 0. `capture_max` is
    /// `--capture-frames-max=`.
    pub fn start_trace(
        &mut self,
        option_selectors: &[String],
        capture_every: u32,
        capture_max: u32,
        scene_every: u32,
        bundle_id: &str,
    ) {
        self.trace = MsgTrace::open(
            option_selectors,
            capture_every,
            capture_max,
            scene_every,
            bundle_id,
        );
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
///
/// A scene dump asked for since the last frame (F11, a `scene` selector),
/// or due by `--dump-scene-every=`, is written here, before the picture is
/// read back, under the same frame number (see [write_scene_dump]), and the
/// picture is saved with it.
pub fn trace_present_frame(env: &mut Environment) -> Option<FrameCaptureTicket> {
    let elapsed = env.startup_time.elapsed();
    let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    let trace = env.objc.trace.as_mut()?;
    let frame = trace.frame;
    trace.frame = trace.frame.wrapping_add(1);
    if let Some(reason) = scene_due(frame, trace.scene.every, &mut trace.scene.pending) {
        // The picture goes with the dump.
        trace.capture_pending = true;
        start_capture_for_scene(trace);
        write_scene_dump(env, frame, elapsed.as_secs_f64() * 1000.0, ms, &reason);
    }
    let trace = env.objc.trace.as_mut()?;
    let due = capture_due(frame, trace.capture_every, trace.capture_pending);
    // Past the cap nothing is read back, so the frame costs nothing more.
    if !due || trace.capture.as_mut().is_none_or(FrameCapture::at_cap) {
        return None;
    }
    trace.capture_pending = false;
    Some(FrameCaptureTicket { frame, ms })
}

/// Whether the frame numbered `frame` gets a scene dump, and why: the
/// reasons asked for since the last frame (`pending`, which is emptied),
/// and `periodic` every `every`th frame (counting from frame 0; `every` = 0
/// is off), joined with `, `. [None] if no dump is due.
fn scene_due(frame: u32, every: u32, pending: &mut Vec<String>) -> Option<String> {
    let mut reasons = std::mem::take(pending);
    if every != 0 && frame.is_multiple_of(every) {
        reasons.push("periodic".to_string());
    }
    (!reasons.is_empty()).then(|| reasons.join(", "))
}

/// Add `reason` to the reasons for the next scene dump, unless it is there.
fn add_scene_reason(pending: &mut Vec<String>, reason: &str) {
    if !pending.iter().any(|r| r == reason) {
        pending.push(reason.to_string());
    }
}

/// Start the frame capture for a scene dump's picture, if it is not on: a
/// run with neither `--capture-frames=` nor a `png` marker starts it at its
/// first dump. It is started at most once a run, so `frames/` is emptied of
/// an earlier run's pictures at most once.
fn start_capture_for_scene(trace: &mut MsgTrace) {
    if trace.capture.is_some() || trace.capture_no_restart {
        return;
    }
    trace.capture_no_restart = true;
    let dir = crate::paths::user_data_base_path().join(FRAMES_DIR);
    match FrameCapture::start(dir.clone(), trace.capture_max) {
        Ok(capture) => {
            log!(
                "Frame capture on for the scene dumps' pictures ({}), writing to {}",
                capture_cap(trace.capture_max),
                dir.display()
            );
            trace.capture = Some(capture);
        }
        Err(e) => {
            log!(
                "Warning: frame capture is off, scene dumps have no picture: {}",
                e
            );
        }
    }
}

/// Write a scene dump of frame `frame`, presented `ms` milliseconds after
/// startup (`whole_ms` cut to a whole number), asked for by `reason`: a
/// `mark` row in `trace.log` (see [mark_row]) and the dump in
/// `scene/s{frame:06}_{whole_ms}.txt` (see [scene]). The `scene` folder is
/// emptied of an earlier run's dumps before the first. Only reads guest
/// memory and the class table: nothing here calls the guest. A dump that
/// cannot be written is logged and the run goes on.
fn write_scene_dump(env: &mut Environment, frame: u32, ms: f64, whole_ms: u64, reason: &str) {
    let thread = env.current_thread;
    let viewport = env.window.as_ref().map(|window| window.viewport());
    // Taken out while it is used, so the class table can be read alongside.
    let Some(mut trace) = env.objc.trace.take() else {
        return;
    };
    let file_name = scene::scene_file_name(frame, whole_ms);
    let row = mark_row(
        frame,
        ms,
        thread,
        &format!("scene: {reason} ({}/{file_name})", scene::SCENE_DIR),
    );
    let _ = trace.out.write_all(row.as_bytes());

    let dir = trace
        .scene
        .dir
        .get_or_insert_with(|| {
            let dir = crate::paths::user_data_base_path().join(scene::SCENE_DIR);
            match scene::prepare_dir(&dir) {
                Ok(()) => {
                    log!("Scene dumps on, writing to {}", dir.display());
                    Ok(dir)
                }
                Err(e) => {
                    log!("Warning: scene dumps are off: {}", e);
                    Err(())
                }
            }
        })
        .clone();
    if let Ok(dir) = dir {
        let SceneDumps {
            director_slot,
            texture_cache_slot,
            no_director_logged,
            no_texture_cache_logged,
            ..
        } = &mut trace.scene;
        let mut guest = scene::GuestScene::new(&env.objc, &env.mem);
        let mut find = |slot: &mut Option<u32>, class: &str| {
            env.bins
                .first()
                .and_then(|bin| scene::find_singleton(&mut guest, bin, slot, class))
        };
        let director = find(director_slot, "CCDirector");
        let texture_cache = find(texture_cache_slot, "CCTextureCache");
        if director.is_none() && !std::mem::replace(no_director_logged, true) {
            log!("Scene dump: no CCDirector found yet, nothing is walked (logged once)");
        }
        let files = texture_cache.and_then(|cache| scene::texture_files(&mut guest, cache));
        if files.is_none() && !std::mem::replace(no_texture_cache_logged, true) {
            log!("Scene dump: the CCTextureCache's textures could not be read, tex_file is - (logged once)");
        }
        let info = scene::DumpInfo {
            frame,
            ms: whole_ms,
            trigger: reason,
            viewport,
            textures: files.as_ref().map(HashMap::len),
        };
        let text = scene::dump_text(&mut guest, &info, director, &files.unwrap_or_default());
        let path = dir.join(&file_name);
        match std::fs::write(&path, text) {
            Ok(()) if reason != "periodic" => {
                log!("Scene dump ({}) written to {}", reason, path.display());
            }
            Ok(()) => (),
            Err(e) => {
                log!("Warning: could not write {}: {}", path.display(), e);
            }
        }
    }
    env.objc.trace = Some(trace);
}

/// Called when F11 is pressed (or the Android wrapper sends it): ask for a
/// scene dump, taken when the next frame is presented (see
/// [trace_present_frame]). When the trace is off nothing is written, and
/// that is logged once.
pub fn trace_request_scene_dump(env: &mut Environment, reason: &str) {
    static TRACE_OFF_LOGGED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    let Some(trace) = env.objc.trace.as_mut() else {
        if !TRACE_OFF_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            log!(
                "Scene dump ({}) not taken: the message trace is off; a selector, --capture-frames= or --dump-scene-every= turns it on (logged once)",
                reason
            );
        }
        return;
    };
    add_scene_reason(&mut trace.scene.pending, reason);
    log!(
        "Scene dump asked for ({}), taken when the next frame is presented",
        reason
    );
}

/// How long [trace_finish] waits for the frame capture worker.
const FINISH_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Called on UIKit's exit path just before touchHLE exits: lets the frame
/// capture worker write the frames still queued, waiting at most
/// [FINISH_WAIT]. A stop that does not go through that path (a crash, a
/// killed process) can still lose the last few frames.
pub fn trace_finish(env: &mut Environment) {
    let Some(trace) = env.objc.trace.as_mut() else {
        return;
    };
    // A scene dump after this must not start a new capture, which would
    // empty frames/.
    trace.capture_no_restart = true;
    if let Some(capture) = trace.capture.take() {
        capture.finish(FINISH_WAIT);
    }
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

/// The index in [MsgTrace::traced] of `selector` if it is one of the traced
/// ones. The trace must be on.
fn traced_index(env: &mut Environment, selector: SEL) -> Option<usize> {
    let mem = &*env.mem;
    let MsgTrace { by_name, cache, .. } = env.objc.trace.as_mut()?;
    *cache.entry(selector).or_insert_with(|| {
        if selector.is_null() {
            return None;
        }
        selector
            .try_as_str(mem)
            .and_then(|s| by_name.get(s).copied())
    })
}

/// The markers of a call of `selector` on `receiver` if it is traced: the
/// selector is one of the traced ones, and one of its lines takes the
/// receiver's class (see [Traced::markers_for]). The trace must be on.
fn selector_markers(env: &mut Environment, selector: SEL, receiver: id) -> Option<Markers> {
    let index = traced_index(env, selector)?;
    let trace = env.objc.trace.as_ref()?;
    let traced = &trace.traced[index];
    if !traced.by_class() {
        return traced.markers_for::<&str>(&[]);
    }
    let class = if receiver == nil {
        nil
    } else {
        ObjC::read_isa(receiver, &env.mem)
    };
    if let Some(&markers) = trace.class_cache.get(&(selector, class)) {
        return markers;
    }
    let chain = class_chain_names(&env.objc, class);
    let trace = env.objc.trace.as_mut()?;
    let markers = trace.traced[index].markers_for(&chain);
    trace.class_cache.insert((selector, class), markers);
    markers
}

/// Whether `selector` is one of the traced ones, for any receiver. The
/// trace must be on.
fn selector_is_traced(env: &mut Environment, selector: SEL) -> bool {
    traced_index(env, selector).is_some()
}

/// The names of `class` and of each of its superclasses, in that order (for
/// a metaclass, the metaclasses' names, which are their classes' names, and
/// last the root class). Empty for nil. Never panics: the walk stops at a
/// class touchHLE has no full record of.
fn class_chain_names(objc: &ObjC, class: Class) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = class;
    // A bound, in case a broken chain loops.
    for _ in 0..64 {
        if current == nil {
            break;
        }
        if let Some(name) = objc.try_get_class_name(current) {
            names.push(name.to_string());
        }
        match objc
            .get_host_object(current)
            .and_then(|o| o.as_any().downcast_ref::<ClassHostObject>())
        {
            Some(host) => current = host.superclass,
            None => break,
        }
    }
    names
}

/// Act on a traced call's markers once its line is written: `png` asks for
/// the next frame's picture, `scene` for a scene dump at the next frame,
/// with `selector <name>` as the reason.
fn note_markers(env: &mut Environment, markers: Markers, selector: SEL) {
    let reason = markers
        .scene
        .then(|| format!("selector {}", selector.try_as_str(&env.mem).unwrap_or("?")));
    if let Some(trace) = env.objc.trace.as_mut() {
        trace.capture_pending |= markers.png;
        if let Some(reason) = reason {
            add_scene_reason(&mut trace.scene.pending, &reason);
        }
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
    let Some(markers) = selector_markers(env, selector, receiver) else {
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
    note_markers(env, markers, selector);
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
    let class_of = |object: u32| {
        read_guest_word(mem, object)
            .ok()
            .map(|isa| class_name_for_trace(objc, Ptr::from_bits(isa)))
    };
    if spec.ty == PeekType::Str && word != 0 {
        // STR_MAX_BYTES + 1 code units are always enough to tell whether
        // the escaped text has to be cut: each one is at least a byte.
        let text = ns_string::host_string_text(objc, Ptr::from_bits(word), peek::STR_MAX_BYTES + 1)
            .ok_or_else(|| {
                format!(
                    "{word:#x} is not one of touchHLE's strings (class {})",
                    class_of(word).as_deref().unwrap_or("unknown")
                )
            })?;
        return Ok(peek::fmt_peek_value(spec.ty, word, Some(&text)));
    }
    let class = match spec.ty {
        PeekType::Id if word != 0 => class_of(word),
        _ => None,
    };
    Ok(peek::fmt_peek_value(spec.ty, word, class.as_deref()))
}

/// One `trace.log` row of text rather than a call: the frame, ms and thread
/// as for any row, `origin`, `-` for the variant and the class, the text in
/// the selector column with each tab, carriage return and newline turned
/// into a space, and `-` in the ten columns after it.
fn text_row(frame: u32, ms: f64, thread: usize, origin: &str, text: &str) -> String {
    let text = text.replace(['\t', '\r', '\n'], " ");
    let rest = ["-"; 10].join("\t");
    format!("{frame}\t{ms:.3}\t{thread}\t{origin}\t-\t-\t{text}\t{rest}\n")
}

/// The row for a line the app printed with `NSLog`: origin `nslog`.
fn nslog_row(frame: u32, ms: f64, thread: usize, message: &str) -> String {
    text_row(frame, ms, thread, "nslog", message)
}

/// The row for an app lifecycle step: origin `lifecycle`, and
/// `lifecycle: <step>` in the selector column.
fn lifecycle_row(frame: u32, ms: f64, thread: usize, step: &str) -> String {
    text_row(
        frame,
        ms,
        thread,
        "lifecycle",
        &format!("lifecycle: {step}"),
    )
}

/// Called by UIKit at each app lifecycle step it tells the app about
/// (`resign active`, `background`, `foreground`, `active`, `terminate`).
/// When the trace is on, the step goes into `trace.log` as a row with origin
/// `lifecycle` (see [lifecycle_row]), whether or not the delegate's methods
/// are traced selectors, so a trace shows where the app went to the
/// background and came back.
pub fn trace_lifecycle(env: &mut Environment, step: &str) {
    let elapsed_ms = env.startup_time.elapsed().as_secs_f64() * 1000.0;
    let thread = env.current_thread;
    let Some(trace) = env.objc.trace.as_mut() else {
        return;
    };
    let row = lifecycle_row(trace.frame, elapsed_ms, thread, step);
    let _ = trace.out.write_all(row.as_bytes());
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

/// One `trace.log` row for a scene dump: the frame, ms and thread as for
/// any row, origin `mark`, `-` for the variant and the class, `text` (the
/// reason and the file) in the selector column with each tab, carriage
/// return and newline turned into a space, and `-` in the ten columns after
/// it, like an `nslog` row (see [nslog_row]).
fn mark_row(frame: u32, ms: f64, thread: usize, text: &str) -> String {
    let text = text.replace(['\t', '\r', '\n'], " ");
    let rest = ["-"; 10].join("\t");
    format!("{frame}\t{ms:.3}\t{thread}\tmark\t-\t-\t{text}\t{rest}\n")
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

/// Runs when the guest calls a trampoline. Writes the trace line (if the
/// selector's class filters take the receiver, see [selector_markers]),
/// then sets the PC (and the Thumb state) to the real IMP, so the guest
/// carries on inside the real method with every register (r0-r3, SP, LR) as
/// the caller left it, and the method returns straight to the caller. The
/// stub's own return instruction never runs. Only methods that do not return
/// a struct get a trampoline, so the receiver is in r0 and the arguments
/// start at r2.
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
    // The stub is made for any receiver; a class filter is applied here.
    let Some(markers) = selector_markers(env, selector, receiver) else {
        env.cpu.branch(real);
        return;
    };
    let class_name = if receiver == nil {
        "nil".to_string()
    } else {
        class_name_for_trace(&env.objc, ObjC::read_isa(receiver, &env.mem))
    };
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
    note_markers(env, markers, selector);

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
        capture_due, class_chain_names, lifecycle_row, nslog_row, parse_selector_line,
        returns_struct, touch_row, Class, ClassHostObject, Filter, Markers, ObjC, Ptr, Traced,
        TrampolineCache,
    };
    use crate::objc::classes::InitializationStatus;
    use crate::objc::{nil, TrivialHostObject};
    use crate::window::FingerId;
    use std::collections::HashMap;

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

    #[test]
    fn lifecycle_rows_fill_every_column() {
        let row = lifecycle_row(120, 45678.25, 0, "background");
        assert_eq!(
            row,
            "120\t45678.250\t0\tlifecycle\t-\t-\tlifecycle: background\t\
             -\t-\t-\t-\t-\t-\t-\t-\t-\t-\n"
        );
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
    fn selector_words_take_a_receiver_class() {
        let line = parse_selector_line("setPosition:@CCLabelTTF png arg0:str").unwrap();
        assert_eq!(line.word, "setPosition:@CCLabelTTF");
        assert_eq!(line.selector, "setPosition:");
        assert_eq!(line.class.as_deref(), Some("CCLabelTTF"));
        assert_eq!(line.error, None);
        assert!(line.png);
        assert_eq!(line.peeks.len(), 1);
        assert_eq!(line.peeks[0].text, "arg0:str");
        // Without '@' the word is the selector and there is no class.
        let line = parse_selector_line("setString: arg0:str").unwrap();
        assert_eq!(line.word, "setString:");
        assert_eq!(line.selector, "setString:");
        assert_eq!(line.class, None);
        assert_eq!(line.error, None);
        // An item of --trace-selectors= once ';' became a space.
        let line = parse_selector_line(&"tLoop:@Game;png".replace(';', " ")).unwrap();
        assert_eq!(line.selector, "tLoop:");
        assert_eq!(line.class.as_deref(), Some("Game"));
        assert!(line.png);
        // A class method's selector without arguments.
        let line = parse_selector_line("sharedDirector@CCDirector").unwrap();
        assert_eq!(line.selector, "sharedDirector");
        assert_eq!(line.class.as_deref(), Some("CCDirector"));
        // Words that are not a selector and a class.
        for word in ["@CCLabelTTF", "setString:@", "a:@B@C", "@"] {
            let line = parse_selector_line(word).unwrap();
            assert!(line.error.is_some(), "{word}");
            assert_eq!(line.word, word);
        }
    }

    fn filter(class: Option<&str>, png: bool) -> Filter {
        Filter {
            class: class.map(str::to_string),
            png,
            scene: false,
        }
    }

    #[test]
    fn lines_of_one_selector_add_up_per_class() {
        let mut traced = Traced::default();
        traced.add(Some("CCLabelTTF"), false, false);
        traced.add(Some("CCLabelBMFont"), true, false);
        traced.add(Some("CCLabelTTF"), true, false);
        traced.add(None, false, false);
        assert_eq!(
            traced.filters,
            vec![
                filter(Some("CCLabelTTF"), true),
                filter(Some("CCLabelBMFont"), true),
                filter(None, false),
            ]
        );
        assert!(traced.by_class());
        let mut plain = Traced::default();
        plain.add(None, false, false);
        plain.add(None, true, false);
        assert_eq!(plain.filters, vec![filter(None, true)]);
        assert!(!plain.by_class());
    }

    #[test]
    fn class_filters_match_the_class_or_a_superclass() {
        let traced = Traced {
            filters: vec![
                filter(Some("CCNode"), false),
                filter(Some("CCMenuItemLabel"), true),
            ],
            peeks: Some(2),
        };
        let label = ["CCLabelTTF", "CCSprite", "CCNode", "NSObject"];
        let item = ["CCMenuItemLabel", "CCMenuItem", "CCNode", "NSObject"];
        // A superclass's name takes the call.
        assert_eq!(
            traced.markers_for(&label),
            Some(Markers {
                png: false,
                scene: false,
                peeks: Some(2)
            })
        );
        // png only from a filter that takes the receiver.
        assert_eq!(
            traced.markers_for(&item),
            Some(Markers {
                png: true,
                scene: false,
                peeks: Some(2)
            })
        );
        // Not on the chain, a nil receiver, and a name that is only a prefix.
        assert_eq!(traced.markers_for(&["Creeps", "NSObject"]), None);
        assert_eq!(traced.markers_for::<&str>(&[]), None);
        let ttf = Traced {
            filters: vec![filter(Some("CCLabel"), false)],
            peeks: None,
        };
        assert_eq!(ttf.markers_for(&label), None);
        // A class method: the metaclass chain has the same names, and ends
        // with the root class after the root metaclass.
        let metaclass = ["CCLabelTTF", "CCSprite", "CCNode", "NSObject", "NSObject"];
        let by_label = Traced {
            filters: vec![filter(Some("CCLabelTTF"), false)],
            peeks: None,
        };
        assert!(by_label.markers_for(&metaclass).is_some());
        // A line without a class takes any receiver, nil included.
        let any = Traced {
            filters: vec![filter(None, true)],
            peeks: None,
        };
        assert_eq!(
            any.markers_for::<&str>(&[]),
            Some(Markers {
                png: true,
                scene: false,
                peeks: None
            })
        );
        assert!(any.markers_for(&label).is_some());
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

    /// A class's host object, as touchHLE keeps it, with empty method and
    /// ivar tables.
    fn class_object(name: &str, is_metaclass: bool, superclass: Class) -> Box<ClassHostObject> {
        Box::new(ClassHostObject {
            name: name.to_string(),
            is_metaclass,
            superclass,
            methods: HashMap::new(),
            guest_method_signatures: HashMap::new(),
            ivars: HashMap::new(),
            instance_start: 0,
            instance_size: 0,
            is_initialized: InitializationStatus::NotInitialized,
        })
    }

    /// [class_chain_names] reads only the object table, so a bare [ObjC]
    /// with classes at made-up addresses is enough.
    #[test]
    fn class_chain_names_walks_to_the_root() {
        let at = |bits| -> Class { Ptr::from_bits(bits) };
        let mut objc = ObjC::new();
        let mut class = |bits, name, is_metaclass, superclass| {
            objc.register_static_object(at(bits), class_object(name, is_metaclass, superclass))
        };
        // NSObject <- CCNode <- CCSprite <- CCLabelTTF, and their
        // metaclasses, the root metaclass's superclass being the root class.
        class(0x100, "NSObject", false, nil);
        class(0x104, "CCNode", false, at(0x100));
        class(0x108, "CCSprite", false, at(0x104));
        class(0x10c, "CCLabelTTF", false, at(0x108));
        class(0x200, "NSObject", true, at(0x100));
        class(0x204, "CCNode", true, at(0x200));
        class(0x208, "CCSprite", true, at(0x204));
        class(0x20c, "CCLabelTTF", true, at(0x208));
        // A class whose superclass touchHLE has no record of.
        class(0x300, "Orphan", false, at(0x3f0));
        // A class whose superclass is some other kind of object.
        class(0x304, "OddParent", false, at(0x308));
        // A class that is its own superclass.
        class(0x30c, "Loop", false, at(0x30c));
        objc.register_static_object(at(0x308), Box::new(TrivialHostObject));

        let names = |bits| class_chain_names(&objc, at(bits));
        assert_eq!(
            names(0x10c),
            ["CCLabelTTF", "CCSprite", "CCNode", "NSObject"]
        );
        assert_eq!(names(0x104), ["CCNode", "NSObject"]);
        assert_eq!(names(0x100), ["NSObject"]);
        assert_eq!(
            names(0x20c),
            ["CCLabelTTF", "CCSprite", "CCNode", "NSObject", "NSObject"]
        );
        // The walk stops where the record does, without a panic.
        assert_eq!(names(0x300), ["Orphan"]);
        assert_eq!(names(0x304), ["OddParent"]);
        assert!(names(0x308).is_empty());
        assert!(names(0x3f0).is_empty());
        assert!(class_chain_names(&objc, nil).is_empty());
        // A loop ends at the bound.
        let looped = names(0x30c);
        assert_eq!(looped.len(), 64);
        assert!(looped.iter().all(|name| name == "Loop"));
    }

    #[test]
    fn mark_rows_fill_every_column() {
        let row = super::mark_row(42, 15873.25, 0, "scene: key F11\t(scene/x.txt)\n");
        assert_eq!(
            row,
            "42\t15873.250\t0\tmark\t-\t-\tscene: key F11 (scene/x.txt) \t\
             -\t-\t-\t-\t-\t-\t-\t-\t-\t-\n"
        );
        // As many columns as the header names, like an nslog row.
        assert_eq!(row.trim_end_matches('\n').split('\t').count(), 17);
        assert_eq!(
            row.split('\t').count(),
            nslog_row(42, 15873.25, 0, "x").split('\t').count()
        );
    }

    #[test]
    fn selector_lines_take_the_scene_marker() {
        let line = parse_selector_line("powerFire scene png self.lvl:i32").unwrap();
        assert!(line.scene);
        assert!(line.png);
        assert_eq!(line.peeks.len(), 1);
        assert!(line.unknown.is_empty());
        let line = parse_selector_line("startWave@Game;scene".replace(';', " ").as_str()).unwrap();
        assert!(line.scene);
        assert!(!line.png);
        assert!(!parse_selector_line("tLoop:").unwrap().scene);
        assert_eq!(
            parse_selector_line("tLoop: SCENE").unwrap().unknown,
            ["SCENE"]
        );
        // The marker is per receiver class, like png.
        let mut traced = Traced::default();
        traced.add(Some("CCLabelTTF"), false, true);
        traced.add(None, false, false);
        let label = ["CCLabelTTF", "CCSprite", "CCNode", "NSObject"];
        assert!(traced.markers_for(&label).unwrap().scene);
        assert!(!traced.markers_for(&["Creeps", "NSObject"]).unwrap().scene);
    }

    #[test]
    fn scene_dumps_are_due_when_asked_and_every_n() {
        let mut pending = Vec::new();
        assert_eq!(super::scene_due(5, 0, &mut pending), None);
        assert_eq!(
            super::scene_due(20, 10, &mut pending).as_deref(),
            Some("periodic")
        );
        assert_eq!(super::scene_due(21, 10, &mut pending), None);
        super::add_scene_reason(&mut pending, "key F11");
        super::add_scene_reason(&mut pending, "selector powerFire");
        super::add_scene_reason(&mut pending, "key F11");
        assert_eq!(
            super::scene_due(30, 10, &mut pending).as_deref(),
            Some("key F11, selector powerFire, periodic")
        );
        // Asked for once: the next frame has no dump.
        assert!(pending.is_empty());
        assert_eq!(super::scene_due(31, 10, &mut pending), None);
    }
}
