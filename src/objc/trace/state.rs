/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! State stream for the message trace (`--state-every=N`): every Nth
//! presented frame, one JSON line per live object of the chosen classes
//! (`--state-classes=`, by default the game's), with every instance
//! variable by name, read through the Objective-C metadata of the app
//! binary, written to `state.jsonl` next to `trace.log`.
//!
//! Objects are found without calling the guest, from three places:
//! - the cocos2d scene: every node from the `CCDirector`'s `runningScene_`
//!   down, as for the scene dump;
//! - the app binary's data sections: every word that points at an
//!   instance of a chosen class (a singleton like the game's `SharedData`
//!   is held in a static variable);
//! - the objects found so far: each object-typed instance variable (also
//!   inside a struct or a C array) of a chosen object that points at
//!   another chosen object, or at an array (touchHLE's `NSArray`,
//!   cocos2d's `CCArray`) or a dictionary (touchHLE's) whose items are.
//!
//! A class is chosen by name, matched on the class chain, so a subclass of
//! a chosen class is chosen too.
//!
//! The instance variables come from each class's `ivar_list_t` in the
//! binary: name, type encoding and size, and the offset touchHLE settled
//! on (read through the offset pointer, so it is the one in use). The type
//! encoding gives the value's JSON shape (see [Enc]); one that cannot be
//! decoded, or whose size does not match the size the compiler wrote, is
//! written as hex bytes. A bitfield ivar has only a byte offset, shared
//! with the bitfields declared next to it, so its bits are counted from the
//! lowest: the first of a run at one offset starts at bit 0, each next one
//! where the one before ended.
//!
//! See dev-docs/4towers-branch.md, "State stream", for the file's schema.
//! The search runs over [StateMemory], so the tests can give it a made-up
//! memory and class table.

use super::scene::{self, ClassInfo, SceneMemory};
use crate::mem::Ptr;
use crate::objc::{nil, Class, ClassHostObject};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::rc::Rc;

/// File in the user data directory the stream is written to.
pub(super) const STATE_FILE: &str = "state.jsonl";

/// The `schema` of the header line; it changes if the format does.
pub(super) const SCHEMA: &str = "touchHLE-state-1";

/// The classes written when `--state-classes=` is not given: 4Towers'
/// game layer, tower layer, towers, creeps, bullets and shared data (the
/// names in its class dump).
pub(super) const DEFAULT_CLASSES: [&str; 7] = [
    "Game",
    "towersLayer",
    "towerGrid",
    "Tower",
    "Creeps",
    "Bullets",
    "SharedData",
];

/// At most this many objects are written per snapshot.
pub(super) const MAX_OBJECTS: usize = 5000;

/// At most this many elements of a C array, or bytes of a value written as
/// hex, are written per instance variable.
pub(super) const MAX_ELEMENTS: u32 = 1024;

/// At most this many notes are written per snapshot.
const MAX_NOTES: usize = 50;

/// How deep a type encoding may nest.
const MAX_TYPE_DEPTH: u32 = 16;

/// The JSON shape of a value, decoded from its type encoding.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Enc {
    /// `c C s S i I l L q Q B`: a number.
    Int { size: u32, signed: bool },
    /// `f`: a number, or `"nan"`, `"inf"`, `"-inf"`.
    F32,
    /// `d`: the same.
    F64,
    /// `@`: an object, written as its address in hex. The objects it
    /// points at are looked at for more chosen objects.
    Object,
    /// `^...`, `*`, `:`, `#`: another pointer, written as its address.
    Pointer,
    /// `{name="field"type...}`: a JSON object, by field name (the index
    /// when the encoding has no names).
    Struct(Vec<(String, Enc)>),
    /// `[Ntype]`: a JSON array.
    Array(u32, Box<Enc>),
    /// Hex bytes, in memory order: a union, or a type not decoded.
    Bytes { size: u32, align: u32 },
    /// A bitfield instance variable (`bN`): a number, `width` bits from
    /// bit `shift` of the `size` bytes at its offset (see [class_layout]).
    Bits { size: u32, shift: u32, width: u32 },
    /// A type whose size is not known: a bitfield, `?`, `v`, a struct
    /// whose fields are not given.
    Unknown,
}

/// A reader of one type encoding.
struct TypeParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl TypeParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    /// The text between the quotes at the current position.
    fn quoted(&mut self) -> Result<String, String> {
        self.i += 1;
        let start = self.i;
        while self.peek().ok_or("a quote is not closed")? != b'"' {
            self.i += 1;
        }
        let text = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
        self.i += 1;
        Ok(text)
    }

    fn number(&mut self) -> Result<u32, String> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i])
            .unwrap()
            .parse()
            .map_err(|_| "a number is missing or too big".to_string())
    }

    /// The fields of a struct or union up to `close`, each with its name
    /// (or its index).
    fn fields(&mut self, depth: u32, close: u8) -> Result<Vec<(String, Enc)>, String> {
        let named = self.peek() == Some(b'"');
        let mut fields = Vec::new();
        while self.peek().ok_or("a struct or union is not closed")? != close {
            let name = if named && self.peek() == Some(b'"') {
                self.quoted()?
            } else {
                fields.len().to_string()
            };
            fields.push((name, self.parse(depth + 1, named)?));
        }
        self.i += 1;
        Ok(fields)
    }

    /// The type at the current position. `named_fields`: it is a field of
    /// a struct whose fields have names.
    fn parse(&mut self, depth: u32, named_fields: bool) -> Result<Enc, String> {
        if depth > MAX_TYPE_DEPTH {
            return Err("nested too deep".to_string());
        }
        // Qualifiers (const, in, out and the like) change nothing here.
        while matches!(
            self.peek(),
            Some(b'r' | b'n' | b'N' | b'o' | b'O' | b'R' | b'V')
        ) {
            self.i += 1;
        }
        let c = self.peek().ok_or("it ends early")?;
        self.i += 1;
        let int = |size, signed| Enc::Int { size, signed };
        Ok(match c {
            b'c' => int(1, true),
            b'C' | b'B' => int(1, false),
            b's' => int(2, true),
            b'S' => int(2, false),
            b'i' | b'l' => int(4, true),
            b'I' | b'L' => int(4, false),
            b'q' => int(8, true),
            b'Q' => int(8, false),
            b'f' => Enc::F32,
            b'd' => Enc::F64,
            b'*' | b':' | b'#' => Enc::Pointer,
            b'v' | b'?' => Enc::Unknown,
            b'@' => {
                match self.peek() {
                    // A block.
                    Some(b'?') => self.i += 1,
                    Some(b'"') => {
                        let at = self.i;
                        self.quoted()?;
                        // In a struct whose fields have names, the quotes
                        // after `@` may hold the next field's name instead
                        // of a class name: they hold a class name only if
                        // another name or the end follows.
                        if named_fields && !matches!(self.peek(), Some(b'"' | b'}') | None) {
                            self.i = at;
                        }
                    }
                    _ => (),
                }
                Enc::Object
            }
            b'^' => {
                self.parse(depth + 1, false)?;
                Enc::Pointer
            }
            b'[' => {
                let count = self.number()?;
                let element = self.parse(depth + 1, false)?;
                if self.peek() != Some(b']') {
                    return Err("an array is not closed".to_string());
                }
                self.i += 1;
                Enc::Array(count, Box::new(element))
            }
            b'{' | b'(' => {
                let close = if c == b'{' { b'}' } else { b')' };
                while !matches!(
                    self.peek().ok_or("a name is not closed")?,
                    b'=' | b'}' | b')'
                ) {
                    self.i += 1;
                }
                if self.peek() != Some(b'=') {
                    // A struct named without its fields.
                    self.i += 1;
                    return Ok(Enc::Unknown);
                }
                self.i += 1;
                let fields = self.fields(depth, close)?;
                if c == b'{' {
                    Enc::Struct(fields)
                } else {
                    // A union: its size and alignment are its largest
                    // member's.
                    let mut size = 0;
                    let mut align = 1;
                    for (_, field) in &fields {
                        let Some((s, a)) = layout(field) else {
                            return Ok(Enc::Unknown);
                        };
                        size = size.max(s);
                        align = align.max(a);
                    }
                    Enc::Bytes {
                        size: align_up(size, align),
                        align,
                    }
                }
            }
            b'b' => {
                self.number()?;
                Enc::Unknown
            }
            b'j' => {
                self.parse(depth + 1, false)?;
                Enc::Unknown
            }
            other => return Err(format!("unknown type character {:?}", other as char)),
        })
    }
}

