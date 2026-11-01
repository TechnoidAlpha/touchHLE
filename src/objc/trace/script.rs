/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Scripted input for the message trace (`--input-script=FILE`, or
//! `input_script.txt`, and `--quit-at-frame=F`): touches and a quit at
//! chosen frames, on the trace's frame clock, so a run can play the game
//! unattended. This part parses the file and decides, frame by frame, what
//! to do; the touches go through the window's event queue and the rows into
//! `trace.log` in the parent module.
//!
//! One command per line, words separated by spaces (`#` starts a comment):
//!
//! ```text
//! [@F | +N] tap X Y
//! [@F | +N] down X Y
//! [@F | +N] move X Y
//! [@F | +N] up X Y
//! [@F | +N] wait <target>:<type> <op> <value> [timeout T]
//! [@F | +N] quit
//! ```
//!
//! - `@F` runs the command at presented frame F (the frame numbers of
//!   `trace.log` and `input.log`), `+N` N frames after the command before
//!   it ran (after a `wait`, from the frame it passed or timed out). With
//!   neither, `+0`. Commands run in order, so an `@F` that comes after a
//!   `wait` still holding runs when the wait is over.
//! - `tap` is a `down` at (X, Y) and an `up` at the same point 3 frames
//!   later. X and Y are guest points, unrotated, as `input.log` writes them.
//! - `wait` holds the script until the value reads true. `<target>:<type>`
//!   is as for a watchpoint (see [super::watch]), `<op>` one of `>=`, `<=`,
//!   `==`, `>`, `<` and `!=`, `<value>` a number. With `timeout T`, after T
//!   frames of waiting the script carries on anyway.
//! - `quit` exits touchHLE through its usual exit path.
//!
//! A row of `input.log` (frame, ms, phase, finger, x, y) is also a line:
//! it is read as `@frame phase x y`, so the rows of one run play it again.

use super::watch::{parse_watch_line, WatchTarget, WatchType};

/// A touch phase, as the game receives it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Down,
    Move,
    Up,
}

impl Phase {
    fn parse(text: &str) -> Option<Phase> {
        Some(match text {
            "down" => Phase::Down,
            "move" => Phase::Move,
            "up" => Phase::Up,
            _ => return None,
        })
    }
}

/// A comparison of a `wait`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Op {
    Ge,
    Le,
    Eq,
    Gt,
    Lt,
    Ne,
}

impl Op {
    fn parse(text: &str) -> Option<Op> {
        Some(match text {
            ">=" => Op::Ge,
            "<=" => Op::Le,
            "==" => Op::Eq,
            ">" => Op::Gt,
            "<" => Op::Lt,
            "!=" => Op::Ne,
            _ => return None,
        })
    }

    fn holds(self, value: f64, against: f64) -> bool {
        match self {
            Op::Ge => value >= against,
            Op::Le => value <= against,
            Op::Eq => value == against,
            Op::Gt => value > against,
            Op::Lt => value < against,
            Op::Ne => value != against,
        }
    }
}

/// What a `wait` waits for.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct WaitCond {
    pub(super) target: WatchTarget,
    pub(super) ty: WatchType,
    op: Op,
    value: f64,
    timeout: Option<u32>,
}

/// When a command runs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Timing {
    /// At this presented frame.
    At(u32),
    /// This many frames after the command before it ran.
    After(u32),
}

/// What a command does.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Action {
    Tap(f32, f32),
    Touch(Phase, f32, f32),
    Wait(WaitCond),
    Quit,
}

/// One line of the script.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Command {
    pub(super) timing: Timing,
    pub(super) action: Action,
    /// The line as written, words joined by one space, for the rows.
    pub(super) text: String,
}

