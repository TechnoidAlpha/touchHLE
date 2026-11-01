/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Watchpoints for the message trace (`trace_watch.txt` and
//! `--trace-watch=`): a row with origin `watch` for every write the guest
//! makes to a chosen field, with the old and new value and the writing
//! instruction. This part parses the lines and writes the rows; the CPU
//! side is in src/cpu.rs and the rest in the parent module.
//!
//! A line, words separated by spaces (`#` starts a comment):
//!
//! ```text
//! <target>:<type> [<name>]
//! ```
//!
//! - `<target>` is an address in hex, with or without `0x`, or
//!   `Class.ivar`, optionally followed by `.+N` (bytes further on, `0x`
//!   for hex), for the ivar of the first instance of `Class` (or of a
//!   subclass) allocated once the trace is on; when that object is freed,
//!   the next one allocated is watched.
//! - `<type>` is how the value is read and written, and how many bytes are
//!   watched: `u8`, `i8`, `u16`, `i16`, `u32`, `i32`, `f32`, `ptr`, `u64`,
//!   `i64` or `f64`.
//! - `<name>` goes in the row's selector column; without one, the target
//!   as written is the name.

use super::peek;

/// What a watched value is read as.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum WatchType {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    F32,
    Ptr,
    U64,
    I64,
    F64,
}

impl WatchType {
    fn parse(text: &str) -> Option<WatchType> {
        Some(match text {
            "u8" => WatchType::U8,
            "i8" => WatchType::I8,
            "u16" => WatchType::U16,
            "i16" => WatchType::I16,
            "u32" => WatchType::U32,
            "i32" => WatchType::I32,
            "f32" => WatchType::F32,
            "ptr" => WatchType::Ptr,
            "u64" => WatchType::U64,
            "i64" => WatchType::I64,
            "f64" => WatchType::F64,
            _ => return None,
        })
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            WatchType::U8 => "u8",
            WatchType::I8 => "i8",
            WatchType::U16 => "u16",
            WatchType::I16 => "i16",
            WatchType::U32 => "u32",
            WatchType::I32 => "i32",
            WatchType::F32 => "f32",
            WatchType::Ptr => "ptr",
            WatchType::U64 => "u64",
            WatchType::I64 => "i64",
            WatchType::F64 => "f64",
        }
    }

    /// How many bytes are watched.
    pub(super) fn size(self) -> u32 {
        match self {
            WatchType::U8 | WatchType::I8 => 1,
            WatchType::U16 | WatchType::I16 => 2,
            WatchType::U32 | WatchType::I32 | WatchType::F32 | WatchType::Ptr => 4,
            WatchType::U64 | WatchType::I64 | WatchType::F64 => 8,
        }
    }

    /// `bytes` (the value's little-endian bytes as a number) as a column
    /// shows it: decimal, a float with six significant digits (as a peek
    /// writes it), or hex for `ptr`.
    pub(super) fn format(self, bytes: u64) -> String {
        match self {
            WatchType::U8 => (bytes as u8).to_string(),
            WatchType::I8 => (bytes as u8 as i8).to_string(),
            WatchType::U16 => (bytes as u16).to_string(),
            WatchType::I16 => (bytes as u16 as i16).to_string(),
            WatchType::U32 => (bytes as u32).to_string(),
            WatchType::I32 => (bytes as u32 as i32).to_string(),
            WatchType::F32 => peek::fmt_f32_6(f32::from_bits(bytes as u32)),
            WatchType::Ptr => format!("{:#010x}", bytes as u32),
            WatchType::U64 => bytes.to_string(),
            WatchType::I64 => (bytes as i64).to_string(),
            WatchType::F64 => format!("{}", f64::from_bits(bytes)),
        }
    }
}

/// What is watched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum WatchTarget {
    /// A fixed address.
    Addr(u32),
    /// The ivar `ivar` of an instance of `class`, `offset` bytes on.
    Ivar {
        class: String,
        ivar: String,
        offset: u32,
    },
}

/// One line of `trace_watch.txt`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WatchLine {
    pub(super) target: WatchTarget,
    pub(super) ty: WatchType,
    pub(super) name: String,
}

