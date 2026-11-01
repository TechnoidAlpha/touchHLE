/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Replaying random numbers from a file (`--rng-replay=`).
//!
//! The file lists values for the guest's draws (`rand`, `rand_r`, `random`
//! and `arc4random`), used in order, one per draw, whatever the function.
//! This module only parses the file and hands the values out; the draws are
//! in the parent module. It uses nothing from the rest of touchHLE, so its
//! tests can run on their own.

/// One value of the file, with the function it was drawn by, if the line
/// says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayValue {
    pub func: Option<String>,
    pub value: u32,
}

/// The functions whose results can be replayed.
pub const DRAW_FUNCTIONS: [&str; 4] = ["rand", "rand_r", "random", "arc4random"];

/// A number written in decimal, or in hex with `0x`. A decimal may be
/// negative (as a signed result is), and is then taken as its 32 bits.
fn parse_number(word: &str) -> Option<u32> {
    if let Some(hex) = word.strip_prefix("0x").or_else(|| word.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).ok();
    }
    if let Some(negative) = word.strip_prefix('-') {
        return negative
            .parse::<u32>()
            .ok()
            .filter(|&n| n <= 1 << 31)
            .map(|n| n.wrapping_neg());
    }
    word.parse().ok()
}

/// One line of a replay file. Returns `Ok(None)` for a blank line or a
/// comment (`#` starts one), and `Err` with the reason for a line that is
/// not understood. Three forms are read:
///
/// - a value alone: `12345` or `0x3039`;
/// - a function and a value: `arc4random 0x3039`;
/// - a `trace.log` row with origin `rng` (tab-separated, the function in
///   the selector column and the value in `arg0`), so the `rng` rows of a
///   trace can be fed back as they are. Its seed rows (variant `seed`) are
///   skipped, as they are not draws.
pub fn parse_replay_line(line: &str) -> Result<Option<ReplayValue>, String> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim_start().starts_with('#') || line.trim().is_empty() {
        return Ok(None);
    }
    let columns: Vec<&str> = line.split('\t').collect();
    if columns.len() >= 8 && columns[3] == "rng" {
        if columns[4] == "seed" {
            return Ok(None);
        }
        let value = parse_number(columns[7])
            .ok_or_else(|| format!("the arg0 column {:?} is not a number", columns[7]))?;
        return Ok(Some(ReplayValue {
            func: Some(columns[6].to_string()),
            value,
        }));
    }
    let line = line.split('#').next().unwrap();
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        [] => Ok(None),
        [value] => parse_number(value)
            .map(|value| Some(ReplayValue { func: None, value }))
            .ok_or_else(|| format!("{value:?} is not a number")),
        [func, value] => {
            if !DRAW_FUNCTIONS.contains(func) {
                return Err(format!(
                    "{func:?} is not one of {}",
                    DRAW_FUNCTIONS.join(", ")
                ));
            }
            parse_number(value)
                .map(|value| {
                    Some(ReplayValue {
                        func: Some(func.to_string()),
                        value,
                    })
                })
                .ok_or_else(|| format!("{value:?} is not a number"))
        }
        _ => Err("too many words".to_string()),
    }
}

/// What [Replay::next] has to report, once each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayNote {
    /// The value at this index (from 0) was written for another function
    /// than the one that drew it; it was used anyway.
    OtherFunction {
        index: usize,
        written_for: String,
        drawn_by: String,
    },
    /// All the values have been used; this draw and the later ones get the
    /// generator's own values.
    UsedUp { count: usize, drawn_by: String },
}

/// The values of a replay file, handed out in order.
#[derive(Clone, Debug, Default)]
pub struct Replay {
    values: Vec<ReplayValue>,
    next: usize,
    mismatch_noted: bool,
    used_up_noted: bool,
}

impl Replay {
    /// Parse a whole file. Lines that are not understood are skipped and
    /// returned with their line number (from 1) and the reason.
    pub fn parse(text: &str) -> (Replay, Vec<(usize, String)>) {
        let mut values = Vec::new();
        let mut bad = Vec::new();
        for (n, line) in text.lines().enumerate() {
            match parse_replay_line(line) {
                Ok(Some(value)) => values.push(value),
                Ok(None) => (),
                Err(why) => bad.push((n + 1, why)),
            }
        }
        (
            Replay {
                values,
                ..Replay::default()
            },
            bad,
        )
    }