/// The value of a watched type's little-endian bytes as a number.
fn as_f64(ty: WatchType, bytes: u64) -> f64 {
    match ty {
        WatchType::U8 => bytes as u8 as f64,
        WatchType::I8 => bytes as u8 as i8 as f64,
        WatchType::U16 => bytes as u16 as f64,
        WatchType::I16 => bytes as u16 as i16 as f64,
        WatchType::U32 | WatchType::Ptr => bytes as u32 as f64,
        WatchType::I32 => bytes as u32 as i32 as f64,
        WatchType::F32 => f32::from_bits(bytes as u32) as f64,
        WatchType::U64 => bytes as f64,
        WatchType::I64 => bytes as i64 as f64,
        WatchType::F64 => f64::from_bits(bytes),
    }
}

fn parse_coords(words: &[&str]) -> Result<(f32, f32), String> {
    match words {
        [x, y] => match (x.parse::<f32>(), y.parse::<f32>()) {
            (Ok(x), Ok(y)) => Ok((x, y)),
            _ => Err(format!("{x:?} {y:?} are not two numbers")),
        },
        _ => Err("expected two numbers, X and Y".to_string()),
    }
}

fn parse_frames(text: &str, what: &str) -> Result<u32, String> {
    text.parse()
        .map_err(|_| format!("{what} {text:?} is not a number of frames"))
}

/// Parse one line (see the module doc). `Ok(None)` for a blank line or a
/// comment, `Err` with the reason for a line that is not understood.
pub(super) fn parse_script_line(line: &str) -> Result<Option<Command>, String> {
    let line = line.split('#').next().unwrap();
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.is_empty() {
        return Ok(None);
    }
    // A row of input.log: frame, ms, phase, finger, x, y.
    if words.len() == 6 && words[1].parse::<f64>().is_ok() {
        if let (Ok(frame), Some(phase)) = (words[0].parse::<u32>(), Phase::parse(words[2])) {
            let (x, y) = parse_coords(&words[4..])?;
            return Ok(Some(Command {
                timing: Timing::At(frame),
                action: Action::Touch(phase, x, y),
                text: format!("@{} {} {} {}", frame, words[2], words[4], words[5]),
            }));
        }
    }
    let text = words.join(" ");
    let (timing, rest) = if let Some(f) = words[0].strip_prefix('@') {
        (Timing::At(parse_frames(f, "@")?), &words[1..])
    } else if let Some(n) = words[0].strip_prefix('+') {
        (Timing::After(parse_frames(n, "+")?), &words[1..])
    } else {
        (Timing::After(0), &words[..])
    };
    let Some((&command, args)) = rest.split_first() else {
        return Err("a timing with no command".to_string());
    };
    let action = match command {
        "tap" => {
            let (x, y) = parse_coords(args)?;
            Action::Tap(x, y)
        }
        "down" | "move" | "up" => {
            let (x, y) = parse_coords(args)?;
            Action::Touch(Phase::parse(command).unwrap(), x, y)
        }
        "quit" => {
            if !args.is_empty() {
                return Err(format!("unexpected {:?} after quit", args[0]));
            }
            Action::Quit
        }
        "wait" => {
            let [target, op, value, timeout @ ..] = args else {
                return Err("expected wait <target>:<type> <op> <value> [timeout T]".to_string());
            };
            let watch = parse_watch_line(target)?.ok_or("no target")?;
            let op = Op::parse(op)
                .ok_or_else(|| format!("unknown comparison {op:?} (>=, <=, ==, >, < or !=)"))?;
            let value: f64 = value
                .parse()
                .map_err(|_| format!("{value:?} is not a number"))?;
            let timeout = match timeout {
                [] => None,
                ["timeout", t] => Some(parse_frames(t, "timeout")?),
                _ => return Err(format!("expected timeout T, got {:?}", timeout.join(" "))),
            };
            Action::Wait(WaitCond {
                target: watch.target,
                ty: watch.ty,
                op,
                value,
                timeout,
            })
        }
        _ => return Err(format!("unknown command {command:?}")),
    };
    Ok(Some(Command {
        timing,
        action,
        text,
    }))
}