/// A number in hex with `0x`, or decimal.
fn parse_offset(text: &str) -> Option<u32> {
    match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

/// Parse one line (see the module doc). `Ok(None)` for a blank line or a
/// comment, `Err` with the reason for a line that is not understood.
pub(super) fn parse_watch_line(line: &str) -> Result<Option<WatchLine>, String> {
    let line = line.split('#').next().unwrap();
    let mut words = line.split_whitespace();
    let Some(first) = words.next() else {
        return Ok(None);
    };
    let (target_text, ty) = first
        .rsplit_once(':')
        .ok_or_else(|| format!("{first:?} has no :type"))?;
    let ty = WatchType::parse(ty).ok_or_else(|| {
        format!("unknown type {ty:?} (u8, i8, u16, i16, u32, i32, f32, ptr, u64, i64 or f64)")
    })?;
    let hex = target_text
        .strip_prefix("0x")
        .or_else(|| target_text.strip_prefix("0X"))
        .unwrap_or(target_text);
    let target = if let Ok(addr) = u32::from_str_radix(hex, 16) {
        WatchTarget::Addr(addr)
    } else {
        let mut parts = target_text.split('.');
        let class = parts.next().unwrap_or("");
        let ivar = parts.next().unwrap_or("");
        let is_name =
            |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !is_name(class) || !is_name(ivar) || class.starts_with(|c: char| c.is_ascii_digit()) {
            return Err(format!(
                "{target_text:?} is neither an address in hex nor Class.ivar"
            ));
        }
        let offset = match parts.next() {
            None => 0,
            Some(step) => step
                .strip_prefix('+')
                .and_then(parse_offset)
                .ok_or_else(|| format!("expected .+offset after {class}.{ivar}, got .{step}"))?,
        };
        if let Some(extra) = parts.next() {
            return Err(format!("unexpected .{extra}"));
        }
        WatchTarget::Ivar {
            class: class.to_string(),
            ivar: ivar.to_string(),
            offset,
        }
    };
    let name = words.next().unwrap_or(target_text).to_string();
    if let Some(extra) = words.next() {
        return Err(format!("unexpected word {extra:?} after the name"));
    }
    Ok(Some(WatchLine { target, ty, name }))
}

/// The header line of one watch: `# watch <name> <target> <type>`.
pub(super) fn watch_header_line(line: &WatchLine) -> String {
    let target = match &line.target {
        WatchTarget::Addr(addr) => format!("{addr:#010x}"),
        WatchTarget::Ivar {
            class,
            ivar,
            offset: 0,
        } => format!("{class}.{ivar}"),
        WatchTarget::Ivar {
            class,
            ivar,
            offset,
        } => format!("{class}.{ivar}.+{offset:#x}"),
    };
    format!("# watch {} {} {}\n", line.name, target, line.ty.name())
}

/// What a `watch` row says, see [watch_row].
pub(super) enum WatchEvent {
    /// A write to the watched bytes.
    Write {
        /// Where the write went and its size in bytes.
        addr: u32,
        size: u32,
        old: u64,
        new: u64,
        /// The writing instruction's address, and the link register then,
        /// if known.
        pc: Option<u32>,
        lr: Option<u32>,
    },
    /// A `Class.ivar` watch starts on the object (its current value given).
    Arm { value: u64 },
    /// The object of a `Class.ivar` watch was freed.
    Disarm,
}

/// What every `watch` row has, see [watch_row].
pub(super) struct WatchRowHead<'a> {
    pub(super) frame: u32,
    pub(super) ms: f64,
    pub(super) thread: usize,
    pub(super) name: &'a str,
    pub(super) ty: WatchType,
    /// The watched address.
    pub(super) field: u32,
    /// The object of a `Class.ivar` watch.
    pub(super) object: Option<u32>,
}

