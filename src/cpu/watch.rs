/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Watchpoint bookkeeping for the CPU (see the parent module): which
//! address ranges are watched, and the writes made to them that have not
//! been logged yet. The memory itself is read and written by the parent
//! module; this part uses nothing from the rest of touchHLE, so its tests
//! can run on their own.

/// A write to a watched range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchHit {
    /// Which watch (as [Watches::add] numbered it).
    pub watch: usize,
    /// Where the write went and how many bytes it wrote. It may start
    /// before the watched range or end after it.
    pub addr: u32,
    pub size: u32,
    /// The watched range's bytes before and after the write, little-endian.
    pub old: u64,
    pub new: u64,
    /// The address of the writing instruction, once known: a write found
    /// while running is given it when the CPU has halted on it, one found
    /// while stepping past that instruction has it at once.
    pub pc: Option<u32>,
    /// Whether it was a store-exclusive (`strex`), which does not halt the
    /// CPU, so its instruction is not known.
    pub exclusive: bool,
}

/// The instruction being stepped past after a halt on a watched write.
#[derive(Debug)]
struct Step {
    pc: u32,
    /// The writes (address and size) already logged, which the instruction
    /// makes again when it runs again; each is skipped once.
    logged: Vec<(u32, u32)>,
}

/// The watched ranges and the writes to them not yet taken.
#[derive(Debug, Default)]
pub struct Watches {
    /// Start and length per watch; a removed watch has length 0.
    ranges: Vec<(u32, u32)>,
    hits: Vec<WatchHit>,
    step: Option<Step>,
}

/// Whether `[a, a + a_len)` and `[b, b + b_len)` share a byte.
fn overlaps(a: u32, a_len: u32, b: u32, b_len: u32) -> bool {
    let (a, a_end) = (u64::from(a), u64::from(a) + u64::from(a_len));
    let (b, b_end) = (u64::from(b), u64::from(b) + u64::from(b_len));
    a < b_end && b < a_end
}

/// The 4 KiB pages (by number) that `[start, start + len)` touches.
pub fn pages_of(start: u32, len: u32) -> std::ops::RangeInclusive<u32> {
    let last = u64::from(start) + u64::from(len.max(1)) - 1;
    let last = last.min(u64::from(u32::MAX)) as u32;
    (start >> 12)..=(last >> 12)
}

impl Watches {
    /// Watch `len` bytes (at most 8) at `start`. Returns the watch's number.
    pub fn add(&mut self, start: u32, len: u32) -> usize {
        assert!((1..=8).contains(&len));
        self.ranges.push((start, len));
        self.ranges.len() - 1
    }

    /// Stop watching `watch`. Returns its range, or [None] if it was not
    /// watched.
    pub fn remove(&mut self, watch: usize) -> Option<(u32, u32)> {
        let range = self.ranges.get_mut(watch)?;
        if range.1 == 0 {
            return None;
        }
        Some(std::mem::take(range))
    }

    /// The watches a write of `size` bytes at `addr` touches, each with its
    /// start and length.
    pub fn touched(&self, addr: u32, size: u32) -> Vec<(usize, u32, u32)> {
        self.ranges
            .iter()
            .enumerate()
            .filter(|&(_, &(start, len))| len != 0 && overlaps(addr, size, start, len))
            .map(|(watch, &(start, len))| (watch, start, len))
            .collect()
    }

    /// Record a write of `size` bytes at `addr` that touched the watches in
    /// `touched` (each with its range's old and new bytes). Returns whether
    /// the CPU should halt so the writing instruction can be found:
    /// - while stepping past an instruction (see [Watches::begin_step]), a
    ///   write it already made before the halt is skipped once, and any
    ///   other is recorded with that instruction's address; no halt;
    /// - a store-exclusive (`may_halt` false) is recorded without one;
    /// - otherwise the write is recorded and the CPU halts.
    pub fn record(
        &mut self,
        addr: u32,
        size: u32,
        touched: &[(usize, u64, u64)],
        may_halt: bool,
    ) -> bool {
        if touched.is_empty() {
            return false;
        }
        let mut pc = None;
        if let Some(step) = &mut self.step {
            if let Some(i) = step.logged.iter().position(|&w| w == (addr, size)) {
                step.logged.remove(i);
                return false;
            }
            pc = Some(step.pc);
        }
        for &(watch, old, new) in touched {
            self.hits.push(WatchHit {
                watch,
                addr,
                size,
                old,
                new,
                pc,
                exclusive: !may_halt,
            });
        }
        pc.is_none() && may_halt
    }