/// What the script asks for at a frame, see [Script::tick].
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Out {
    /// A touch for the window's event queue.
    Touch(Phase, f32, f32),
    /// A `trace.log` row with origin `input`, this text in the selector
    /// column.
    Row(String),
    /// A message for the log (a wait's target could not be read, the
    /// script is done).
    Note(String),
    /// Exit touchHLE.
    Quit,
}

/// How many frames after its `down` a `tap` lifts.
pub(super) const TAP_FRAMES: u32 = 3;

/// The script and where it stands.
#[derive(Default)]
pub(super) struct Script {
    commands: Vec<Command>,
    /// The next command to run.
    next: usize,
    /// The frame the command before [Script::next] ran, which `+N` counts
    /// from.
    anchor: u32,
    /// The `up`s of taps still to come: the frame, the point and the tap's
    /// text.
    ups: Vec<(u32, f32, f32, String)>,
    /// While a `wait` holds: the frame it started.
    wait_since: Option<u32>,
    /// The last value the `wait` read, and whether a failed read has been
    /// noted.
    wait_last: Option<String>,
    wait_error_noted: bool,
    /// `--quit-at-frame=`.
    quit_at: Option<u32>,
    /// Set once the script has quit, and once the end was noted.
    quit: bool,
    end_noted: bool,
}

impl Script {
    /// Parse a whole file. Lines that are not understood are skipped and
    /// returned with their line number (from 1) and the reason.
    pub(super) fn parse(text: &str) -> (Vec<Command>, Vec<(usize, String)>) {
        let mut commands = Vec::new();
        let mut bad = Vec::new();
        for (n, line) in text.lines().enumerate() {
            match parse_script_line(line) {
                Ok(Some(command)) => commands.push(command),
                Ok(None) => (),
                Err(why) => bad.push((n + 1, why)),
            }
        }
        (commands, bad)
    }

    pub(super) fn new(commands: Vec<Command>, quit_at: Option<u32>) -> Script {
        Script {
            end_noted: commands.is_empty(),
            commands,
            quit_at,
            ..Script::default()
        }
    }

    /// Called once per presented frame with that frame's number: what to do
    /// now. `read` reads a `wait`'s target, as the bytes of its type (in a
    /// `u64`, little-endian), or says why it cannot.
    pub(super) fn tick(
        &mut self,
        frame: u32,
        mut read: impl FnMut(&WaitCond) -> Result<u64, String>,
    ) -> Vec<Out> {
        let mut out = Vec::new();
        if self.quit {
            return out;
        }
        self.ups.retain(|(due, x, y, text)| {
            if *due > frame {
                return true;
            }
            out.push(Out::Touch(Phase::Up, *x, *y));
            out.push(Out::Row(format!("{text}: up")));
            false
        });
        if self.quit_at.is_some_and(|at| frame >= at) {
            out.push(Out::Row(format!(
                "quit (--quit-at-frame={})",
                self.quit_at.unwrap()
            )));
            out.push(Out::Quit);
            self.quit = true;
            return out;
        }
        while let Some(command) = self.commands.get(self.next) {
            let due = match command.timing {
                Timing::At(f) => f,
                Timing::After(n) => self.anchor.saturating_add(n),
            };
            if frame < due {
                break;
            }
            match &command.action {
                Action::Tap(x, y) => {
                    out.push(Out::Touch(Phase::Down, *x, *y));
                    out.push(Out::Row(command.text.clone()));
                    self.ups
                        .push((frame + TAP_FRAMES, *x, *y, command.text.clone()));
                }
                Action::Touch(phase, x, y) => {
                    out.push(Out::Touch(*phase, *x, *y));
                    out.push(Out::Row(command.text.clone()));
                }
                Action::Quit => {
                    out.push(Out::Row(command.text.clone()));
                    out.push(Out::Quit);
                    self.quit = true;
                    return out;
                }
                Action::Wait(cond) => {
                    let since = *self.wait_since.get_or_insert_with(|| {
                        out.push(Out::Row(format!("{}: waiting", command.text)));
                        frame
                    });
                    match read(cond) {
                        Ok(bytes) => {
                            self.wait_last = Some(cond.ty.format(bytes));
                            if cond.op.holds(as_f64(cond.ty, bytes), cond.value) {
                                out.push(Out::Row(format!(
                                    "{}: passed after {} frames ({})",
                                    command.text,
                                    frame - since,
                                    self.wait_last.as_deref().unwrap()
                                )));
                                self.end_wait();
                                self.anchor = frame;
                                self.next += 1;
                                continue;
                            }
                        }
                        Err(why) => {
                            if !self.wait_error_noted {
                                self.wait_error_noted = true;
                                out.push(Out::Note(format!(
                                    "Input script: {}: cannot read the target yet ({}), still waiting (logged once per wait)",
                                    command.text, why
                                )));
                            }
                        }
                    }
                    if cond.timeout.is_some_and(|t| frame - since >= t) {
                        out.push(Out::Row(format!(
                            "{}: timed out after {} frames (last {})",
                            command.text,
                            frame - since,
                            self.wait_last.as_deref().unwrap_or("-")
                        )));
                        out.push(Out::Note(format!(
                            "Input script: {}: timed out at frame {}, carrying on",
                            command.text, frame
                        )));
                        self.end_wait();
                        self.anchor = frame;
                        self.next += 1;
                        continue;
                    }
                    break;
                }
            }
            self.anchor = frame;
            self.next += 1;
        }
        if self.next == self.commands.len() && !self.end_noted {
            self.end_noted = true;
            out.push(Out::Note(format!(
                "Input script: all {} commands done at frame {}",
                self.commands.len(),
                frame
            )));
        }
        out
    }