    /// How many values the file has.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// The value for the next draw, made by `func`, or [None] when the
    /// values are used up. The second item is something to log, at most
    /// once per kind of note.
    pub fn next(&mut self, func: &str) -> (Option<u32>, Option<ReplayNote>) {
        let Some(entry) = self.values.get(self.next) else {
            let note =
                (!std::mem::replace(&mut self.used_up_noted, true)).then(|| ReplayNote::UsedUp {
                    count: self.values.len(),
                    drawn_by: func.to_string(),
                });
            return (None, note);
        };
        let index = self.next;
        self.next += 1;
        let mut note = None;
        if let Some(written_for) = &entry.func {
            if written_for != func && !std::mem::replace(&mut self.mismatch_noted, true) {
                note = Some(ReplayNote::OtherFunction {
                    index,
                    written_for: written_for.clone(),
                    drawn_by: func.to_string(),
                });
            }
        }
        (Some(entry.value), note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_alone_and_with_a_function() {
        assert_eq!(
            parse_replay_line("12345"),
            Ok(Some(ReplayValue {
                func: None,
                value: 12345
            }))
        );
        assert_eq!(
            parse_replay_line("  0x3039  # a comment"),
            Ok(Some(ReplayValue {
                func: None,
                value: 0x3039
            }))
        );
        assert_eq!(
            parse_replay_line("arc4random 0xFFFFFFFF"),
            Ok(Some(ReplayValue {
                func: Some("arc4random".to_string()),
                value: u32::MAX
            }))
        );
        assert_eq!(
            parse_replay_line("rand -1"),
            Ok(Some(ReplayValue {
                func: Some("rand".to_string()),
                value: u32::MAX
            }))
        );
        assert_eq!(parse_replay_line("# only a comment"), Ok(None));
        assert_eq!(parse_replay_line("   "), Ok(None));
        assert!(parse_replay_line("srand 5").is_err());
        assert!(parse_replay_line("rand five").is_err());
        assert!(parse_replay_line("rand 1 2").is_err());
        assert!(parse_replay_line("0x1_0000_0000").is_err());
    }

    #[test]
    fn trace_rows_are_read_back() {
        let row = "12\t3456.789\t0\trng\tgen\t-\tarc4random\t0x0000abcd\t43981\t\
                   0x0000abcd\t-\t-\t-\t-\t-\t0x0000a1b3\t-";
        assert_eq!(
            parse_replay_line(row),
            Ok(Some(ReplayValue {
                func: Some("arc4random".to_string()),
                value: 0xabcd
            }))
        );
        let seed = "0\t1.000\t0\trng\tseed\t-\tsrandom\t0x00000007\t7\t\
                    -\t-\t-\t-\t-\t-\t0x0000a1b3\t-";
        assert_eq!(parse_replay_line(seed), Ok(None));
    }

    #[test]
    fn values_are_handed_out_in_order_then_used_up() {
        let (mut replay, bad) = Replay::parse("1\nrandom 2\nbad words here\n\n3\n");
        assert_eq!(bad, vec![(3, "too many words".to_string())]);
        assert_eq!(replay.len(), 3);
        assert_eq!(replay.next("rand"), (Some(1), None));
        assert_eq!(
            replay.next("arc4random"),
            (
                Some(2),
                Some(ReplayNote::OtherFunction {
                    index: 1,
                    written_for: "random".to_string(),
                    drawn_by: "arc4random".to_string()
                })
            )
        );
        assert_eq!(replay.next("random"), (Some(3), None));
        assert_eq!(
            replay.next("rand"),
            (
                None,
                Some(ReplayNote::UsedUp {
                    count: 3,
                    drawn_by: "rand".to_string()
                })
            )
        );
        // Each note once.
        assert_eq!(replay.next("rand"), (None, None));
    }
}