    /// The writes recorded since the last call.
    pub fn take_hits(&mut self) -> Vec<WatchHit> {
        std::mem::take(&mut self.hits)
    }

    /// Start stepping past the instruction at `pc` after the CPU halted on
    /// its write; `logged` are the writes already recorded for it.
    pub fn begin_step(&mut self, pc: u32, logged: Vec<(u32, u32)>) {
        self.step = Some(Step { pc, logged });
    }

    /// The instruction has run.
    pub fn end_step(&mut self) {
        self.step = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_touch_the_ranges_they_overlap() {
        let mut watches = Watches::default();
        let gold = watches.add(0x1000, 4);
        let flag = watches.add(0x1004, 1);
        assert_eq!(watches.touched(0x1000, 4), vec![(gold, 0x1000, 4)]);
        assert_eq!(watches.touched(0x0ffc, 4), vec![]);
        assert_eq!(watches.touched(0x0ffd, 4), vec![(gold, 0x1000, 4)]);
        assert_eq!(
            watches.touched(0x1000, 8),
            vec![(gold, 0x1000, 4), (flag, 0x1004, 1)]
        );
        assert_eq!(watches.touched(0x1005, 2), vec![]);
        assert_eq!(watches.remove(gold), Some((0x1000, 4)));
        assert_eq!(watches.remove(gold), None);
        assert_eq!(watches.touched(0x1000, 8), vec![(flag, 0x1004, 1)]);
        // Near the top of the address space nothing wraps.
        let top = watches.add(0xffff_fffc, 4);
        assert_eq!(watches.touched(0xffff_fffe, 2), vec![(top, 0xffff_fffc, 4)]);
        assert_eq!(watches.touched(0, 4), vec![]);
    }

    #[test]
    fn pages_cover_the_range() {
        assert_eq!(pages_of(0x1ffc, 4), 1..=1);
        assert_eq!(pages_of(0x1ffe, 4), 1..=2);
        assert_eq!(pages_of(0xffff_fffc, 8), 0xfffff..=0xfffff);
    }

    #[test]
    fn a_write_halts_then_the_step_skips_it_once() {
        let mut watches = Watches::default();
        let a = watches.add(0x2000, 4);
        let b = watches.add(0x2004, 4);
        // An stm writing both words: the first halts.
        assert!(watches.record(0x2000, 4, &[(a, 1, 2)], true));
        let hits = watches.take_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].pc, None);
        assert!(!hits[0].exclusive);
        // Stepping past it: the first word again is skipped, the second is
        // recorded with the instruction's address, and neither halts.
        watches.begin_step(0x8000, vec![(0x2000, 4)]);
        assert!(!watches.record(0x2000, 4, &[(a, 2, 2)], true));
        assert!(!watches.record(0x2004, 4, &[(b, 0, 7)], true));
        watches.end_step();
        assert_eq!(
            watches.take_hits(),
            vec![WatchHit {
                watch: b,
                addr: 0x2004,
                size: 4,
                old: 0,
                new: 7,
                pc: Some(0x8000),
                exclusive: false,
            }]
        );
        // After the step a write halts again.
        assert!(watches.record(0x2000, 4, &[(a, 2, 3)], true));
    }

    #[test]
    fn exclusive_writes_do_not_halt() {
        let mut watches = Watches::default();
        let a = watches.add(0x2000, 4);
        assert!(!watches.record(0x2000, 4, &[(a, 1, 2)], false));
        let hits = watches.take_hits();
        assert!(hits[0].exclusive);
        assert_eq!(hits[0].pc, None);
        assert!(!watches.record(0x3000, 4, &[], true));
    }
}