    fn end_wait(&mut self) {
        self.wait_since = None;
        self.wait_last = None;
        self.wait_error_noted = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> Command {
        parse_script_line(text).unwrap().unwrap()
    }

    /// Run `script` from frame 0 to `last`, with `read` for the waits, and
    /// give each frame's outputs that are not empty.
    fn run(
        script: &mut Script,
        last: u32,
        mut read: impl FnMut(u32, &WaitCond) -> Result<u64, String>,
    ) -> Vec<(u32, Vec<Out>)> {
        (0..=last)
            .filter_map(|frame| {
                let out = script.tick(frame, |cond| read(frame, cond));
                (!out.is_empty()).then_some((frame, out))
            })
            .collect()
    }

    fn touches(frames: &[(u32, Vec<Out>)]) -> Vec<(u32, Phase, f32, f32)> {
        frames
            .iter()
            .flat_map(|(frame, out)| {
                out.iter().filter_map(move |o| match o {
                    Out::Touch(phase, x, y) => Some((*frame, *phase, *x, *y)),
                    _ => None,
                })
            })
            .collect()
    }

    fn rows(frames: &[(u32, Vec<Out>)]) -> Vec<(u32, String)> {
        frames
            .iter()
            .flat_map(|(frame, out)| {
                out.iter().filter_map(move |o| match o {
                    Out::Row(text) => Some((*frame, text.clone())),
                    _ => None,
                })
            })
            .collect()
    }

    #[test]
    fn lines_give_timing_command_and_point() {
        assert_eq!(
            line("@700 tap 160 214 # PLAY"),
            Command {
                timing: Timing::At(700),
                action: Action::Tap(160.0, 214.0),
                text: "@700 tap 160 214".to_string(),
            }
        );
        assert_eq!(line("+30 down 1.5 2").timing, Timing::After(30));
        assert_eq!(
            line("+30 down 1.5 2").action,
            Action::Touch(Phase::Down, 1.5, 2.0)
        );
        assert_eq!(line("move 3 4").timing, Timing::After(0));
        assert_eq!(line("+2   up  3 4").text, "+2 up 3 4");
        assert_eq!(line("@9000 quit").action, Action::Quit);
        assert_eq!(parse_script_line("   # only a comment"), Ok(None));
        assert_eq!(parse_script_line(""), Ok(None));
    }

    #[test]
    fn wait_lines_give_target_type_op_value_and_timeout() {
        let Action::Wait(cond) = line("wait SharedData.totalGold:f32 >= 60 timeout 900").action
        else {
            panic!()
        };
        assert_eq!(
            cond.target,
            WatchTarget::Ivar {
                class: "SharedData".to_string(),
                ivar: "totalGold".to_string(),
                offset: 0,
            }
        );
        assert_eq!(cond.ty, WatchType::F32);
        assert_eq!(
            (cond.op, cond.value, cond.timeout),
            (Op::Ge, 60.0, Some(900))
        );
        let Action::Wait(cond) = line("+5 wait 0x0012a4c8:i32 != -1").action else {
            panic!()
        };
        assert_eq!(cond.target, WatchTarget::Addr(0x12a4c8));
        assert_eq!(
            (cond.ty, cond.op, cond.timeout),
            (WatchType::I32, Op::Ne, None)
        );
    }

    #[test]
    fn input_log_rows_are_read_as_touches_at_their_frame() {
        assert_eq!(
            line("812\t27100.250\tdown\tMouse\t160\t214.5"),
            Command {
                timing: Timing::At(812),
                action: Action::Touch(Phase::Down, 160.0, 214.5),
                text: "@812 down 160 214.5".to_string(),
            }
        );
        assert_eq!(
            line("815\t27200.000\tup\tTouch(0)\t160\t214").action,
            Action::Touch(Phase::Up, 160.0, 214.0)
        );
        // input.log's header lines are comments.
        assert_eq!(
            parse_script_line("# frame\tms\tphase\tfinger\tx\ty"),
            Ok(None)
        );
    }

    #[test]
    fn bad_lines_are_skipped_with_their_number_and_reason() {
        let (commands, bad) = Script::parse(
            "@10 tap 1 2\njump 3 4\n@x tap 1 2\ntap 1\nwait Foo.bar:f99 >= 1\nwait Foo.bar:f32 => 1\n+3\nquit now\nwait Foo.bar:f32 >= 1 timeout\n@20 quit\n",
        );
        assert_eq!(commands.len(), 2);
        let numbers: Vec<usize> = bad.iter().map(|(n, _)| *n).collect();
        assert_eq!(numbers, [2, 3, 4, 5, 6, 7, 8, 9]);
        assert!(bad[0].1.contains("unknown command"));
        assert!(bad[4].1.contains("comparison"));
    }

    #[test]
    fn at_and_plus_fire_at_their_frames_and_a_tap_lifts_three_frames_later() {
        let (commands, bad) =
            Script::parse("@5 tap 10 20\n+10 tap 30 40\n+1 down 1 1\n+2 move 2 2\n+0 up 2 2\n");
        assert!(bad.is_empty());
        let mut script = Script::new(commands, None);
        let frames = run(&mut script, 40, |_, _| unreachable!());
        assert_eq!(
            touches(&frames),
            [
                (5, Phase::Down, 10.0, 20.0),
                (8, Phase::Up, 10.0, 20.0),
                (15, Phase::Down, 30.0, 40.0),
                (16, Phase::Down, 1.0, 1.0),
                (18, Phase::Up, 30.0, 40.0),
                (18, Phase::Move, 2.0, 2.0),
                (18, Phase::Up, 2.0, 2.0),
            ]
        );
        let rows = rows(&frames);
        assert_eq!(rows[0], (5, "@5 tap 10 20".to_string()));
        assert_eq!(rows[1], (8, "@5 tap 10 20: up".to_string()));
        // The end is noted once.
        let notes = frames
            .iter()
            .flat_map(|(_, out)| out.iter())
            .filter(|o| matches!(o, Out::Note(text) if text.contains("all 5 commands done")))
            .count();
        assert_eq!(notes, 1);
    }

    #[test]
    fn an_at_in_the_past_fires_at_once_in_order() {
        let mut script = Script::new(Script::parse("@3 down 1 1\n@1 up 1 1\n").0, None);
        let frames = run(&mut script, 5, |_, _| unreachable!());
        assert_eq!(
            touches(&frames),
            [(3, Phase::Down, 1.0, 1.0), (3, Phase::Up, 1.0, 1.0)]
        );
    }

    #[test]
    fn a_wait_holds_until_its_value_passes_and_plus_counts_from_there() {
        let mut script = Script::new(
            Script::parse("@2 wait SharedData.totalGold:f32 >= 60\n+4 tap 5 6\n").0,
            None,
        );
        // The gold reaches 60 at frame 10.
        let frames = run(&mut script, 20, |frame, cond| {
            assert_eq!(cond.ty, WatchType::F32);
            Ok(f32::to_bits(if frame >= 10 { 60.0 } else { 45.0 }) as u64)
        });
        assert_eq!(touches(&frames)[0], (14, Phase::Down, 5.0, 6.0));
        let rows = rows(&frames);
        assert_eq!(
            rows[0],
            (
                2,
                "@2 wait SharedData.totalGold:f32 >= 60: waiting".to_string()
            )
        );
        assert_eq!(
            rows[1],
            (
                10,
                format!(
                    "@2 wait SharedData.totalGold:f32 >= 60: passed after 8 frames ({})",
                    super::super::peek::fmt_f32_6(60.0)
                )
            )
        );
    }

    #[test]
    fn a_wait_that_cannot_read_notes_it_once_and_times_out() {
        let mut script = Script::new(
            Script::parse("wait Foo.bar:i32 == 1 timeout 5\n+1 tap 1 1\n").0,
            None,
        );
        let frames = run(&mut script, 10, |_, _| Err("no Foo".to_string()));
        let notes: Vec<&Out> = frames
            .iter()
            .flat_map(|(_, out)| out.iter())
            .filter(|o| matches!(o, Out::Note(_)))
            .collect();
        assert!(matches!(notes[0], Out::Note(text) if text.contains("no Foo")));
        assert!(matches!(notes[1], Out::Note(text) if text.contains("timed out at frame 5")));
        assert_eq!(
            rows(&frames)[1],
            (
                5,
                "wait Foo.bar:i32 == 1 timeout 5: timed out after 5 frames (last -)".to_string()
            )
        );
        assert_eq!(touches(&frames)[0], (6, Phase::Down, 1.0, 1.0));
    }

    #[test]
    fn a_timeout_shows_the_last_value_read() {
        let mut script = Script::new(Script::parse("wait 0x1000:i32 > 3 timeout 2\n").0, None);
        let frames = run(&mut script, 4, |_, _| Ok(0xffff_ffff));
        assert_eq!(
            rows(&frames)[1],
            (
                2,
                "wait 0x1000:i32 > 3 timeout 2: timed out after 2 frames (last -1)".to_string()
            )
        );
    }

    #[test]
    fn quit_stops_the_script_and_quit_at_frame_wins_over_a_wait() {
        let mut script = Script::new(Script::parse("@4 quit\n@6 tap 1 1\n").0, None);
        let frames = run(&mut script, 10, |_, _| unreachable!());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, 4);
        assert_eq!(frames[0].1.last(), Some(&Out::Quit));

        let mut script = Script::new(Script::parse("wait Foo.bar:u8 == 1\n").0, Some(7));
        let frames = run(&mut script, 10, |_, _| Ok(0));
        let (frame, out) = frames.last().unwrap();
        assert_eq!(*frame, 7);
        assert_eq!(
            out[..],
            [Out::Row("quit (--quit-at-frame=7)".to_string()), Out::Quit]
        );
        // Without a script.
        let mut script = Script::new(Vec::new(), Some(3));
        let frames = run(&mut script, 10, |_, _| unreachable!());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, 3);
    }
}
