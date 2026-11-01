/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Address hooks for the message trace (`trace_addresses.txt` and
//! `--trace-addresses=`): a row with origin `addr` each time execution
//! reaches a chosen guest address, with r0 to r3, sp, lr and peeks. This
//! part parses the lines and writes the rows; the CPU side is in
//! src/cpu.rs and the reading in the parent module.
//!
//! A line, words separated by spaces (`#` starts a comment):
//!
//! ```text
//! <address> [arm|thumb] [<name>] [<peek>...]
//! ```
//!
//! - `<address>` is hex, with or without `0x` (the decompile writes
//!   `@0005bb24`; a leading `@` is allowed too). An odd address is Thumb
//!   code at the address one below, as a function pointer with the Thumb
//!   bit is.
//! - `arm` or `thumb` says which instruction set the hook is for. Without
//!   it (and with an even address) the hook is for both.
//! - `<name>` is any word that is not a peek; it goes in the row's selector
//!   column. Without one, the address is the name.
//! - A peek is a peek of the selector trace whose base is `r0` to `r3`,
//!   `sp` or a hex address (see [super::peek::parse_hook_peek]).

use super::peek::{self, PeekSpec};

/// What one line of `trace_addresses.txt` asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HookLine {
    /// The address, without the Thumb bit.
    pub(super) addr: u32,
    /// Hook it when it runs as Arm code, and as Thumb code.
    pub(super) arm: bool,
    pub(super) thumb: bool,
    pub(super) name: String,
    pub(super) peeks: Vec<PeekSpec>,
    /// Peeks that did not parse, with the reason: ignored, logged.
    pub(super) bad_peeks: Vec<(String, String)>,
    /// Words after the name that are not peeks: ignored, logged.
    pub(super) unknown: Vec<String>,
}

/// Parse one line (see the module doc). `Ok(None)` for a blank line or a
/// comment, `Err` with the reason when the address is not understood.
pub(super) fn parse_hook_line(line: &str) -> Result<Option<HookLine>, String> {
    let line = line.split('#').next().unwrap();
    let mut words = line.split_whitespace().peekable();
    let Some(first) = words.next() else {
        return Ok(None);
    };
    let digits = first.strip_prefix('@').unwrap_or(first);
    let digits = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
        .unwrap_or(digits);
    let word = u32::from_str_radix(digits, 16)
        .map_err(|_| format!("{first:?} is not an address in hex"))?;
    let addr = word & !1;
    let (mut arm, mut thumb) = if word & 1 == 1 {
        (false, true)
    } else {
        (true, true)
    };
    match words.peek().copied() {
        Some("arm") if word & 1 == 1 => {
            return Err(format!("{first} is odd (Thumb) but the line says arm"));
        }
        Some("arm") => {
            words.next();
            (arm, thumb) = (true, false);
        }
        Some("thumb") => {
            words.next();
            (arm, thumb) = (false, true);
        }
        _ => (),
    }
    let name = match words.peek() {
        Some(word) if !peek::looks_like_hook_peek(word) => words.next().unwrap().to_string(),
        _ => format!("{addr:#010x}"),
    };
    let mut parsed = HookLine {
        addr,
        arm,
        thumb,
        name,
        peeks: Vec::new(),
        bad_peeks: Vec::new(),
        unknown: Vec::new(),
    };
    for word in words {
        if peek::looks_like_hook_peek(word) {
            match peek::parse_hook_peek(word) {
                Ok(spec) => parsed.peeks.push(spec),
                Err(e) => parsed.bad_peeks.push((word.to_string(), e)),
            }
        } else {
            parsed.unknown.push(word.to_string());
        }
    }
    Ok(Some(parsed))
}

/// Which instruction sets a hook is for, as the header writes it.
pub(super) fn mode_name(arm: bool, thumb: bool) -> &'static str {
    match (arm, thumb) {
        (true, false) => "arm",
        (false, true) => "thumb",
        _ => "any",
    }
}

/// The header line of one hook: `# addr <name> <address> <mode>` and then
/// the names of the columns its rows have after `self`: `r2 r3 sp` and its
/// peeks.
pub(super) fn hook_header_line(line: &HookLine) -> String {
    let mut text = format!(
        "# addr {} {:#010x} {} r2 r3 sp",
        line.name,
        line.addr,
        mode_name(line.arm, line.thumb)
    );
    for spec in &line.peeks {
        text.push(' ');
        text.push_str(&spec.text);
    }
    text.push('\n');
    text
}

/// What a hook row is made from, see [hook_row].
pub(super) struct HookHit<'a> {
    pub(super) frame: u32,
    pub(super) ms: f64,
    pub(super) thread: usize,
    pub(super) name: &'a str,
    pub(super) thumb: bool,
    pub(super) regs: [u32; 16],
    pub(super) stack: [u32; 4],
    /// The peek columns, each with its leading tab.
    pub(super) peeks: &'a str,
}