/// Decode the type encoding `encoding` (as an ivar has it).
pub(super) fn parse_type(encoding: &str) -> Result<Enc, String> {
    let mut parser = TypeParser {
        s: encoding.as_bytes(),
        i: 0,
    };
    let enc = parser.parse(0, false)?;
    if parser.i != parser.s.len() {
        return Err("more follows the type".to_string());
    }
    Ok(enc)
}

fn align_up(value: u32, align: u32) -> u32 {
    value.div_ceil(align) * align
}

/// The size and alignment of a value of type `enc` on iPhone OS (armv7:
/// 64-bit types are 4-byte aligned in structs), or [None] if not known.
pub(super) fn layout(enc: &Enc) -> Option<(u32, u32)> {
    Some(match enc {
        Enc::Int { size, .. } => (*size, (*size).min(4)),
        Enc::F32 | Enc::Object | Enc::Pointer => (4, 4),
        Enc::F64 => (8, 4),
        Enc::Struct(fields) => {
            let mut offset = 0u32;
            let mut align = 1;
            for (_, field) in fields {
                let (s, a) = layout(field)?;
                offset = align_up(offset, a).checked_add(s)?;
                align = align.max(a);
            }
            (align_up(offset, align), align)
        }
        Enc::Array(count, element) => {
            let (s, a) = layout(element)?;
            (s.checked_mul(*count)?, a)
        }
        Enc::Bytes { size, align } => (*size, *align),
        Enc::Bits { size, .. } => (*size, 1),
        Enc::Unknown => return None,
    })
}

/// `text` as a JSON string.
pub(super) fn json_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// An address as the stream writes it: a JSON string, `"0x0012abcd"`.
fn json_addr(addr: u32) -> String {
    format!("\"{addr:#010x}\"")
}

/// A float as a JSON number, or a string if it is not finite.
fn json_float(value: f64, text: String) -> String {
    if value.is_nan() {
        "\"nan\"".to_string()
    } else if value.is_infinite() {
        if value > 0.0 { "\"inf\"" } else { "\"-inf\"" }.to_string()
    } else {
        text
    }
}

/// What the writing of one object's instance variables found.
#[derive(Default)]
struct Found {
    /// The non-nil object pointers, each with its path (`towers[3][5]`).
    objects: Vec<(String, u32)>,
    /// Whether a C array or hex value was cut at [MAX_ELEMENTS].
    cut: bool,
}

/// Write the JSON of the value of type `enc` at `addr` into `out`, and add
/// each non-nil object pointer in it to `found`, with its path below
/// `path`. A value that cannot be read is `null`.
fn write_value<M: SceneMemory + ?Sized>(
    m: &M,
    addr: u32,
    enc: &Enc,
    path: &str,
    out: &mut String,
    found: &mut Found,
) {
    let read = |size: u32| -> Option<u64> {
        let bytes = m.bytes(addr, size).ok()?;
        Some(
            bytes
                .iter()
                .rev()
                .fold(0u64, |acc, &b| (acc << 8) | u64::from(b)),
        )
    };
    match enc {
        Enc::Int { size, signed } => match read(*size) {
            Some(raw) if *signed => {
                let shift = 64 - 8 * size;
                out.push_str(&(((raw << shift) as i64) >> shift).to_string());
            }
            Some(raw) => out.push_str(&raw.to_string()),
            None => out.push_str("null"),
        },
        Enc::F32 => match read(4) {
            Some(raw) => {
                let value = f32::from_bits(raw as u32);
                out.push_str(&json_float(f64::from(value), value.to_string()));
            }
            None => out.push_str("null"),
        },
        Enc::F64 => match read(8) {
            Some(raw) => {
                let value = f64::from_bits(raw);
                out.push_str(&json_float(value, value.to_string()));
            }
            None => out.push_str("null"),
        },
        Enc::Object | Enc::Pointer => match read(4) {
            Some(raw) => {
                let value = raw as u32;
                out.push_str(&json_addr(value));
                if matches!(enc, Enc::Object) && value != 0 {
                    found.objects.push((path.to_string(), value));
                }
            }
            None => out.push_str("null"),
        },
        Enc::Struct(fields) => {
            out.push('{');
            let mut offset = 0u32;
            for (i, (name, field)) in fields.iter().enumerate() {
                // The struct's layout was checked, so every field has one.
                let (s, a) = layout(field).unwrap_or((0, 1));
                offset = align_up(offset, a);
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&json_str(name));
                out.push(':');
                let field_path = format!("{path}.{name}");
                write_value(m, addr.wrapping_add(offset), field, &field_path, out, found);
                offset += s;
            }
            out.push('}');
        }
        Enc::Array(count, element) => {
            let (s, _) = layout(element).unwrap_or((0, 1));
            if *count > MAX_ELEMENTS {
                found.cut = true;
            }
            out.push('[');
            for i in 0..(*count).min(MAX_ELEMENTS) {
                if i > 0 {
                    out.push(',');
                }
                let element_path = format!("{path}[{i}]");
                write_value(
                    m,
                    addr.wrapping_add(i * s),
                    element,
                    &element_path,
                    out,
                    found,
                );
            }
            out.push(']');
        }
        Enc::Bytes { size, .. } => {
            if *size > MAX_ELEMENTS {
                found.cut = true;
            }
            match m.bytes(addr, (*size).min(MAX_ELEMENTS)) {
                Ok(bytes) => {
                    out.push('"');
                    for b in bytes {
                        out.push_str(&format!("{b:02x}"));
                    }
                    out.push('"');
                }
                Err(_) => out.push_str("null"),
            }
        }
        Enc::Bits { size, shift, width } => match read(*size) {
            Some(raw) => out.push_str(&((raw >> shift) & ((1u64 << width) - 1)).to_string()),
            None => out.push_str("null"),
        },
        Enc::Unknown => out.push_str("null"),
    }
}

/// One class on an object's class chain, as [StateMemory::class_records]
/// gives it.
#[derive(Clone, Debug)]
pub(super) struct ClassRecord {
    pub(super) name: String,
    /// The address of the class's `ivar_list_t` in the binary, or 0 if it
    /// has none (as touchHLE's own classes).
    pub(super) ivar_list: u32,
    /// The names of its instance variables as touchHLE's class table has
    /// them, which the list is checked against.
    pub(super) ivar_names: Vec<String>,
    /// The size of an instance.
    pub(super) instance_size: u32,
}

/// What the state stream reads besides what the scene dump reads. Nothing
/// here may call the guest or panic.
pub(super) trait StateMemory: SceneMemory {
    /// The class at `isa` and its superclasses, the class first, or why
    /// not.
    fn class_records(&mut self, isa: u32) -> Result<Vec<ClassRecord>, String>;

    /// The objects in the array at `array`, in order, if it is one of
    /// touchHLE's, read from its host object.
    fn array_items(&self, array: u32) -> Option<Vec<u32>>;

