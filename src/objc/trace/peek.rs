/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Peek specs for the message trace: a field of the receiver or of a
//! pointer argument, read from guest memory when a traced call is logged and
//! written as an extra column (see the parent module's doc). This part only
//! parses specs and formats values; the reading is in the parent module.
//!
//! The grammar, one word with no spaces:
//!
//! ```text
//! <base>(.<ivar> | .+<offset> | ->(<ivar> | +<offset>))* :<type>
//! ```
//!
//! - `<base>` is `self` (the receiver), `arg0` or `arg1` (the words in the
//!   trace's `arg0` and `arg1` columns: r2 and r3 for an ordinary send).
//! - `.<ivar>` is the instance variable of that name, found on the class
//!   chain of the object the pointer so far points at. It may only follow
//!   the base or `->`: a field is not an object.
//! - `.+<offset>` moves that many bytes further on (`0x` for hex, or
//!   decimal), from the object or from the field so far, so
//!   `self.sprPoint.+4:f32` is the y of a `CGPoint` ivar.
//! - `->` reads the 4-byte pointer at the address so far and carries on in
//!   the object it points at; it must be followed by an ivar or an offset.
//! - `<type>` is `i32`, `u32`, `f32`, `u8`, `ptr` (hex), `id` (hex and the
//!   class name from the object's isa, or `nil`) or `str` (the text of an
//!   `NSString`, escaped and cut, see [fmt_str]; `nil`, or `-` for an object
//!   that is not one of touchHLE's own strings).
//!
//! With no step at all (`arg0:id`), the base word itself is the value.

/// Where a peek starts.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum PeekBase {
    /// `self`: the receiver.
    Receiver,
    /// `arg0`: the word in the trace's `arg0` column.
    Arg0,
    /// `arg1`: the word in the trace's `arg1` column.
    Arg1,
}

/// One step of a peek's path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PeekStep {
    /// `.name` (or the `name` of `->name`): the ivar `name` of the object
    /// the pointer so far points at.
    Ivar(String),
    /// `.+n` (or the `+n` of `->+n`): `n` bytes further on.
    Bytes(u32),
    /// `->`: read the pointer at the address so far and go to that object.
    Deref,
}

/// The type a peek's value is read and written as.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum PeekType {
    I32,
    U32,
    F32,
    U8,
    Ptr,
    Id,
    /// An `NSString`'s text, see [fmt_str].
    Str,
}

impl PeekType {
    /// How many bytes are read for the value.
    pub(super) fn size(self) -> u32 {
        match self {
            PeekType::U8 => 1,
            _ => 4,
        }
    }
}

/// A parsed peek spec, see the module doc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PeekSpec {
    /// The spec as written, for the header and the log.
    pub(super) text: String,
    pub(super) base: PeekBase,
    pub(super) steps: Vec<PeekStep>,
    pub(super) ty: PeekType,
}

/// Whether a word of a `trace_selectors.txt` line is meant as a peek: it
/// starts with one of the bases.
pub(super) fn looks_like_peek(word: &str) -> bool {
    ["self", "arg0", "arg1"].iter().any(|b| word.starts_with(b))
}

/// Parse one peek spec (see the module doc), or say what is wrong with it.
pub(super) fn parse_peek(text: &str) -> Result<PeekSpec, String> {
    let (path, ty) = text
        .rsplit_once(':')
        .ok_or_else(|| "no :type at the end".to_string())?;
    let ty = match ty {
        "i32" => PeekType::I32,
        "u32" => PeekType::U32,
        "f32" => PeekType::F32,
        "u8" => PeekType::U8,
        "ptr" => PeekType::Ptr,
        "id" => PeekType::Id,
        "str" => PeekType::Str,
        _ => {
            return Err(format!(
                "unknown type {ty:?} (i32, u32, f32, u8, ptr, id or str)"
            ))
        }
    };
    let bases = [
        ("self", PeekBase::Receiver),
        ("arg0", PeekBase::Arg0),
        ("arg1", PeekBase::Arg1),
    ];
    let (base, mut rest) = bases
        .iter()
        .find_map(|&(name, base)| {
            let rest = path.strip_prefix(name)?;
            (rest.is_empty() || rest.starts_with('.') || rest.starts_with("->"))
                .then_some((base, rest))
        })
        .ok_or_else(|| "it must start with self, arg0 or arg1".to_string())?;

    let mut steps = Vec::new();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("->") {
            steps.push(PeekStep::Deref);
            rest = after;
        } else if let Some(after) = rest.strip_prefix('.') {
            rest = after;
        } else {
            return Err(format!("expected . or -> at {rest:?}"));
        }
        let on_object = matches!(steps.last(), None | Some(PeekStep::Deref));
        if let Some(after) = rest.strip_prefix('+') {
            let len = after
                .find(|c: char| !c.is_ascii_alphanumeric())
                .unwrap_or(after.len());
            let (number, after) = after.split_at(len);
            let bytes = match number.strip_prefix("0x") {
                Some(hex) => u32::from_str_radix(hex, 16),
                None => number.parse(),
            }
            .map_err(|_| format!("bad offset +{number:?}"))?;
            steps.push(PeekStep::Bytes(bytes));
            rest = after;
        } else {
            let len = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let (name, after) = rest.split_at(len);
            if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
                return Err(format!("expected an ivar name or +offset at {rest:?}"));
            }
            if !on_object {
                return Err(format!(
                    "ivar {name} must follow the base or -> (after a field, . only takes +offset)"
                ));
            }
            steps.push(PeekStep::Ivar(name.to_string()));
            rest = after;
        }
    }
    Ok(PeekSpec {
        text: text.to_string(),
        base,
        steps,
        ty,
    })
}