/// One `addr` row: the 17 columns of a call row, then `r2`, `r3`, `sp` and
/// the peeks. The variant column is `arm` or `thumb` (the instruction set
/// it ran as), the class `-`, the selector the hook's name, `arg0` and
/// `arg1` are r0 and r1 (in hex and as floats), `sp0` to `sp3` the words on
/// the stack, `lr` the link register and `self` the hooked address.
pub(super) fn hook_row(hit: &HookHit, fmt_f32: fn(u32) -> String) -> String {
    let r = &hit.regs;
    format!(
        "{}\t{:.3}\t{}\taddr\t{}\t-\t{}\t{:#010x}\t{}\t{:#010x}\t{}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}\t{:#010x}{}\n",
        hit.frame,
        hit.ms,
        hit.thread,
        if hit.thumb { "thumb" } else { "arm" },
        hit.name,
        r[0],
        fmt_f32(r[0]),
        r[1],
        fmt_f32(r[1]),
        hit.stack[0],
        hit.stack[1],
        hit.stack[2],
        hit.stack[3],
        r[14],
        r[15],
        r[2],
        r[3],
        r[13],
        hit.peeks,
    )
}

#[cfg(test)]
mod tests {
    use super::super::peek::PeekBase;
    use super::*;

    #[test]
    fn lines_give_address_mode_name_and_peeks() {
        let line = parse_hook_line("0x0005bb24 thumb asbs r0:f32 sp.+4:u32 # comment")
            .unwrap()
            .unwrap();
        assert_eq!(line.addr, 0x5bb24);
        assert_eq!((line.arm, line.thumb), (false, true));
        assert_eq!(line.name, "asbs");
        assert_eq!(line.peeks.len(), 2);
        assert_eq!(line.peeks[1].base, PeekBase::Sp);

        // The decompile's spelling, an odd (Thumb) address, no name.
        let line = parse_hook_line("@000054ed r0:id").unwrap().unwrap();
        assert_eq!(line.addr, 0x54ec);
        assert_eq!((line.arm, line.thumb), (false, true));
        assert_eq!(line.name, "0x000054ec");
        assert_eq!(line.peeks[0].base, PeekBase::Reg(0));

        let line = parse_hook_line("2d24 arm rand_stub stray r0..x:u32 r9:u32")
            .unwrap()
            .unwrap();
        assert_eq!((line.addr, line.arm, line.thumb), (0x2d24, true, false));
        assert_eq!(line.name, "rand_stub");
        // r9 is not one of the bases, so not a peek at all.
        assert_eq!(
            line.unknown,
            vec!["stray".to_string(), "r9:u32".to_string()]
        );
        assert_eq!(line.bad_peeks.len(), 1);

        let line = parse_hook_line("0x1000 either").unwrap().unwrap();
        assert_eq!(
            (line.arm, line.thumb, line.name.as_str()),
            (true, true, "either")
        );

        assert_eq!(parse_hook_line("   # nothing"), Ok(None));
        assert!(parse_hook_line("main").is_err());
        assert!(parse_hook_line("0x1001 arm").is_err());
    }

    #[test]
    fn header_and_row_have_the_documented_columns() {
        let line = parse_hook_line("0x2d24 arm spawn r0.hp:f32")
            .unwrap()
            .unwrap();
        assert_eq!(
            hook_header_line(&line),
            "# addr spawn 0x00002d24 arm r2 r3 sp r0.hp:f32\n"
        );
        let mut regs = [0u32; 16];
        regs[0] = 0x3f80_0000;
        regs[1] = 2;
        regs[2] = 3;
        regs[3] = 4;
        regs[13] = 0x00ff_f000;
        regs[14] = 0x0000_4001;
        regs[15] = 0x2d24;
        let hit = HookHit {
            frame: 5,
            ms: 12.5,
            thread: 0,
            name: "spawn",
            thumb: false,
            regs,
            stack: [9, 8, 7, 6],
            peeks: "\t25",
        };
        let row = hook_row(&hit, |w| format!("{}", f32::from_bits(w)));
        assert_eq!(
            row,
            "5\t12.500\t0\taddr\tarm\t-\tspawn\t0x3f800000\t1\t0x00000002\t0.000000000000000000000000000000000000000000003\t0x00000009\t0x00000008\t0x00000007\t0x00000006\t0x00004001\t0x00002d24\t0x00000003\t0x00000004\t0x00fff000\t25\n"
        );
        assert_eq!(row.trim_end().split('\t').count(), 17 + 3 + 1);
    }
}