    /// The value of the number at `number`, if it is one of touchHLE's
    /// NSNumbers, read from its host object.
    fn number_value(&self, number: u32) -> Option<f64>;
}

/// An instance variable as an `ivar_t` of the binary has it.
#[derive(Debug, PartialEq)]
struct RawIvar {
    name: String,
    encoding: String,
    offset: u32,
    size: u32,
}

/// The NUL-terminated string at `addr`, at most `max` bytes.
fn cstr<M: SceneMemory + ?Sized>(m: &M, addr: u32, max: u32) -> Result<String, String> {
    let mut bytes = Vec::new();
    for i in 0..max {
        let b = m.bytes(addr.checked_add(i).ok_or("past the end")?, 1)?[0];
        if b == 0 {
            return Ok(String::from_utf8_lossy(&bytes).into_owned());
        }
        bytes.push(b);
    }
    Err(format!(
        "the string at {addr:#x} is longer than {max} bytes"
    ))
}

/// The entries of the `ivar_list_t` at `list`: `{ entsize, count }` and
/// then `count` entries of `entsize` bytes, each an `ivar_t`: `{ offset
/// pointer, name, type, alignment, size }`.
fn read_ivar_list<M: SceneMemory + ?Sized>(m: &M, list: u32) -> Result<Vec<RawIvar>, String> {
    let entsize = m.word(list)?;
    let count = m.word(list.wrapping_add(4))?;
    if !(20..=64).contains(&entsize) || count > 4096 {
        return Err(format!(
            "the ivar list at {list:#x} has entsize {entsize} and count {count}"
        ));
    }
    let mut ivars = Vec::new();
    for i in 0..count {
        let entry = list.wrapping_add(8 + i * entsize);
        let offset_ptr = m.word(entry)?;
        ivars.push(RawIvar {
            name: cstr(m, m.word(entry + 4)?, 256)?,
            encoding: cstr(m, m.word(entry + 8)?, 4096)?,
            offset: m.word(offset_ptr)?,
            size: m.word(entry + 16)?,
        });
    }
    Ok(ivars)
}

/// An instance variable as the stream writes it.
#[derive(Debug)]
pub(super) struct IvarLayout {
    /// The key in an object line's `ivars`: the name, or `Class.name` if a
    /// superclass has an instance variable of that name too.
    pub(super) key: String,
    /// The class that declares it.
    pub(super) class: String,
    pub(super) name: String,
    /// The type encoding, as the binary has it.
    pub(super) encoding: String,
    pub(super) offset: u32,
    pub(super) size: u32,
    pub(super) enc: Enc,
    /// Whether the type was decoded (otherwise it is written as hex).
    pub(super) decoded: bool,
}

/// What the stream knows of a class.
#[derive(Debug)]
pub(super) struct ClassLayout {
    pub(super) name: String,
    pub(super) chain: Vec<String>,
    pub(super) size: u32,
    /// The instance variables of the class and its superclasses,
    /// superclasses first.
    pub(super) ivars: Vec<IvarLayout>,
    /// What could not be read.
    pub(super) notes: Vec<String>,
}

/// Read the layout of the class at `isa` from its class chain.
fn class_layout<M: StateMemory + ?Sized>(m: &mut M, isa: u32) -> Result<ClassLayout, String> {
    let records = m.class_records(isa)?;
    let first = records.first().ok_or("an empty class chain")?;
    let mut layout = ClassLayout {
        name: first.name.clone(),
        chain: records.iter().map(|r| r.name.clone()).collect(),
        size: first.instance_size,
        ivars: Vec::new(),
        notes: Vec::new(),
    };
    let mut keys = HashSet::new();
    for record in records.iter().rev() {
        if record.ivar_list == 0 {
            if !record.ivar_names.is_empty() {
                layout.notes.push(format!(
                    "{} has instance variables but no ivar list, they are left out",
                    record.name
                ));
            }
            continue;
        }
        let list = match read_ivar_list(m, record.ivar_list) {
            Ok(list) => list,
            Err(why) => {
                layout
                    .notes
                    .push(format!("the ivars of {} are left out: {why}", record.name));
                continue;
            }
        };
        let mut listed: Vec<&str> = list.iter().map(|ivar| ivar.name.as_str()).collect();
        let mut known: Vec<&str> = record.ivar_names.iter().map(String::as_str).collect();
        listed.sort_unstable();
        known.sort_unstable();
        if listed != known {
            layout.notes.push(format!(
                "the ivar list of {} does not match touchHLE's class table, its ivars are left out",
                record.name
            ));
            continue;
        }
        // The bitfield run the last ivar was in: its offset, and the bit
        // after the last bitfield.
        let mut run: Option<(u32, u32)> = None;
        for raw in list {
            let bits = raw
                .encoding
                .strip_prefix('b')
                .and_then(|width| width.parse::<u32>().ok())
                .filter(|&width| (1..=32).contains(&width));
            let shift = match (bits, run) {
                (Some(_), Some((offset, next))) if offset == raw.offset => next,
                _ => 0,
            };
            run = bits.map(|width| (raw.offset, shift + width));
            let (enc, decoded) = match parse_type(&raw.encoding) {
                Ok(_) if bits.is_some_and(|width| shift + width <= 8 * raw.size.min(8)) => (
                    Enc::Bits {
                        size: raw.size.min(8),
                        shift,
                        width: bits.unwrap(),
                    },
                    true,
                ),
                Ok(enc) if layout_size(&enc) == Some(raw.size) => (enc, true),
                _ => (
                    Enc::Bytes {
                        size: raw.size,
                        align: 1,
                    },
                    false,
                ),
            };
            let key = if keys.insert(raw.name.clone()) {
                raw.name.clone()
            } else {
                format!("{}.{}", record.name, raw.name)
            };
            layout.ivars.push(IvarLayout {
                key,
                class: record.name.clone(),
                name: raw.name,
                encoding: raw.encoding,
                offset: raw.offset,
                size: raw.size,
                enc,
                decoded,
            });
        }
    }
    Ok(layout)
}

fn layout_size(enc: &Enc) -> Option<u32> {
    layout(enc).map(|(size, _)| size)
}

/// The class line of `layout` (see the module's documentation).
pub(super) fn class_line(layout: &ClassLayout) -> String {
    let mut line = format!(
        "{{\"kind\":\"class\",\"class\":{},\"chain\":[{}],\"size\":{},\"ivars\":[",
        json_str(&layout.name),
        layout
            .chain
            .iter()
            .map(|name| json_str(name))
            .collect::<Vec<_>>()
            .join(","),
        layout.size
    );
    for (i, ivar) in layout.ivars.iter().enumerate() {
        if i > 0 {
            line.push(',');
        }
        line.push_str(&format!(
            "{{\"key\":{},\"class\":{},\"name\":{},\"type\":{},\"offset\":{},\"size\":{},\"decoded\":{}}}",
            json_str(&ivar.key),
            json_str(&ivar.class),
            json_str(&ivar.name),
            json_str(&ivar.encoding),
            ivar.offset,
            ivar.size,
            ivar.decoded
        ));
    }
    line.push_str("],\"notes\":[");
    line.push_str(
        &layout
            .notes
            .iter()
            .map(|note| json_str(note))
            .collect::<Vec<_>>()
            .join(","),
    );
    line.push_str("]}\n");
    line
}