/// One `watch` row, with the 17 columns of a call row: the variant column
/// is `write`, `arm` or `disarm`; `class` is the watch's type; `selector`
/// its name; `arg0` the watched address; `arg0_f32` the old value (the
/// current one for `arm`); `arg1` the object (`-` for a fixed address);
/// `arg1_f32` the new value; `sp0` the write's address and `sp1` its size
/// (it may cover more than the watched bytes, or part of them); `sp2` and
/// `sp3` the old and new watched bytes in hex; `lr` the link register; and
/// `self` the address of the writing instruction (`lr` and `self` are `-`
/// when not known).
pub(super) fn watch_row(head: &WatchRowHead, event: &WatchEvent) -> String {
    let object = head
        .object
        .map_or("-".to_string(), |o| format!("{o:#010x}"));
    let digits = 2 * head.ty.size() as usize;
    let hex = |word: Option<u32>| word.map_or("-".to_string(), |w| format!("{w:#010x}"));
    let rest = match *event {
        WatchEvent::Write {
            addr,
            size,
            old,
            new,
            pc,
            lr,
        } => format!(
            "write\t{}\t{}\t{:#010x}\t{}\t{}\t{}\t{:#010x}\t{}\t0x{:0digits$x}\t0x{:0digits$x}\t{}\t{}",
            head.ty.name(),
            head.name,
            head.field,
            head.ty.format(old),
            object,
            head.ty.format(new),
            addr,
            size,
            old,
            new,
            hex(lr),
            hex(pc),
        ),
        WatchEvent::Arm { value } => format!(
            "arm\t{}\t{}\t{:#010x}\t{}\t{}\t-\t-\t-\t0x{:0digits$x}\t-\t-\t-",
            head.ty.name(),
            head.name,
            head.field,
            head.ty.format(value),
            object,
            value,
        ),
        WatchEvent::Disarm => format!(
            "disarm\t{}\t{}\t{:#010x}\t-\t{}\t-\t-\t-\t-\t-\t-\t-",
            head.ty.name(),
            head.name,
            head.field,
            object,
        ),
    };
    format!(
        "{}\t{:.3}\t{}\twatch\t{}\n",
        head.frame, head.ms, head.thread, rest
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_give_target_type_and_name() {
        assert_eq!(
            parse_watch_line("0x0012a4c8:f32 gold # SharedData +0x68"),
            Ok(Some(WatchLine {
                target: WatchTarget::Addr(0x12a4c8),
                ty: WatchType::F32,
                name: "gold".to_string()
            }))
        );
        assert_eq!(
            parse_watch_line("12a4c8:u8"),
            Ok(Some(WatchLine {
                target: WatchTarget::Addr(0x12a4c8),
                ty: WatchType::U8,
                name: "12a4c8".to_string()
            }))
        );
        assert_eq!(
            parse_watch_line("SharedData.totalGold:f32"),
            Ok(Some(WatchLine {
                target: WatchTarget::Ivar {
                    class: "SharedData".to_string(),
                    ivar: "totalGold".to_string(),
                    offset: 0
                },
                ty: WatchType::F32,
                name: "SharedData.totalGold".to_string()
            }))
        );
        assert_eq!(
            parse_watch_line("Game.next.+0xc:f32 nextHeight").map(|l| l.unwrap().target),
            Ok(WatchTarget::Ivar {
                class: "Game".to_string(),
                ivar: "next".to_string(),
                offset: 12
            })
        );
        assert_eq!(parse_watch_line("  # nothing"), Ok(None));
        for (line, reason) in [
            ("0x1000", "no :type"),
            ("0x1000:f16", "unknown type"),
            ("Game:f32", "neither"),
            ("Game.:f32", "neither"),
            ("Game.next.4:f32", "expected .+offset"),
            ("Game.next.+4.+4:f32", "unexpected"),
            ("0x1000:u32 a b", "unexpected word"),
        ] {
            match parse_watch_line(line) {
                Ok(parsed) => panic!("{line} parsed as {parsed:?}"),
                Err(e) => assert!(e.contains(reason), "{line}: {e}, wanted {reason}"),
            }
        }
    }

    #[test]
    fn values_are_written_by_type() {
        assert_eq!(WatchType::I8.format(0xff), "-1");
        assert_eq!(WatchType::U16.format(0xffff), "65535");
        assert_eq!(WatchType::I32.format(0xffff_fffe), "-2");
        assert_eq!(WatchType::F32.format(0x4120_0000), "10");
        assert_eq!(WatchType::Ptr.format(0x1234), "0x00001234");
        assert_eq!(WatchType::F64.format(1.5f64.to_bits()), "1.5");
        assert_eq!(WatchType::I64.format(u64::MAX), "-1");
    }

    #[test]
    fn rows_have_the_call_row_columns() {
        let head = WatchRowHead {
            frame: 7,
            ms: 100.25,
            thread: 0,
            name: "gold",
            ty: WatchType::F32,
            field: 0x0012_a4c8,
            object: None,
        };
        let row = watch_row(
            &head,
            &WatchEvent::Write {
                addr: 0x0012_a4c8,
                size: 4,
                old: 0x41f0_0000,
                new: 0,
                pc: Some(0x0003_a4e6),
                lr: Some(0x0003_a001),
            },
        );
        assert_eq!(
            row,
            "7\t100.250\t0\twatch\twrite\tf32\tgold\t0x0012a4c8\t30\t-\t0\t0x0012a4c8\t4\t0x41f00000\t0x00000000\t0x0003a001\t0x0003a4e6\n"
        );
        assert_eq!(row.trim_end().split('\t').count(), 17);
        let head = WatchRowHead {
            object: Some(0x0020_0000),
            ty: WatchType::U8,
            ..head
        };
        let row = watch_row(&head, &WatchEvent::Arm { value: 1 });
        assert_eq!(
            row,
            "7\t100.250\t0\twatch\tarm\tu8\tgold\t0x0012a4c8\t1\t0x00200000\t-\t-\t-\t0x01\t-\t-\t-\n"
        );
        assert_eq!(row.trim_end().split('\t').count(), 17);
        let row = watch_row(&head, &WatchEvent::Disarm);
        assert_eq!(row.trim_end().split('\t').count(), 17);
        let row = watch_row(
            &head,
            &WatchEvent::Write {
                addr: 0x0012_a4c8,
                size: 4,
                old: 0,
                new: 1,
                pc: None,
                lr: None,
            },
        );
        assert!(row.ends_with("\t0x01\t-\t-\n"), "{row}");
    }
}