/// A float with 6 significant digits, by C's `%g` rules (plain notation for
/// exponents -4 to 5, trailing zeros dropped), but with the exponent written
/// the way Rust and Python read it (`1.5e-7`, `1e6`).
pub(super) fn fmt_f32_6(value: f32) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    fn trim_zeros(s: &str) -> &str {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.')
        } else {
            s
        }
    }
    // Rounded to 6 significant digits first, so the exponent is the one of
    // the rounded value (999999.5 becomes 1e6).
    let scientific = format!("{value:.5e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap();
    let exponent: i32 = exponent.parse().unwrap();
    if (-4..6).contains(&exponent) {
        let decimals = (5 - exponent) as usize;
        trim_zeros(&format!("{value:.decimals$}")).to_string()
    } else {
        format!("{}e{exponent}", trim_zeros(mantissa))
    }
}

/// How many bytes of escaped text a `str` peek writes at most, see
/// [fmt_str].
pub(super) const STR_MAX_BYTES: usize = 200;

/// A string's text as a `str` peek writes it, on one line and in one
/// column: each tab, newline, carriage return and backslash is written as
/// `\t`, `\n`, `\r` and `\\`. If the escaped text is longer than
/// [STR_MAX_BYTES] bytes, it is cut before the first character (or escape)
/// that would go past that and `...` is added, so the column holds at most
/// 203 bytes and is always whole UTF-8 characters.
pub(super) fn fmt_str(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let escape = match c {
            '\t' => Some("\\t"),
            '\n' => Some("\\n"),
            '\r' => Some("\\r"),
            '\\' => Some("\\\\"),
            _ => None,
        };
        let len = escape.map_or(c.len_utf8(), str::len);
        if out.len() + len > STR_MAX_BYTES {
            out.push_str("...");
            break;
        }
        match escape {
            Some(escape) => out.push_str(escape),
            None => out.push(c),
        }
    }
    out
}