/// The first line of the file (see the module's documentation).
pub(super) fn header_line(build: &str, app: &str, every: u32, classes: &[String]) -> String {
    format!(
        "{{\"kind\":\"header\",\"schema\":{},\"touchhle\":{},\"app\":{},\"every\":{},\"classes\":[{}],\"max_objects\":{},\"max_elements\":{}}}\n",
        json_str(SCHEMA),
        json_str(build),
        json_str(app),
        every,
        classes
            .iter()
            .map(|name| json_str(name))
            .collect::<Vec<_>>()
            .join(","),
        MAX_OBJECTS,
        MAX_ELEMENTS
    )
}

/// What the stream keeps from one snapshot to the next.
#[derive(Default)]
pub(super) struct StateStream {
    /// The chosen classes.
    pub(super) classes: Vec<String>,
    /// Per isa: the class's layout, or [None] if it could not be read.
    layouts: HashMap<u32, Option<Rc<ClassLayout>>>,
    /// The classes whose class line has been written.
    written: HashSet<String>,
    /// The address of the static variable that held the `CCDirector` last
    /// time, see [scene::find_singleton].
    pub(super) director_slot: Option<u32>,
}

/// Where a snapshot starts looking.
pub(super) struct Roots {
    /// The running scene, if there is one.
    pub(super) scene: Option<u32>,
    /// The app binary's data sections, each an address and a size.
    pub(super) sections: Vec<(u32, u32)>,
}

impl StateStream {
    /// A stream of the instances of `classes`.
    pub(super) fn new(classes: Vec<String>) -> Self {
        StateStream {
            classes,
            ..StateStream::default()
        }
    }

    /// The class of `object` if it is an instance (not a class) of a
    /// chosen class or a subclass of one.
    fn chosen<M: StateMemory + ?Sized>(&self, m: &mut M, object: u32) -> Option<Rc<ClassInfo>> {
        if object == 0 {
            return None;
        }
        let class = m.class_of(object).ok()?;
        (!class.name.starts_with('+') && self.classes.iter().any(|c| class.chain.contains(c)))
            .then_some(class)
    }

    /// The layout of the class of `object`, read once per class.
    fn layout_of<M: StateMemory + ?Sized>(
        &mut self,
        m: &mut M,
        object: u32,
    ) -> Result<Rc<ClassLayout>, String> {
        let isa = m.word(object)?;
        if let Some(known) = self.layouts.get(&isa) {
            return known
                .clone()
                .ok_or_else(|| format!("the class at {isa:#x} could not be read"));
        }
        let layout = class_layout(m, isa).map(Rc::new);
        self.layouts.insert(isa, layout.as_ref().ok().cloned());
        layout
    }
}

impl StateMemory for scene::GuestScene<'_> {
    fn class_records(&mut self, isa: u32) -> Result<Vec<ClassRecord>, String> {
        let objc = self.objc;
        let mut records = Vec::new();
        let mut current: Class = Ptr::from_bits(isa);
        // A bound, in case a broken chain loops.
        for _ in 0..64 {
            let host = objc
                .get_host_object(current)
                .and_then(|o| o.as_any().downcast_ref::<ClassHostObject>())
                .ok_or_else(|| format!("{:#x} is not a class we know", current.to_bits()))?;
            // A class with instance variables is the app binary's: its
            // class_t has the class data at +16, whose ivar list is at +28
            // (see src/objc/classes.rs).
            let ivar_list = if host.ivars.is_empty() {
                0
            } else {
                let data = self.word(current.to_bits().wrapping_add(16))?;
                self.word(data.wrapping_add(28))?
            };
            records.push(ClassRecord {
                name: host.name.clone(),
                ivar_list,
                ivar_names: host.ivars.keys().cloned().collect(),
                instance_size: host.instance_size,
            });
            if host.superclass == nil {
                return Ok(records);
            }
            current = host.superclass;
        }
        Err("the class chain is too long".to_string())
    }

    fn array_items(&self, array: u32) -> Option<Vec<u32>> {
        let items = crate::frameworks::foundation::ns_array::host_array_items(
            self.objc,
            Ptr::from_bits(array),
        )?;
        Some(items.into_iter().map(|item| item.to_bits()).collect())
    }

    fn number_value(&self, number: u32) -> Option<f64> {
        crate::frameworks::foundation::ns_value::host_number_value(
            self.objc,
            Ptr::from_bits(number),
        )
    }
}

/// Add `note` to `notes`, unless [MAX_NOTES] are there already.
fn add_note(notes: &mut Vec<String>, note: String) {
    if notes.len() < MAX_NOTES {
        notes.push(note);
    } else if notes.len() == MAX_NOTES {
        notes.push("more notes left out".to_string());
    }
}

/// The items of `container` if it is one of touchHLE's arrays, one of
/// touchHLE's dictionaries (its values; then the second value is true), or
/// a cocos2d `CCArray`.
fn container_items<M: StateMemory + ?Sized>(m: &mut M, container: u32) -> Option<(Vec<u32>, bool)> {
    if let Some(items) = m.array_items(container) {
        return Some((items, false));
    }
    if let Some(pairs) = m.dictionary_pairs(container) {
        return Some((pairs.into_iter().map(|(_, value)| value).collect(), true));
    }
    let class = m.class_of(container).ok()?;
    if class.name.starts_with('+') || !class.chain.iter().any(|c| c == "CCArray") {
        return None;
    }
    let data = m.ivar_addr(container, "data").ok()?;
    let data = m.word(data).ok()?;
    scene::ccarray_items(m, data)
        .ok()
        .map(|items| (items, false))
}

/// The nodes of the scene from `root`, depth-first, within the scene
/// dump's caps.
pub(super) fn scene_nodes<M: SceneMemory + ?Sized>(
    m: &mut M,
    root: u32,
    notes: &mut Vec<String>,
) -> Vec<u32> {
    let mut nodes = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![(root, 0usize)];
    while let Some((node, depth)) = stack.pop() {
        if nodes.len() == scene::MAX_NODES {
            add_note(
                notes,
                format!("the scene walk stopped at {} nodes", scene::MAX_NODES),
            );
            break;
        }
        if !seen.insert(node) {
            continue;
        }
        nodes.push(node);
        if depth + 1 >= scene::MAX_DEPTH {
            continue;
        }
        if let Ok(children) = scene::children_of(m, node) {
            for &child in children.iter().rev() {
                if child != 0 {
                    stack.push((child, depth + 1));
                }
            }
        }
    }
    nodes
}

/// Where an object was found, for the snapshot line's `found`.
#[derive(Copy, Clone)]
enum Source {
    Scene,
    Static,
    Ivar,
}

