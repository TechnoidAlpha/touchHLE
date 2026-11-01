/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Coverage for the message trace (`--coverage`): every instruction of the
//! app binary the JIT translated, written as address ranges to
//! `coverage.txt`. The CPU gathers the addresses (src/cpu.rs); this part
//! merges them into ranges and writes the file.

/// File in the user data directory the coverage is written to.
pub(super) const COVERAGE_FILE: &str = "coverage.txt";

/// A run of instructions of one instruction set, each starting where the
/// one before ended: `start` up to `end` (not included).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CoverageRange {
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) thumb: bool,
}

/// The size of the Thumb instruction whose first halfword is `halfword`:
/// 4 for a 32-bit encoding (top five bits `0b11101`, `0b11110` or
/// `0b11111`), otherwise 2.
pub(super) fn thumb_size(halfword: u16) -> u32 {
    if halfword >> 11 >= 0b11101 {
        4
    } else {
        2
    }
}

/// Merge instructions (address, size in bytes, Thumb or not), in address
/// order, into ranges: an instruction that starts where the last one of the
/// same instruction set ended extends its range.
pub(super) fn merge(
    instructions: impl IntoIterator<Item = (u32, u32, bool)>,
) -> Vec<CoverageRange> {
    let mut ranges: Vec<CoverageRange> = Vec::new();
    for (addr, size, thumb) in instructions {
        let end = addr.saturating_add(size);
        match ranges.last_mut() {
            Some(last) if last.thumb == thumb && last.end == addr => last.end = end,
            Some(last) if last.thumb == thumb && last.start <= addr && addr < last.end => {
                last.end = last.end.max(end);
            }
            _ => ranges.push(CoverageRange {
                start: addr,
                end,
                thumb,
            }),
        }
    }
    ranges
}

/// The text of `coverage.txt`: the `header` lines (each starting `# `),
/// then the column names, then a line per range with the section it is in.
pub(super) fn coverage_text(header: &[String], ranges: &[(CoverageRange, &str)]) -> String {
    let mut text = String::new();
    for line in header {
        text.push_str("# ");
        text.push_str(line);
        text.push('\n');
    }
    text.push_str("start\tend\tset\tsection\n");
    for (range, section) in ranges {
        text.push_str(&format!(
            "{:#010x}\t{:#010x}\t{}\t{}\n",
            range.start,
            range.end,
            if range.thumb { "thumb" } else { "arm" },
            section
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_sizes_follow_the_first_halfword() {
        assert_eq!(thumb_size(0xb580), 2); // push {r7, lr}
        assert_eq!(thumb_size(0xe7fe), 2); // b .
        assert_eq!(thumb_size(0xe92d), 4); // push.w
        assert_eq!(thumb_size(0xf000), 4); // bl (first half)
        assert_eq!(thumb_size(0xf8d0), 4); // ldr.w
    }

    #[test]
    fn adjacent_instructions_merge() {
        let ranges = merge([
            (0x1000, 2, true),
            (0x1002, 4, true),
            (0x1006, 2, true),
            // A gap (a literal pool, say) starts a new range.
            (0x100c, 2, true),
            // Arm code right after Thumb code is a range of its own.
            (0x100e, 4, false),
            (0x1012, 4, false),
        ]);
        assert_eq!(
            ranges,
            vec![
                CoverageRange {
                    start: 0x1000,
                    end: 0x1008,
                    thumb: true
                },
                CoverageRange {
                    start: 0x100c,
                    end: 0x100e,
                    thumb: true
                },
                CoverageRange {
                    start: 0x100e,
                    end: 0x1016,
                    thumb: false
                },
            ]
        );
        assert_eq!(merge([]), vec![]);
    }

    #[test]
    fn the_file_has_a_header_columns_and_a_line_per_range() {
        let text = coverage_text(
            &["touchHLE coverage".to_string(), "ranges=1".to_string()],
            &[(
                CoverageRange {
                    start: 0x2d24,
                    end: 0x2d6a,
                    thumb: true,
                },
                "__text",
            )],
        );
        assert_eq!(
            text,
            "# touchHLE coverage\n# ranges=1\nstart\tend\tset\tsection\n0x00002d24\t0x00002d6a\tthumb\t__text\n"
        );
    }
}