/// A peek's value as written in its column. `word` holds the value (for
/// `u8`, the byte). `detail` is, for an `id`, the class name of the object
/// it points at and, for a `str`, the string's text, when it could be read.
pub(super) fn fmt_peek_value(ty: PeekType, word: u32, detail: Option<&str>) -> String {
    match ty {
        PeekType::I32 => (word as i32).to_string(),
        PeekType::U32 => word.to_string(),
        PeekType::U8 => (word & 0xff).to_string(),
        PeekType::F32 => fmt_f32_6(f32::from_bits(word)),
        PeekType::Ptr => format!("{word:#010x}"),
        PeekType::Id if word == 0 => "nil".to_string(),
        PeekType::Id => format!("{word:#010x}:{}", detail.unwrap_or("?")),
        PeekType::Str if word == 0 => "nil".to_string(),
        PeekType::Str => detail.map_or_else(|| "-".to_string(), fmt_str),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        fmt_f32_6, fmt_peek_value, fmt_str, looks_like_peek, parse_peek, PeekBase, PeekStep,
        PeekType, STR_MAX_BYTES,
    };

    fn ivar(name: &str) -> PeekStep {
        PeekStep::Ivar(name.to_string())
    }

    #[test]
    fn every_grammar_form_parses() {
        let cases: &[(&str, PeekBase, Vec<PeekStep>, PeekType)] = &[
            (
                "self.hp:f32",
                PeekBase::Receiver,
                vec![ivar("hp")],
                PeekType::F32,
            ),
            (
                "arg0.+0x40:u32",
                PeekBase::Arg0,
                vec![PeekStep::Bytes(0x40)],
                PeekType::U32,
            ),
            (
                "self.+16:u8",
                PeekBase::Receiver,
                vec![PeekStep::Bytes(16)],
                PeekType::U8,
            ),
            (
                "self.sharedData->waveNum:i32",
                PeekBase::Receiver,
                vec![ivar("sharedData"), PeekStep::Deref, ivar("waveNum")],
                PeekType::I32,
            ),
            (
                "arg1->+0x8:ptr",
                PeekBase::Arg1,
                vec![PeekStep::Deref, PeekStep::Bytes(8)],
                PeekType::Ptr,
            ),
            (
                "self.sprPoint.+0x4:f32",
                PeekBase::Receiver,
                vec![ivar("sprPoint"), PeekStep::Bytes(4)],
                PeekType::F32,
            ),
            (
                "self.+0xec->+0x4.+0x4:i32",
                PeekBase::Receiver,
                vec![
                    PeekStep::Bytes(0xec),
                    PeekStep::Deref,
                    PeekStep::Bytes(4),
                    PeekStep::Bytes(4),
                ],
                PeekType::I32,
            ),
            (
                "arg0.target_2->hp:f32",
                PeekBase::Arg0,
                vec![ivar("target_2"), PeekStep::Deref, ivar("hp")],
                PeekType::F32,
            ),
            ("arg0:id", PeekBase::Arg0, vec![], PeekType::Id),
            ("arg0:str", PeekBase::Arg0, vec![], PeekType::Str),
            (
                "self.string_->+0x4:str",
                PeekBase::Receiver,
                vec![ivar("string_"), PeekStep::Deref, PeekStep::Bytes(4)],
                PeekType::Str,
            ),
        ];
        for (text, base, steps, ty) in cases {
            let spec = parse_peek(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(spec.text, *text);
            assert_eq!(spec.base, *base, "{text}");
            assert_eq!(&spec.steps, steps, "{text}");
            assert_eq!(spec.ty, *ty, "{text}");
        }
    }

    #[test]
    fn malformed_specs_are_rejected_with_a_reason() {
        let cases = [
            ("self.hp", "no :type"),
            ("self.hp:f64", "unknown type"),
            ("arg0:string", "unknown type"),
            ("arg0:STR", "unknown type"),
            ("self.hp:", "unknown type"),
            ("this.hp:f32", "must start with"),
            ("selfish.hp:f32", "must start with"),
            ("self.:f32", "expected an ivar name"),
            ("self..hp:f32", "expected an ivar name"),
            ("self.2nd:f32", "expected an ivar name"),
            ("self->:i32", "expected an ivar name"),
            ("self.hp->:i32", "expected an ivar name"),
            ("self.a.b:f32", "must follow the base or ->"),
            ("self.+0x4.hp:f32", "must follow the base or ->"),
            ("self.+0xZZ:u32", "bad offset"),
            ("self.+:u32", "bad offset"),
            ("self.+99999999999:u32", "bad offset"),
            ("self-hp:i32", "must start with"),
            ("self.hp-x:i32", "expected . or ->"),
        ];
        for (text, reason) in cases {
            match parse_peek(text) {
                Ok(spec) => panic!("{text} parsed as {spec:?}"),
                Err(e) => assert!(e.contains(reason), "{text}: {e:?}, wanted {reason:?}"),
            }
        }
    }

    #[test]
    fn peek_words_are_told_from_markers() {
        assert!(looks_like_peek("self.hp:f32"));
        assert!(looks_like_peek("arg1->x:u8"));
        assert!(looks_like_peek("selfish"));
        assert!(!looks_like_peek("png"));
        assert!(!looks_like_peek("hp:f32"));
    }

    #[test]
    fn floats_have_six_significant_digits() {
        let cases: &[(f32, &str)] = &[
            (0.0, "0"),
            (-0.0, "-0"),
            (100.0, "100"),
            (30.0, "30"),
            (-2.5, "-2.5"),
            (0.1, "0.1"),
            (1.0 / 3.0, "0.333333"),
            (123456.7, "123457"),
            (999999.5, "1e6"),
            (1234567.0, "1.23457e6"),
            (0.0001, "0.0001"),
            (0.00001234, "1.234e-5"),
            (f32::INFINITY, "inf"),
            (f32::NEG_INFINITY, "-inf"),
            (f32::NAN, "nan"),
        ];
        for &(value, text) in cases {
            assert_eq!(fmt_f32_6(value), text, "{value:?}");
        }
    }

    #[test]
    fn values_are_written_by_type() {
        let f = |ty, word| fmt_peek_value(ty, word, None);
        assert_eq!(f(PeekType::I32, 0xffff_ffff), "-1");
        assert_eq!(f(PeekType::I32, 42), "42");
        assert_eq!(f(PeekType::U32, 0xffff_ffff), "4294967295");
        assert_eq!(f(PeekType::U8, 0xff), "255");
        assert_eq!(f(PeekType::F32, 1.5f32.to_bits()), "1.5");
        assert_eq!(f(PeekType::F32, 250.0f32.to_bits()), "250");
        assert_eq!(f(PeekType::Ptr, 0x1234), "0x00001234");
        assert_eq!(f(PeekType::Ptr, 0), "0x00000000");
        assert_eq!(f(PeekType::Id, 0), "nil");
        assert_eq!(f(PeekType::Id, 0x1000), "0x00001000:?");
        assert_eq!(
            fmt_peek_value(PeekType::Id, 0x1000, Some("Creeps")),
            "0x00001000:Creeps"
        );
        assert_eq!(
            fmt_peek_value(PeekType::Id, 0x2000, Some("+SharedData")),
            "0x00002000:+SharedData"
        );
        assert_eq!(f(PeekType::Str, 0), "nil");
        assert_eq!(f(PeekType::Str, 0x1000), "-");
        assert_eq!(
            fmt_peek_value(PeekType::Str, 0x1000, Some("Wave 3\tof 20")),
            r"Wave 3\tof 20"
        );
        assert_eq!(fmt_peek_value(PeekType::Str, 0x1000, Some("")), "");
        assert_eq!(PeekType::U8.size(), 1);
        assert_eq!(PeekType::Id.size(), 4);
        assert_eq!(PeekType::Str.size(), 4);
    }

    #[test]
    fn strings_are_escaped_onto_one_line() {
        assert_eq!(fmt_str("Gold: 250"), "Gold: 250");
        assert_eq!(fmt_str("a\tb\nc\rd\\e"), r"a\tb\nc\rd\\e");
        assert_eq!(fmt_str("\r\n"), r"\r\n");
        // A backslash already followed by a letter stays two characters.
        assert_eq!(fmt_str(r"C:\new"), r"C:\\new");
        // Everything else, quotes and non-ASCII text included, is kept.
        assert_eq!(fmt_str("\"R\u{e9}\" 50%"), "\"R\u{e9}\" 50%");
        assert_eq!(fmt_str(""), "");
    }

    #[test]
    fn long_strings_are_cut_on_a_char_boundary() {
        let a = |n: usize| "a".repeat(n);
        // Exactly the limit: kept whole, no marker.
        assert_eq!(fmt_str(&a(STR_MAX_BYTES)), a(STR_MAX_BYTES));
        // One byte more: cut to the limit, then the marker.
        assert_eq!(
            fmt_str(&a(STR_MAX_BYTES + 1)),
            format!("{}...", a(STR_MAX_BYTES))
        );
        // A two-byte character that would straddle the limit is left out
        // whole, so the text stays valid UTF-8.
        let straddle = format!("{}\u{e9}", a(STR_MAX_BYTES - 1));
        assert_eq!(fmt_str(&straddle), format!("{}...", a(STR_MAX_BYTES - 1)));
        // An escape is never split either.
        let escaped = format!("{}\n", a(STR_MAX_BYTES - 1));
        assert_eq!(fmt_str(&escaped), format!("{}...", a(STR_MAX_BYTES - 1)));
        // Escapes count as the two bytes they are written as.
        let tabs = "\t".repeat(STR_MAX_BYTES);
        assert_eq!(
            fmt_str(&tabs),
            format!("{}...", r"\t".repeat(STR_MAX_BYTES / 2))
        );
        // A four-byte character is never split.
        let cut = fmt_str(&"\u{1f3f0}".repeat(60));
        assert_eq!(cut, format!("{}...", "\u{1f3f0}".repeat(50)));
        assert!(cut.len() <= STR_MAX_BYTES + 3);
    }
}