/// The lines of one snapshot of frame `frame`, presented `ms` milliseconds
/// after startup: the snapshot line, a class line for each class not
/// written before in this file, and an object line per chosen object (see
/// the module's documentation). Only reads memory and the class table.
pub(super) fn snapshot<M: StateMemory + ?Sized>(
    m: &mut M,
    stream: &mut StateStream,
    roots: &Roots,
    frame: u32,
    ms: u64,
) -> String {
    let mut notes = Vec::new();
    let mut queue: VecDeque<(u32, String, Source)> = VecDeque::new();
    match roots.scene {
        Some(root) => {
            for node in scene_nodes(m, root, &mut notes) {
                if stream.chosen(m, node).is_some() {
                    queue.push_back((node, "scene".to_string(), Source::Scene));
                }
            }
        }
        None => add_note(&mut notes, "no running scene".to_string()),
    }
    for &(start, size) in &roots.sections {
        let Ok(bytes) = m.bytes(start, size) else {
            add_note(
                &mut notes,
                format!("the section at {start:#x} could not be read"),
            );
            continue;
        };
        let words: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| u32::from_le_bytes(b))
            .collect();
        for (i, &word) in words.iter().enumerate() {
            if stream.chosen(m, word).is_some() {
                let slot = start.wrapping_add(4 * i as u32);
                queue.push_back((word, format!("static {slot:#010x}"), Source::Static));
            }
        }
    }

    let mut seen = HashSet::new();
    let mut scanned = HashSet::new();
    let mut class_lines = String::new();
    let mut object_lines = String::new();
    let mut count = 0usize;
    let mut by_class: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_source = [0usize; 3];
    while let Some((object, via, source)) = queue.pop_front() {
        if seen.contains(&object) {
            continue;
        }
        if count == MAX_OBJECTS {
            notes.push(format!(
                "cut: the cap of {MAX_OBJECTS} objects was reached, more were found"
            ));
            break;
        }
        seen.insert(object);
        let Some(class) = stream.chosen(m, object) else {
            continue;
        };
        count += 1;
        *by_class.entry(class.name.clone()).or_default() += 1;
        by_source[source as usize] += 1;

        let mut line = format!(
            "{{\"kind\":\"object\",\"frame\":{frame},\"class\":{},\"addr\":{},\"via\":{}",
            json_str(&class.name),
            json_addr(object),
            json_str(&via)
        );
        let mut found = Found::default();
        match stream.layout_of(m, object) {
            Ok(layout) => {
                if stream.written.insert(layout.name.clone()) {
                    class_lines.push_str(&class_line(&layout));
                }
                line.push_str(",\"ivars\":{");
                for (i, ivar) in layout.ivars.iter().enumerate() {
                    if i > 0 {
                        line.push(',');
                    }
                    line.push_str(&json_str(&ivar.key));
                    line.push(':');
                    write_value(
                        m,
                        object.wrapping_add(ivar.offset),
                        &ivar.enc,
                        &ivar.key,
                        &mut line,
                        &mut found,
                    );
                }
                line.push('}');
            }
            Err(why) => {
                line.push_str(",\"ivars\":null,\"error\":");
                line.push_str(&json_str(&why));
            }
        }
        if found.cut {
            line.push_str(",\"cut\":true");
        }
        line.push_str("}\n");
        object_lines.push_str(&line);

        // More chosen objects, through this one's object pointers.
        for (path, target) in found.objects {
            if seen.contains(&target) {
                continue;
            }
            if stream.chosen(m, target).is_some() {
                queue.push_back((target, format!("{}.{path}", class.name), Source::Ivar));
                continue;
            }
            if !scanned.insert(target) {
                continue;
            }
            let Some((items, dictionary)) = container_items(m, target) else {
                continue;
            };
            for (i, item) in items.into_iter().enumerate() {
                if item != 0 && !seen.contains(&item) && stream.chosen(m, item).is_some() {
                    let via = if dictionary {
                        format!("{}.{path}{{}}", class.name)
                    } else {
                        format!("{}.{path}[{i}]", class.name)
                    };
                    queue.push_back((item, via, Source::Ivar));
                }
            }
        }
    }

    let mut text = format!(
        "{{\"kind\":\"snapshot\",\"frame\":{frame},\"ms\":{ms},\"objects\":{count},\"by_class\":{{{}}},\"found\":{{\"scene\":{},\"static\":{},\"ivar\":{}}},\"notes\":[{}]}}\n",
        by_class
            .iter()
            .map(|(name, n)| format!("{}:{n}", json_str(name)))
            .collect::<Vec<_>>()
            .join(","),
        by_source[Source::Scene as usize],
        by_source[Source::Static as usize],
        by_source[Source::Ivar as usize],
        notes
            .iter()
            .map(|note| json_str(note))
            .collect::<Vec<_>>()
            .join(",")
    );
    text.push_str(&class_lines);
    text.push_str(&object_lines);
    text
}

/// The `SharedData` instance variables the profile row holds as numbers, in
/// order: the level and mode, then the five buff multipliers the level
/// starts with (`decomp/headers/SharedData.h` in the game's repo).
pub(super) const PROFILE_NUMBERS: [&str; 7] = [
    "lvlNum",
    "gameMode",
    "dmgBuffed",
    "rangeBuffed",
    "speedBuffed",
    "splashBuffed",
    "utilBuffed",
];

/// The perk flags (one byte each) the profile row holds, in order.
pub(super) const PROFILE_PERKS: [&str; 9] = [
    "fireBonus",
    "fireCrit",
    "fireMissle",
    "splashDmgBonus",
    "splashExtraGold",
    "splashReduceCost",
    "snipeDmg",
    "snipeStun",
    "snipeReduceCost",
];

/// The arrays of NSNumbers the profile row holds, in order.
pub(super) const PROFILE_ARRAYS: [&str; 7] = [
    "warningCreeps",
    "unlockArray",
    "lvlArray",
    "lvlArray1",
    "lvlArray2",
    "equipmentArray",
    "tricketArray",
];

/// The text of a `profile` row (LIVE8): one line of JSON with the profile
/// and the gear the level starts with, read from the `SharedData` at
/// `shared` without calling the guest: [PROFILE_NUMBERS], [PROFILE_PERKS]
/// and [PROFILE_ARRAYS] by name, each `null` if it cannot be read.
/// `shared` [None] gives a row that says no `SharedData` was found.
pub(super) fn profile_line<M: StateMemory + ?Sized>(m: &mut M, shared: Option<u32>) -> String {
    let Some(shared) = shared else {
        return "{\"kind\":\"profile\",\"error\":\"no SharedData found\"}".to_string();
    };
    let mut out = format!("{{\"kind\":\"profile\",\"addr\":{}", json_addr(shared));
    for (i, &name) in PROFILE_NUMBERS.iter().enumerate() {
        out.push(',');
        out.push_str(&json_str(name));
        out.push(':');
        let word = m.ivar_addr(shared, name).and_then(|addr| m.word(addr));
        match word {
            // lvlNum and gameMode are ints, the buffs floats
            Ok(word) if i < 2 => out.push_str(&(word as i32).to_string()),
            Ok(word) => {
                let value = f32::from_bits(word);
                out.push_str(&json_float(f64::from(value), value.to_string()));
            }
            Err(_) => out.push_str("null"),
        }
    }
    for &name in &PROFILE_PERKS {
        out.push(',');
        out.push_str(&json_str(name));
        out.push(':');
        match m
            .ivar_addr(shared, name)
            .and_then(|addr| m.bytes(addr, 1).map(|b| b[0]))
        {
            Ok(byte) => out.push_str(&byte.to_string()),
            Err(_) => out.push_str("null"),
        }
    }
    for &name in &PROFILE_ARRAYS {
        out.push(',');
        out.push_str(&json_str(name));
        out.push(':');
        let items = m
            .ivar_addr(shared, name)
            .and_then(|addr| m.word(addr))
            .ok()
            .and_then(|array| m.array_items(array));
        let Some(items) = items else {
            out.push_str("null");
            continue;
        };
        out.push('[');
        for (j, &item) in items.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            match m.number_value(item) {
                Some(value) => out.push_str(&json_number(value)),
                None => out.push_str("null"),
            }
        }
        out.push(']');
    }
    out.push('}');
    out
}

/// An NSNumber's value as JSON: a whole number without a fraction.
fn json_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        (value as i64).to_string()
    } else {
        json_float(value, value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_encodings_decode_with_their_layout() {
        let point = || {
            Enc::Struct(vec![
                ("x".to_string(), Enc::F32),
                ("y".to_string(), Enc::F32),
            ])
        };
        assert_eq!(parse_type("{CGPoint=\"x\"f\"y\"f}"), Ok(point()));
        let rect = parse_type(
            "{CGRect=\"origin\"{CGPoint=\"x\"f\"y\"f}\"size\"{CGSize=\"width\"f\"height\"f}}",
        )
        .unwrap();
        assert_eq!(layout(&rect), Some((16, 4)));
        // towersLayer's grid of towers.
        let towers = parse_type("[13[9@\"towerGrid\"]]").unwrap();
        assert_eq!(layout(&towers), Some((468, 4)));
        // Without names the fields are numbered.
        assert_eq!(
            parse_type("{CGPoint=ff}"),
            Ok(Enc::Struct(vec![
                ("0".to_string(), Enc::F32),
                ("1".to_string(), Enc::F32),
            ]))
        );
        // After `@` in a struct with names: the next field's name, or a
        // class name when another name follows.
        let names = |enc: Enc| match enc {
            Enc::Struct(fields) => fields.into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
            _ => panic!("not a struct"),
        };
        assert_eq!(names(parse_type("{S=\"a\"@\"b\"i}").unwrap()), ["a", "b"]);
        assert_eq!(
            names(parse_type("{S=\"a\"@\"NSString\"\"b\"i}").unwrap()),
            ["a", "b"]
        );
        // 64-bit types are 4-byte aligned.
        assert_eq!(
            layout(&parse_type("{S=\"a\"c\"b\"q}").unwrap()),
            Some((12, 4))
        );
        assert_eq!(layout(&parse_type("d").unwrap()), Some((8, 4)));
        assert_eq!(layout(&parse_type("rc").unwrap()), Some((1, 1)));
        // Pointers, also to a struct that refers to itself.
        assert_eq!(parse_type("^{Node=^{Node}i}"), Ok(Enc::Pointer));
        assert_eq!(parse_type("^?"), Ok(Enc::Pointer));
        assert_eq!(parse_type("@?"), Ok(Enc::Object));
        assert_eq!(parse_type("#"), Ok(Enc::Pointer));
        // A union is bytes the size of its largest member.
        assert_eq!(
            parse_type("(U=\"a\"i\"b\"d)"),
            Ok(Enc::Bytes { size: 8, align: 4 })
        );
        // Unknown sizes.
        assert_eq!(layout(&parse_type("b3").unwrap()), None);
        assert_eq!(layout(&parse_type("{Opaque}").unwrap()), None);
        assert_eq!(layout(&parse_type("{S=\"a\"i\"b\"b1}").unwrap()), None);
        // Malformed.
        assert!(parse_type("{S=\"a\"i").is_err());
        assert!(parse_type("[3i").is_err());
        assert!(parse_type("ii").is_err());
        assert!(parse_type("").is_err());
        assert!(parse_type("%").is_err());
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_str("a\"b\\c\nd\u{1}"), "\"a\\\"b\\\\c\\nd\\u0001\"");
        assert_eq!(json_float(f64::NAN, String::new()), "\"nan\"");
        assert_eq!(json_float(f64::NEG_INFINITY, String::new()), "\"-inf\"");
    }

    /// A made-up memory from [BASE] and class table.
    struct FakeGuest {
        memory: Vec<u8>,
        /// Per isa: the class chain's records, the class first.
        classes: HashMap<u32, Vec<ClassRecord>>,
        /// Per isa and ivar name: the offset (for `children_` and `data`).
        offsets: HashMap<(u32, &'static str), u32>,
        arrays: HashMap<u32, Vec<u32>>,
        dictionaries: HashMap<u32, Vec<(u32, u32)>>,
        numbers: HashMap<u32, f64>,
    }

    const BASE: u32 = 0x1000;

    impl FakeGuest {
        fn new() -> Self {
            FakeGuest {
                memory: Vec::new(),
                classes: HashMap::new(),
                offsets: HashMap::new(),
                arrays: HashMap::new(),
                dictionaries: HashMap::new(),
                numbers: HashMap::new(),
            }
        }

        /// Room for `size` bytes (rounded up to a word), zeroed.
        fn alloc(&mut self, size: u32) -> u32 {
            let addr = BASE + self.memory.len() as u32;
            let size = align_up(size.max(4), 4) as usize;
            self.memory.resize(self.memory.len() + size, 0);
            addr
        }

        fn put(&mut self, addr: u32, bytes: &[u8]) {
            let at = (addr - BASE) as usize;
            self.memory[at..at + bytes.len()].copy_from_slice(bytes);
        }

        fn put_word(&mut self, addr: u32, word: u32) {
            self.put(addr, &word.to_le_bytes());
        }

        fn cstr(&mut self, text: &str) -> u32 {
            let addr = self.alloc(text.len() as u32 + 1);
            self.put(addr, text.as_bytes());
            addr
        }

        /// An `ivar_list_t` of `(name, encoding, offset, size)`.
        fn ivar_list(&mut self, ivars: &[(&str, &str, u32, u32)]) -> u32 {
            let list = self.alloc(8 + 20 * ivars.len() as u32);
            self.put_word(list, 20);
            self.put_word(list + 4, ivars.len() as u32);
            for (i, &(name, encoding, offset, size)) in ivars.iter().enumerate() {
                let entry = list + 8 + 20 * i as u32;
                let offset_ptr = self.alloc(4);
                self.put_word(offset_ptr, offset);
                let name_ptr = self.cstr(name);
                let encoding_ptr = self.cstr(encoding);
                self.put_word(entry, offset_ptr);
                self.put_word(entry + 4, name_ptr);
                self.put_word(entry + 8, encoding_ptr);
                self.put_word(entry + 16, size);
            }
            list
        }

        /// A class (at a fresh address) with its chain.
        fn class(&mut self, records: Vec<ClassRecord>) -> u32 {
            let isa = self.alloc(4);
            self.classes.insert(isa, records);
            isa
        }

        fn object(&mut self, isa: u32, size: u32) -> u32 {
            let object = self.alloc(size);
            self.put_word(object, isa);
            object
        }
    }

    fn record(name: &str, ivar_list: u32, names: &[&str], instance_size: u32) -> ClassRecord {
        ClassRecord {
            name: name.to_string(),
            ivar_list,
            ivar_names: names.iter().map(|n| n.to_string()).collect(),
            instance_size,
        }
    }

    impl SceneMemory for FakeGuest {
        fn bytes(&self, addr: u32, count: u32) -> Result<&[u8], String> {
            let start = addr.checked_sub(BASE).ok_or("below the memory")? as usize;
            self.memory
                .get(start..start + count as usize)
                .ok_or_else(|| format!("{addr:#x} is not mapped"))
        }

        fn class_of(&mut self, object: u32) -> Result<Rc<ClassInfo>, String> {
            let isa = self.word(object)?;
            let records = self.classes.get(&isa).ok_or("no class")?;
            Ok(Rc::new(ClassInfo {
                name: records[0].name.clone(),
                chain: records.iter().map(|r| r.name.clone()).collect(),
            }))
        }

        fn ivar_addr(&mut self, object: u32, name: &'static str) -> Result<u32, String> {
            let isa = self.word(object)?;
            let offset = self.offsets.get(&(isa, name)).ok_or("no such ivar")?;
            Ok(object + offset)
        }

        fn string_text(&self, _string: u32) -> Option<String> {
            None
        }

        fn dictionary_pairs(&self, dict: u32) -> Option<Vec<(u32, u32)>> {
            self.dictionaries.get(&dict).cloned()
        }
    }

    impl StateMemory for FakeGuest {
        fn class_records(&mut self, isa: u32) -> Result<Vec<ClassRecord>, String> {
            self.classes
                .get(&isa)
                .cloned()
                .ok_or_else(|| "no class".to_string())
        }

        fn array_items(&self, array: u32) -> Option<Vec<u32>> {
            self.arrays.get(&array).cloned()
        }

        fn number_value(&self, number: u32) -> Option<f64> {
            self.numbers.get(&number).copied()
        }
    }

    /// A `SharedData` at a fresh address with every profile field: the
    /// numbers from offset 4, the perks after them, then the arrays, each an
    /// array of NSNumbers with the values given.
    fn shared_data(
        g: &mut FakeGuest,
        numbers: [f64; 7],
        perks: [u8; 9],
        arrays: &[&[f64]; 7],
    ) -> u32 {
        let isa = g.class(vec![
            record("SharedData", 0, &[], 128),
            record("NSObject", 0, &[], 4),
        ]);
        let object = g.object(isa, 128);
        for (i, name) in PROFILE_NUMBERS.iter().enumerate() {
            let at = 4 + 4 * i as u32;
            g.offsets.insert((isa, name), at);
            let word = if i < 2 {
                numbers[i] as i32 as u32
            } else {
                (numbers[i] as f32).to_bits()
            };
            g.put_word(object + at, word);
        }
        for (i, name) in PROFILE_PERKS.iter().enumerate() {
            let at = 32 + i as u32;
            g.offsets.insert((isa, name), at);
            g.put(object + at, &[perks[i]]);
        }
        for (i, name) in PROFILE_ARRAYS.iter().enumerate() {
            let at = 44 + 4 * i as u32;
            g.offsets.insert((isa, name), at);
            let array = g.alloc(4);
            let mut items = Vec::new();
            for &v in arrays[i] {
                let n = g.alloc(4);
                g.numbers.insert(n, v);
                items.push(n);
            }
            g.arrays.insert(array, items);
            g.put_word(object + at, array);
        }
        object
    }

    #[test]
    fn the_profile_row_holds_the_numbers_perks_and_arrays() {
        let mut g = FakeGuest::new();
        let shared = shared_data(
            &mut g,
            [24.0, 1.0, 1.1, 1.0, 1.25, 1.0, 1.5],
            [1, 0, 0, 0, 1, 0, 0, 0, 1],
            &[
                &[1.0, 0.0, 1.0],
                &[1.0, 1.0],
                &[3.0, -1.0],
                &[0.0],
                &[],
                &[2.0, 5.0],
                &[7.0],
            ],
        );
        assert_eq!(
            profile_line(&mut g, Some(shared)),
            format!(
                "{{\"kind\":\"profile\",\"addr\":\"{shared:#010x}\",\
                 \"lvlNum\":24,\"gameMode\":1,\"dmgBuffed\":1.1,\"rangeBuffed\":1,\
                 \"speedBuffed\":1.25,\"splashBuffed\":1,\"utilBuffed\":1.5,\
                 \"fireBonus\":1,\"fireCrit\":0,\"fireMissle\":0,\"splashDmgBonus\":0,\
                 \"splashExtraGold\":1,\"splashReduceCost\":0,\"snipeDmg\":0,\
                 \"snipeStun\":0,\"snipeReduceCost\":1,\
                 \"warningCreeps\":[1,0,1],\"unlockArray\":[1,1],\"lvlArray\":[3,-1],\
                 \"lvlArray1\":[0],\"lvlArray2\":[],\"equipmentArray\":[2,5],\
                 \"tricketArray\":[7]}}"
            )
        );
    }

    #[test]
    fn a_profile_row_without_shared_data_says_so() {
        let mut g = FakeGuest::new();
        assert_eq!(
            profile_line(&mut g, None),
            "{\"kind\":\"profile\",\"error\":\"no SharedData found\"}"
        );
    }

    #[test]
    fn a_profile_field_that_cannot_be_read_is_null() {
        let mut g = FakeGuest::new();
        let shared = shared_data(
            &mut g,
            [2.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            [0; 9],
            &[&[1.0], &[], &[], &[], &[], &[], &[]],
        );
        // warningCreeps points at something that is not one of touchHLE's
        // arrays; unlockArray holds an object that is not an NSNumber.
        let isa = g.word(shared).unwrap();
        let at = g.offsets[&(isa, "warningCreeps")];
        let not_an_array = g.alloc(4);
        g.put_word(shared + at, not_an_array);
        let at = g.offsets[&(isa, "unlockArray")];
        let array = g.word(shared + at).unwrap();
        let not_a_number = g.alloc(4);
        g.arrays.insert(array, vec![not_a_number]);
        // gameMode has no offset: the class has no such ivar.
        g.offsets.remove(&(isa, "gameMode"));
        let line = profile_line(&mut g, Some(shared));
        assert!(line.contains("\"gameMode\":null,"), "{line}");
        assert!(line.contains("\"warningCreeps\":null,"), "{line}");
        assert!(line.contains("\"unlockArray\":[null],"), "{line}");
        assert!(line.contains("\"lvlNum\":2,"), "{line}");
    }

    #[test]
    fn objects_are_found_in_the_scene_statics_arrays_and_ivars() {
        let mut g = FakeGuest::new();
        let nsobject = record("NSObject", 0, &[], 4);
        // CCNode: children_ at 4, tag_ at 8.
        let node_list = g.ivar_list(&[("children_", "@\"CCArray\"", 4, 4), ("tag_", "i", 8, 4)]);
        let ccnode = record("CCNode", node_list, &["children_", "tag_"], 12);
        let scene_isa = g.class(vec![
            record("CCScene", 0, &[], 12),
            ccnode.clone(),
            nsobject.clone(),
        ]);
        // Creeps: a float, a struct, an object, a union, a byte.
        let creeps_list = g.ivar_list(&[
            ("hp", "f", 12, 4),
            ("myGrid", "{CGPoint=\"x\"f\"y\"f}", 16, 8),
            ("sharedData", "@\"SharedData\"", 24, 4),
            ("u", "(U=\"a\"i\"b\"s)", 28, 4),
            ("moving", "c", 32, 1),
            ("dirty", "b1", 33, 1),
            ("stage", "b3", 33, 1),
            ("other", "b2", 34, 1),
        ]);
        let creeps_isa = g.class(vec![
            record(
                "Creeps",
                creeps_list,
                &[
                    "hp",
                    "myGrid",
                    "sharedData",
                    "u",
                    "moving",
                    "dirty",
                    "stage",
                    "other",
                ],
                35,
            ),
            ccnode.clone(),
            nsobject.clone(),
        ]);
        // SharedData: an NSMutableArray, a grid of creeps, a long long, a
        // double and a struct whose fields are not given.
        let shared_list = g.ivar_list(&[
            ("mcreepsArray", "@\"NSMutableArray\"", 4, 4),
            ("grid", "[2[2@\"Creeps\"]]", 8, 16),
            ("big", "q", 24, 8),
            ("speed", "d", 32, 8),
            ("bad", "{Opaque}", 40, 4),
        ]);
        let shared_isa = g.class(vec![
            record(
                "SharedData",
                shared_list,
                &["mcreepsArray", "grid", "big", "speed", "bad"],
                44,
            ),
            nsobject.clone(),
        ]);
        let ccarray_isa = g.class(vec![record("CCArray", 0, &[], 8), nsobject.clone()]);
        g.offsets.insert((ccarray_isa, "data"), 4);
        g.offsets.insert((scene_isa, "children_"), 4);
        g.offsets.insert((creeps_isa, "children_"), 4);
        let nsarray_isa = g.class(vec![record("NSMutableArray", 0, &[], 4), nsobject]);

        let creep1 = g.object(creeps_isa, 36);
        g.put_word(creep1 + 8, (-5i32) as u32);
        g.put(creep1 + 12, &30.5f32.to_le_bytes());
        g.put(creep1 + 16, &1.0f32.to_le_bytes());
        g.put(creep1 + 20, &(-2.5f32).to_le_bytes());
        g.put(creep1 + 28, &[1, 2, 3, 4]);
        g.put(creep1 + 32, &[0xff, 0b1011, 0b10]);
        let creep2 = g.object(creeps_isa, 36);
        let creep3 = g.object(creeps_isa, 36);
        let shared = g.object(shared_isa, 44);
        g.put_word(creep1 + 24, shared);
        g.put(shared + 24, &(-3i64).to_le_bytes());
        g.put(shared + 32, &f64::INFINITY.to_le_bytes());
        // The scene: one child, creep1, in a CCArray.
        let scene_node = g.object(scene_isa, 12);
        let array = g.object(ccarray_isa, 8);
        let data = g.alloc(12);
        let arr = g.alloc(4);
        g.put_word(arr, creep1);
        g.put_word(data, 1);
        g.put_word(data + 4, 4);
        g.put_word(data + 8, arr);
        g.put_word(array + 4, data);
        g.put_word(scene_node + 4, array);
        // SharedData's array holds creep1 and creep2, its grid creep3.
        let mutable_array = g.object(nsarray_isa, 4);
        g.arrays.insert(mutable_array, vec![creep1, creep2]);
        g.put_word(shared + 4, mutable_array);
        g.put_word(shared + 8 + 8, creep3);
        // A static variable holds SharedData.
        let statics = g.alloc(8);
        g.put_word(statics + 4, shared);

        let mut stream = StateStream {
            classes: vec!["Creeps".to_string(), "SharedData".to_string()],
            ..StateStream::default()
        };
        let roots = Roots {
            scene: Some(scene_node),
            sections: vec![(statics, 8)],
        };
        let text = snapshot(&mut g, &mut stream, &roots, 30, 1000);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "{\"kind\":\"snapshot\",\"frame\":30,\"ms\":1000,\"objects\":4,\"by_class\":{\"Creeps\":3,\"SharedData\":1},\"found\":{\"scene\":1,\"static\":1,\"ivar\":2},\"notes\":[]}"
        );
        assert_eq!(
            lines[1],
            concat!(
                "{\"kind\":\"class\",\"class\":\"Creeps\",\"chain\":[\"Creeps\",\"CCNode\",\"NSObject\"],\"size\":35,\"ivars\":[",
                "{\"key\":\"children_\",\"class\":\"CCNode\",\"name\":\"children_\",\"type\":\"@\\\"CCArray\\\"\",\"offset\":4,\"size\":4,\"decoded\":true},",
                "{\"key\":\"tag_\",\"class\":\"CCNode\",\"name\":\"tag_\",\"type\":\"i\",\"offset\":8,\"size\":4,\"decoded\":true},",
                "{\"key\":\"hp\",\"class\":\"Creeps\",\"name\":\"hp\",\"type\":\"f\",\"offset\":12,\"size\":4,\"decoded\":true},",
                "{\"key\":\"myGrid\",\"class\":\"Creeps\",\"name\":\"myGrid\",\"type\":\"{CGPoint=\\\"x\\\"f\\\"y\\\"f}\",\"offset\":16,\"size\":8,\"decoded\":true},",
                "{\"key\":\"sharedData\",\"class\":\"Creeps\",\"name\":\"sharedData\",\"type\":\"@\\\"SharedData\\\"\",\"offset\":24,\"size\":4,\"decoded\":true},",
                "{\"key\":\"u\",\"class\":\"Creeps\",\"name\":\"u\",\"type\":\"(U=\\\"a\\\"i\\\"b\\\"s)\",\"offset\":28,\"size\":4,\"decoded\":true},",
                "{\"key\":\"moving\",\"class\":\"Creeps\",\"name\":\"moving\",\"type\":\"c\",\"offset\":32,\"size\":1,\"decoded\":true},",
                "{\"key\":\"dirty\",\"class\":\"Creeps\",\"name\":\"dirty\",\"type\":\"b1\",\"offset\":33,\"size\":1,\"decoded\":true},",
                "{\"key\":\"stage\",\"class\":\"Creeps\",\"name\":\"stage\",\"type\":\"b3\",\"offset\":33,\"size\":1,\"decoded\":true},",
                "{\"key\":\"other\",\"class\":\"Creeps\",\"name\":\"other\",\"type\":\"b2\",\"offset\":34,\"size\":1,\"decoded\":true}",
                "],\"notes\":[]}"
            )
        );
        assert!(lines[2].starts_with("{\"kind\":\"class\",\"class\":\"SharedData\""));
        assert!(lines[2].contains(
            "{\"key\":\"bad\",\"class\":\"SharedData\",\"name\":\"bad\",\"type\":\"{Opaque}\",\"offset\":40,\"size\":4,\"decoded\":false}"
        ));
        assert_eq!(
            lines[3],
            format!(
                "{{\"kind\":\"object\",\"frame\":30,\"class\":\"Creeps\",\"addr\":\"{creep1:#010x}\",\"via\":\"scene\",\"ivars\":{{\"children_\":\"0x00000000\",\"tag_\":-5,\"hp\":30.5,\"myGrid\":{{\"x\":1,\"y\":-2.5}},\"sharedData\":\"{shared:#010x}\",\"u\":\"01020304\",\"moving\":-1,\"dirty\":1,\"stage\":5,\"other\":2}}}}"
            )
        );
        assert_eq!(
            lines[4],
            format!(
                "{{\"kind\":\"object\",\"frame\":30,\"class\":\"SharedData\",\"addr\":\"{shared:#010x}\",\"via\":\"static {:#010x}\",\"ivars\":{{\"mcreepsArray\":\"{mutable_array:#010x}\",\"grid\":[[\"0x00000000\",\"0x00000000\"],[\"{creep3:#010x}\",\"0x00000000\"]],\"big\":-3,\"speed\":\"inf\",\"bad\":\"00000000\"}}}}",
                statics + 4
            )
        );
        assert!(lines[5].contains(&format!(
            "\"addr\":\"{creep2:#010x}\",\"via\":\"SharedData.mcreepsArray[1]\""
        )));
        assert!(lines[6].contains(&format!(
            "\"addr\":\"{creep3:#010x}\",\"via\":\"SharedData.grid[1][0]\""
        )));
        assert_eq!(lines.len(), 7);

        // The next snapshot writes no class lines.
        let text = snapshot(&mut g, &mut stream, &roots, 60, 2000);
        assert_eq!(text.lines().count(), 5);
        assert!(!text.contains("\"kind\":\"class\""));
    }

    #[test]
    fn a_list_that_does_not_match_the_class_table_is_left_out() {
        let mut g = FakeGuest::new();
        let list = g.ivar_list(&[("hp", "f", 4, 4)]);
        let isa = g.class(vec![record("Tower", list, &["hp", "other"], 8)]);
        let tower = g.object(isa, 8);
        let statics = g.alloc(4);
        g.put_word(statics, tower);
        let mut stream = StateStream {
            classes: vec!["Tower".to_string()],
            ..StateStream::default()
        };
        let roots = Roots {
            scene: None,
            sections: vec![(statics, 4)],
        };
        let text = snapshot(&mut g, &mut stream, &roots, 0, 0);
        assert!(text.contains("\"notes\":[\"no running scene\"]"));
        assert!(text.contains(
            "\"notes\":[\"the ivar list of Tower does not match touchHLE's class table, its ivars are left out\"]"
        ));
        assert!(text.contains("\"ivars\":{}"));
    }

    #[test]
    fn the_header_names_the_schema_and_the_caps() {
        assert_eq!(
            header_line("touchHLE v1", "com.example", 30, &["Creeps".to_string()]),
            "{\"kind\":\"header\",\"schema\":\"touchHLE-state-1\",\"touchhle\":\"touchHLE v1\",\"app\":\"com.example\",\"every\":30,\"classes\":[\"Creeps\"],\"max_objects\":5000,\"max_elements\":1024}\n"
        );
    }
}
