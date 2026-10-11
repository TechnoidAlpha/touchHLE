/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Scene dumps for the message trace: what cocos2d has on screen, as one
//! line per node, written to `scene/s{frame:06}_{ms}.txt` next to
//! `trace.log` (see the parent module for when a dump is taken).
//!
//! The walk starts at the `CCDirector`'s `runningScene_` and goes through
//! each node's `children_` depth-first, in the order of the `CCArray`
//! (cocos2d keeps it sorted by `zOrder_`). Every field is read from guest
//! memory by ivar name, through the class chain, and nothing calls the
//! guest: the game is in the middle of presenting a frame. The director and
//! the `CCTextureCache` are cocos2d singletons kept in file-static
//! variables (`DAT_001512a0` and `DAT_001512bc` in the decompile of
//! 4Towers), so they are found by looking through the app binary's data
//! sections for a word that points at an instance of that class (see
//! [find_singleton]); the slot is kept and checked again at each dump.
//!
//! A field that cannot be read, or that the node's class does not have, is
//! `-`, and the walk goes on. The walk stops after [MAX_NODES] nodes and
//! does not go deeper than [MAX_DEPTH] levels, and says so at the end of
//! the file.
//!
//! The walk runs over [SceneMemory], so the tests can give it a made-up
//! memory and class table.

use super::peek::{fmt_f32_6, fmt_str, STR_MAX_BYTES};
use super::{class_chain_names, class_name_for_trace, ivar_offset, read_guest_bytes};
use crate::mach_o::MachO;
use crate::mem::{Mem, Ptr};
use crate::objc::{Class, ClassHostObject, ObjC};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Folder in the user data directory the dumps are written to, next to
/// `frames/`.
pub const SCENE_DIR: &str = "scene";

/// At most this many nodes are written per dump.
pub(super) const MAX_NODES: usize = 5000;

/// Nodes this many levels below the running scene (which is level 0) or
/// deeper are not written.
pub(super) const MAX_DEPTH: usize = 32;

/// At most this many notes (children that could not be read, and the like)
/// are written at the end of a dump. The notes that say a cap cut the walk
/// are written after them all the same.
const MAX_NOTES: usize = 50;

/// The columns of a node line, in order. See dev-docs/4towers-branch.md,
/// "Scene dump", for what each holds.
pub(super) const COLUMNS: [&str; 43] = [
    // Every node (CCNode).
    "class",
    "depth",
    "addr",
    "tag",
    "z",
    "x",
    "y",
    "anchor_x",
    "anchor_y",
    "rel_anchor",
    "scale_x",
    "scale_y",
    "rotation",
    "visible",
    "width",
    "height",
    // Nodes with a colour (CCSprite, CCAtlasNode, CCLabelBMFont,
    // CCLayerColor).
    "opacity",
    "r",
    "g",
    "b",
    "opacity_rgb",
    // CCSprite.
    "flip_x",
    "flip_y",
    "rect_x",
    "rect_y",
    "rect_w",
    "rect_h",
    "batch",
    // The texture (CCSprite, CCAtlasNode, CCSpriteBatchNode).
    "tex",
    "tex_name",
    "tex_w",
    "tex_h",
    "tex_size_w",
    "tex_size_h",
    "tex_format",
    "tex_max_s",
    "tex_max_t",
    "tex_file",
    // Labels.
    "text",
    "font",
    "font_size",
    "dim_w",
    "dim_h",
];

/// Classes whose instances have `opacity_` and a `ccColor3B color_`.
/// `CCSprite` and `CCAtlasNode` also have `colorUnmodified_` and
/// `opacityModifyRGB_`, and `CCLabelBMFont` has `opacityModifyRGB_`.
const COLOR_CLASSES: [&str; 4] = ["CCSprite", "CCAtlasNode", "CCLabelBMFont", "CCLayerColor"];

/// Classes whose text is in `string_`.
const LABEL_CLASSES: [&str; 3] = ["CCLabelTTF", "CCLabelAtlas", "CCLabelBMFont"];

/// What the walk knows of an object's class.
#[derive(Debug)]
pub(super) struct ClassInfo {
    /// The name as the trace writes it (`+` first for a metaclass).
    pub(super) name: String,
    /// The names of the class and each superclass.
    pub(super) chain: Vec<String>,
}

impl ClassInfo {
    fn is_a(&self, class: &str) -> bool {
        self.chain.iter().any(|name| name == class)
    }

    fn is_any_of(&self, classes: &[&str]) -> bool {
        classes.iter().any(|class| self.is_a(class))
    }
}

/// What the walk reads from the guest. Nothing here may call the guest or
/// panic.
pub(super) trait SceneMemory {
    /// The `count` bytes at `addr`, or why not.
    fn bytes(&self, addr: u32, count: u32) -> Result<&[u8], String>;

    /// The class of the object at `object` (not nil), or why not.
    fn class_of(&mut self, object: u32) -> Result<Rc<ClassInfo>, String>;

    /// The address of the ivar `name` of the object at `object`, found on
    /// its class chain, or why not.
    fn ivar_addr(&mut self, object: u32, name: &'static str) -> Result<u32, String>;

    /// The text of the string at `string`, if it is one of touchHLE's.
    fn string_text(&self, string: u32) -> Option<String>;

    /// The little-endian word at `addr`.
    fn word(&self, addr: u32) -> Result<u32, String> {
        let b = self.bytes(addr, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// How a field is read and written.
#[derive(Copy, Clone)]
enum Ty {
    I32,
    U32,
    F32,
    U8,
}

/// A value read at `addr`, written as its column shows it.
fn read_value<M: SceneMemory + ?Sized>(m: &M, addr: u32, ty: Ty) -> Result<String, String> {
    Ok(match ty {
        Ty::U8 => m.bytes(addr, 1)?[0].to_string(),
        Ty::I32 => (m.word(addr)? as i32).to_string(),
        Ty::U32 => m.word(addr)?.to_string(),
        Ty::F32 => fmt_f32_6(f32::from_bits(m.word(addr)?)),
    })
}

/// The field `offset` bytes into the ivar `ivar` of `object`, or `-`.
fn field<M: SceneMemory + ?Sized>(
    m: &mut M,
    object: u32,
    ivar: &'static str,
    offset: u32,
    ty: Ty,
) -> String {
    m.ivar_addr(object, ivar)
        .and_then(|addr| {
            let addr = addr
                .checked_add(offset)
                .ok_or_else(|| "past the end".to_string())?;
            read_value(m, addr, ty)
        })
        .unwrap_or_else(|_| "-".to_string())
}

/// The pointer in the ivar `ivar` of `object`.
fn pointer<M: SceneMemory + ?Sized>(
    m: &mut M,
    object: u32,
    ivar: &'static str,
) -> Result<u32, String> {
    let addr = m.ivar_addr(object, ivar)?;
    m.word(addr)
}

/// A string ivar's text, escaped and cut as a `str` peek writes it: `nil`,
/// or `-` if it cannot be read or is not one of touchHLE's strings.
fn text_field<M: SceneMemory + ?Sized>(m: &mut M, object: u32, ivar: &'static str) -> String {
    match pointer(m, object, ivar) {
        Ok(0) => "nil".to_string(),
        Ok(string) => m
            .string_text(string)
            .map_or_else(|| "-".to_string(), |text| fmt_str(&text)),
        Err(_) => "-".to_string(),
    }
}

/// The ten texture columns for the `CCTexture2D` at `texture`: its address,
/// `name_` (the GL texture name), `width_` and `height_` (pixels), `size_`
/// (the content size), `format_`, `maxS_`, `maxT_`, and the file it was
/// loaded from, from `files` (the texture cache's keys).
fn texture_columns<M: SceneMemory + ?Sized>(
    m: &mut M,
    texture: Result<u32, String>,
    files: &HashMap<u32, String>,
) -> Vec<String> {
    let mut columns = Vec::with_capacity(10);
    let texture = match texture {
        Ok(0) => {
            columns.push("nil".to_string());
            None
        }
        Ok(texture) => {
            columns.push(format!("{texture:#010x}"));
            Some(texture)
        }
        Err(_) => {
            columns.push("-".to_string());
            None
        }
    };
    let texture = texture.filter(|&t| m.class_of(t).is_ok_and(|c| c.is_a("CCTexture2D")));
    match texture {
        Some(t) => {
            columns.push(field(m, t, "name_", 0, Ty::U32));
            columns.push(field(m, t, "width_", 0, Ty::U32));
            columns.push(field(m, t, "height_", 0, Ty::U32));
            columns.push(field(m, t, "size_", 0, Ty::F32));
            columns.push(field(m, t, "size_", 4, Ty::F32));
            columns.push(field(m, t, "format_", 0, Ty::I32));
            columns.push(field(m, t, "maxS_", 0, Ty::F32));
            columns.push(field(m, t, "maxT_", 0, Ty::F32));
            columns.push(
                files
                    .get(&t)
                    .map_or_else(|| "-".to_string(), |file| fmt_str(file)),
            );
        }
        None => columns.extend(std::iter::repeat_n("-".to_string(), 9)),
    }
    columns
}

/// The texture a node draws with, if its class has one: a `CCSprite`'s
/// `texture_`, or for a sprite drawn by a batch node (`usesBatchNode_`) the
/// batch's, through `textureAtlas_`; a `CCAtlasNode`'s or a
/// `CCSpriteBatchNode`'s (`CCLabelAtlas`, `CCLabelBMFont`), through
/// `textureAtlas_`. [None] for other classes.
fn node_texture<M: SceneMemory + ?Sized>(
    m: &mut M,
    node: u32,
    class: &ClassInfo,
) -> Option<Result<u32, String>> {
    let through_atlas = |m: &mut M| {
        let atlas = pointer(m, node, "textureAtlas_")?;
        if atlas == 0 {
            return Err("no textureAtlas_".to_string());
        }
        pointer(m, atlas, "texture_")
    };
    if class.is_a("CCSprite") {
        let batched = m
            .ivar_addr(node, "usesBatchNode_")
            .and_then(|addr| Ok(m.bytes(addr, 1)?[0] != 0))
            .unwrap_or(false);
        if batched {
            if let Ok(texture) = through_atlas(m) {
                return Some(Ok(texture));
            }
        }
        Some(pointer(m, node, "texture_"))
    } else if class.is_any_of(&["CCAtlasNode", "CCSpriteBatchNode"]) {
        Some(through_atlas(m))
    } else {
        None
    }
}

/// The ivar that holds the colour the game set on `node`, which is what
/// its `color` returns. With `opacityModifyRGB_` on (cocos2d's default for
/// a `CCSprite` and a `CCAtlasNode`), `setColor:` keeps the colour in
/// `colorUnmodified_` and multiplies `color_` by `opacity_ / 255`
/// (`CCSprite.c` and `CCAtlasNode.c` in the decompile). A class without
/// `colorUnmodified_` keeps it in `color_`.
fn set_color_ivar<M: SceneMemory + ?Sized>(m: &mut M, node: u32) -> Result<&'static str, String> {
    if m.ivar_addr(node, "colorUnmodified_").is_err() {
        return Ok("color_");
    }
    let addr = m.ivar_addr(node, "opacityModifyRGB_")?;
    Ok(if m.bytes(addr, 1)?[0] != 0 {
        "colorUnmodified_"
    } else {
        "color_"
    })
}

/// One line of the dump for `node`, `depth` levels below the running
/// scene: the [COLUMNS], tab-separated, the class name indented by two
/// spaces per level. A column that does not apply to the node's class, or
/// cannot be read, is `-`.
pub(super) fn node_line<M: SceneMemory + ?Sized>(
    m: &mut M,
    node: u32,
    depth: usize,
    files: &HashMap<u32, String>,
) -> String {
    let class = m.class_of(node).ok();
    let mut columns: Vec<String> = Vec::with_capacity(COLUMNS.len());
    columns.push(format!(
        "{}{}",
        "  ".repeat(depth),
        class.as_ref().map_or("?", |c| c.name.as_str())
    ));
    columns.push(depth.to_string());
    columns.push(format!("{node:#010x}"));
    let node_fields: [(&'static str, u32, Ty); 13] = [
        ("tag_", 0, Ty::I32),
        ("zOrder_", 0, Ty::I32),
        ("position_", 0, Ty::F32),
        ("position_", 4, Ty::F32),
        ("anchorPoint_", 0, Ty::F32),
        ("anchorPoint_", 4, Ty::F32),
        ("isRelativeAnchorPoint_", 0, Ty::U8),
        ("scaleX_", 0, Ty::F32),
        ("scaleY_", 0, Ty::F32),
        ("rotation_", 0, Ty::F32),
        ("visible_", 0, Ty::U8),
        ("contentSize_", 0, Ty::F32),
        ("contentSize_", 4, Ty::F32),
    ];
    for (ivar, offset, ty) in node_fields {
        columns.push(field(m, node, ivar, offset, ty));
    }

    let dashes = |columns: &mut Vec<String>, n: usize| {
        columns.extend(std::iter::repeat_n("-".to_string(), n));
    };
    let has = |classes: &[&str]| class.as_ref().is_some_and(|c| c.is_any_of(classes));

    if has(&COLOR_CLASSES) {
        columns.push(field(m, node, "opacity_", 0, Ty::U8));
        match set_color_ivar(m, node) {
            Ok(ivar) => {
                for i in 0..3 {
                    columns.push(field(m, node, ivar, i, Ty::U8));
                }
            }
            Err(_) => dashes(&mut columns, 3),
        }
        columns.push(field(m, node, "opacityModifyRGB_", 0, Ty::U8));
    } else {
        dashes(&mut columns, 5);
    }

    if has(&["CCSprite"]) {
        columns.push(field(m, node, "flipX_", 0, Ty::U8));
        columns.push(field(m, node, "flipY_", 0, Ty::U8));
        for i in 0..4 {
            columns.push(field(m, node, "rect_", 4 * i, Ty::F32));
        }
        columns.push(field(m, node, "usesBatchNode_", 0, Ty::U8));
    } else {
        dashes(&mut columns, 7);
    }

    match class.as_ref().and_then(|c| node_texture(m, node, c)) {
        Some(texture) => columns.extend(texture_columns(m, texture, files)),
        None => dashes(&mut columns, 10),
    }

    if has(&LABEL_CLASSES) {
        columns.push(text_field(m, node, "string_"));
    } else {
        dashes(&mut columns, 1);
    }
    if has(&["CCLabelTTF"]) {
        columns.push(text_field(m, node, "fontName_"));
        columns.push(field(m, node, "fontSize_", 0, Ty::F32));
        columns.push(field(m, node, "dimensions_", 0, Ty::F32));
        columns.push(field(m, node, "dimensions_", 4, Ty::F32));
    } else {
        dashes(&mut columns, 4);
    }

    debug_assert_eq!(columns.len(), COLUMNS.len());
    let mut line = columns.join("\t");
    line.push('\n');
    line
}

/// The objects in the `ccArray` at `data`: `{ num: u32, max: u32, arr:
/// *id }` (cocos2d 0.99.5's `ccCArray.h`; `CCArray.c` in the decompile
/// reads `num` at +0 and `arr` at +8).
pub(super) fn ccarray_items<M: SceneMemory + ?Sized>(m: &M, data: u32) -> Result<Vec<u32>, String> {
    if data == 0 {
        return Err("its ccArray pointer is NULL".to_string());
    }
    let num = m.word(data)?;
    let max = m.word(data.checked_add(4).ok_or("past the end")?)?;
    let arr = m.word(data.checked_add(8).ok_or("past the end")?)?;
    if num > max {
        return Err(format!(
            "its ccArray at {data:#x} has num {num} > max {max}"
        ));
    }
    if num == 0 {
        return Ok(Vec::new());
    }
    // A sanity bound: the walk stops at MAX_NODES anyway.
    if num as usize > 16 * MAX_NODES {
        return Err(format!("its ccArray at {data:#x} has num {num}"));
    }
    let bytes = m.bytes(arr, num * 4)?;
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| u32::from_le_bytes(b))
        .collect())
}

/// The children of `node`, in the order of its `children_` `CCArray`.
/// Empty if `children_` is nil (cocos2d makes it at the first `addChild:`).
fn children_of<M: SceneMemory + ?Sized>(m: &mut M, node: u32) -> Result<Vec<u32>, String> {
    let array = pointer(m, node, "children_")?;
    if array == 0 {
        return Ok(Vec::new());
    }
    let class = m.class_of(array)?;
    if !class.is_a("CCArray") {
        return Err(format!("children_ is a {}, not a CCArray", class.name));
    }
    let data = pointer(m, array, "data")?;
    ccarray_items(m, data)
}

/// What a walk wrote.
#[derive(Debug, Default)]
pub(super) struct Walk {
    /// The node lines.
    pub(super) lines: String,
    /// How many node lines.
    pub(super) nodes: usize,
    /// What was left out, and what could not be read, one note each.
    pub(super) notes: Vec<String>,
}

impl Walk {
    /// A note, unless [MAX_NOTES] were written already.
    fn note(&mut self, note: String) {
        if self.notes.len() < MAX_NOTES {
            self.notes.push(note);
        } else if self.notes.len() == MAX_NOTES {
            self.notes.push("more notes left out".to_string());
        }
    }

    /// A note that a cap cut the walk: always written, after the others
    /// (it is only made once the walk is over or stopped).
    fn cut(&mut self, note: String) {
        self.notes.push(note);
    }
}

/// Walk the nodes from `root` (depth 0) depth-first, each node before its
/// children, the children in their array's order, and write a line per
/// node (see [node_line]). Stops after [MAX_NODES] lines, does not go
/// [MAX_DEPTH] levels deep, and writes a node seen before (a loop) once.
pub(super) fn walk<M: SceneMemory + ?Sized>(
    m: &mut M,
    root: u32,
    files: &HashMap<u32, String>,
) -> Walk {
    let mut walk = Walk::default();
    let mut seen = HashSet::new();
    let mut too_deep = 0usize;
    // Children are pushed last first, so they come off in order.
    let mut stack = vec![(root, 0usize)];
    while let Some((node, depth)) = stack.pop() {
        if walk.nodes == MAX_NODES {
            walk.cut(format!(
                "cut: the cap of {MAX_NODES} nodes was reached; {} more on the walk's stack (and their children) are not written",
                stack.len() + 1
            ));
            break;
        }
        if !seen.insert(node) {
            walk.note(format!("{node:#010x} was met again (a loop), written once"));
            continue;
        }
        walk.lines.push_str(&node_line(m, node, depth, files));
        walk.nodes += 1;
        match children_of(m, node) {
            Ok(children) if depth + 1 >= MAX_DEPTH => too_deep += children.len(),
            Ok(children) => {
                for &child in children.iter().rev() {
                    if child == 0 {
                        walk.note(format!("{node:#010x} has a nil child"));
                    } else {
                        stack.push((child, depth + 1));
                    }
                }
            }
            Err(why) => walk.note(format!("children of {node:#010x} not read: {why}")),
        }
    }
    if too_deep > 0 {
        walk.cut(format!(
            "cut: {too_deep} node(s) {MAX_DEPTH} levels deep are not written, nor their children"
        ));
    }
    walk
}

/// The first line of a dump: the column names.
pub(super) fn column_line() -> String {
    format!("{}\n", COLUMNS.join("\t"))
}

/// What the second line of a dump says.
pub(super) struct DumpInfo<'a> {
    pub(super) frame: u32,
    pub(super) ms: u64,
    pub(super) trigger: &'a str,
    /// The window's viewport: x, y, width, height in pixels.
    pub(super) viewport: Option<(u32, u32, u32, u32)>,
    /// How many textures the texture cache holds, if it was found.
    pub(super) textures: Option<usize>,
}

/// The second line of a dump: `#`, then tab-separated `key=value` pairs:
/// the frame, ms and trigger, the director (address and class), its
/// `isPaused_`, `runningScene_` and `nextScene_` (address and class, or
/// `nil`), `winSizeInPoints_`, the window's viewport and the number of
/// textures in the texture cache; `-` for what cannot be read.
pub(super) fn info_line<M: SceneMemory + ?Sized>(
    m: &mut M,
    info: &DumpInfo,
    director: Option<u32>,
) -> String {
    let object = |m: &mut M, object: Result<u32, String>| match object {
        Ok(0) => "nil".to_string(),
        Ok(object) => format!(
            "{object:#010x}:{}",
            m.class_of(object)
                .map_or_else(|_| "?".to_string(), |c| c.name.clone())
        ),
        Err(_) => "-".to_string(),
    };
    let (director_text, paused, running, next, win_w, win_h) = match director {
        Some(d) => (
            object(m, Ok(d)),
            field(m, d, "isPaused_", 0, Ty::U8),
            {
                let p = pointer(m, d, "runningScene_");
                object(m, p)
            },
            {
                let p = pointer(m, d, "nextScene_");
                object(m, p)
            },
            field(m, d, "winSizeInPoints_", 0, Ty::F32),
            field(m, d, "winSizeInPoints_", 4, Ty::F32),
        ),
        None => (
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
        ),
    };
    let viewport = info.viewport.map_or_else(
        || "-".to_string(),
        |(x, y, w, h)| format!("{x},{y},{w},{h}"),
    );
    let textures = info
        .textures
        .map_or_else(|| "-".to_string(), |n| n.to_string());
    let trigger = info.trigger.replace(['\t', '\r', '\n'], " ");
    format!(
        "# frame={}\tms={}\ttrigger={}\tdirector={}\tpaused={}\trunning_scene={}\tnext_scene={}\twin_size={},{}\tviewport={}\ttextures={}\n",
        info.frame, info.ms, trigger, director_text, paused, running, next, win_w, win_h, viewport, textures
    )
}

/// A whole dump: the column line, the info line, the node lines from the
/// director's `runningScene_`, and the notes, each as a `#` line, last a
/// `# nodes=N` line.
pub(super) fn dump_text<M: SceneMemory + ?Sized>(
    m: &mut M,
    info: &DumpInfo,
    director: Option<u32>,
    files: &HashMap<u32, String>,
) -> String {
    let mut text = column_line();
    text.push_str(&info_line(m, info, director));
    let scene = match director {
        Some(d) => pointer(m, d, "runningScene_")
            .map_err(|why| format!("the director's runningScene_ not read: {why}")),
        None => Err("no CCDirector found, nothing walked".to_string()),
    };
    let walk = match scene {
        Ok(0) => {
            let mut walk = Walk::default();
            walk.note("the director has no running scene".to_string());
            walk
        }
        Ok(scene) => walk(m, scene, files),
        Err(why) => {
            let mut walk = Walk::default();
            walk.note(why);
            walk
        }
    };
    text.push_str(&walk.lines);
    for note in &walk.notes {
        text.push_str(&format!("# {}\n", note.replace(['\t', '\r', '\n'], " ")));
    }
    text.push_str(&format!("# nodes={}\n", walk.nodes));
    text
}

/// [SceneMemory] over the real guest memory and class table.
pub(super) struct GuestScene<'a> {
    objc: &'a ObjC,
    mem: &'a Mem,
    /// Per isa: what is known of the class, or [None] if the word is not a
    /// class touchHLE knows.
    classes: HashMap<u32, Option<Rc<ClassInfo>>>,
    /// Per isa and ivar name: the offset, or [None] if the chain has no
    /// such ivar.
    offsets: HashMap<(u32, &'static str), Option<u32>>,
}

impl<'a> GuestScene<'a> {
    pub(super) fn new(objc: &'a ObjC, mem: &'a Mem) -> Self {
        GuestScene {
            objc,
            mem,
            classes: HashMap::new(),
            offsets: HashMap::new(),
        }
    }
}

impl SceneMemory for GuestScene<'_> {
    fn bytes(&self, addr: u32, count: u32) -> Result<&[u8], String> {
        read_guest_bytes(self.mem, addr, count)
    }

    fn class_of(&mut self, object: u32) -> Result<Rc<ClassInfo>, String> {
        if object == 0 {
            return Err("nil".to_string());
        }
        let isa = self.word(object)?;
        let objc = self.objc;
        let known = self.classes.entry(isa).or_insert_with(|| {
            let class: Class = Ptr::from_bits(isa);
            objc.get_host_object(class)
                .and_then(|o| o.as_any().downcast_ref::<ClassHostObject>())
                .map(|_| {
                    Rc::new(ClassInfo {
                        name: class_name_for_trace(objc, class),
                        chain: class_chain_names(objc, class),
                    })
                })
        });
        known
            .clone()
            .ok_or_else(|| format!("{object:#x} has no class we know (isa {isa:#x})"))
    }

    fn ivar_addr(&mut self, object: u32, name: &'static str) -> Result<u32, String> {
        if object == 0 {
            return Err(format!("nil before {name}"));
        }
        let isa = self.word(object)?;
        let offset = match self.offsets.get(&(isa, name)) {
            Some(&offset) => offset,
            None => {
                let offset = ivar_offset(self.objc, self.mem, Ptr::from_bits(isa), name)
                    .ok_or_else(|| format!("{object:#x} has no class we know (isa {isa:#x})"))?;
                self.offsets.insert((isa, name), offset);
                offset
            }
        }
        .ok_or_else(|| format!("{object:#x} has no ivar {name}"))?;
        object
            .checked_add(offset)
            .ok_or_else(|| format!("{object:#x} + {offset:#x} is past the end"))
    }

    fn string_text(&self, string: u32) -> Option<String> {
        crate::frameworks::foundation::ns_string::host_string_text(
            self.objc,
            Ptr::from_bits(string),
            STR_MAX_BYTES + 1,
        )
        .map(|text| text.into_owned())
    }
}

/// Whether `object` is an instance (not a class) of `class` or a subclass.
fn is_instance_of<M: SceneMemory + ?Sized>(m: &mut M, object: u32, class: &str) -> bool {
    object != 0
        && m.class_of(object)
            .is_ok_and(|c| !c.name.starts_with('+') && c.is_a(class))
}

/// The data sections of the app binary searched for singletons.
const DATA_SECTIONS: [&str; 3] = ["__data", "__bss", "__common"];

/// Find the instance of `class` that a static variable of the app binary
/// points at: cocos2d keeps its singletons (`CCDirector`,
/// `CCTextureCache`) in file-static variables, which are not exported.
/// `slot` is the variable's address found last time: if it still points at
/// such an instance, that is used; otherwise the binary's `__data`, `__bss`
/// and `__common` sections are searched, a word at a time, for the first
/// word that does, and `slot` is set to it. A singleton is one object, so
/// any word pointing at an instance of its class points at it. [None] if
/// there is none (yet). Only reads memory and the class table.
pub(super) fn find_singleton<M: SceneMemory + ?Sized>(
    m: &mut M,
    bin: &MachO,
    slot: &mut Option<u32>,
    class: &str,
) -> Option<u32> {
    if let Some(addr) = *slot {
        if let Ok(object) = m.word(addr) {
            if is_instance_of(m, object, class) {
                return Some(object);
            }
        }
    }
    for section in &bin.sections {
        if !DATA_SECTIONS.contains(&section.name.as_str()) {
            continue;
        }
        let Ok(bytes) = m.bytes(section.addr, section.size) else {
            continue;
        };
        let words: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| u32::from_le_bytes(b))
            .collect();
        for (i, &word) in words.iter().enumerate() {
            if is_instance_of(m, word, class) {
                *slot = Some(section.addr + 4 * i as u32);
                return Some(word);
            }
        }
    }
    None
}

/// The texture cache's textures, as a map from the `CCTexture2D`'s address
/// to the key it was cached under (the file name the game asked for): the
/// keys and values of its `textures_`, an `NSMutableDictionary` touchHLE
/// made, read from its host object without calling the guest. [None] if
/// `textures_` cannot be read or is not one of touchHLE's dictionaries.
pub(super) fn texture_files(
    scene: &mut GuestScene,
    texture_cache: u32,
) -> Option<HashMap<u32, String>> {
    let dict = pointer(scene, texture_cache, "textures_").ok()?;
    let pairs = crate::frameworks::foundation::ns_dictionary::host_dictionary_pairs(
        scene.objc,
        Ptr::from_bits(dict),
    )?;
    Some(
        pairs
            .into_iter()
            .map(|(key, value)| {
                let file = scene
                    .string_text(key.to_bits())
                    .unwrap_or_else(|| format!("{:#010x}", key.to_bits()));
                (value.to_bits(), file)
            })
            .collect(),
    )
}

/// The file name of a dump: `s{frame:06}_{ms}.txt`, the frame and whole
/// milliseconds of the picture saved with it (`frames/f{frame:06}_{ms}.png`).
pub(super) fn scene_file_name(frame: u32, ms: u64) -> String {
    format!("s{frame:06}_{ms}.txt")
}

/// Whether `name` looks like one of [scene_file_name]'s.
fn is_scene_file_name(name: &str) -> bool {
    let Some(stem) = name
        .strip_prefix('s')
        .and_then(|rest| rest.strip_suffix(".txt"))
    else {
        return false;
    };
    let Some((frame, ms)) = stem.split_once('_') else {
        return false;
    };
    let is_number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    frame.len() >= 6 && is_number(frame) && is_number(ms)
}

/// Create `dir` and delete the dumps an earlier run left there, so the
/// folder matches this run's `trace.log`. Only files named like
/// [scene_file_name] are removed.
pub(super) fn prepare_dir(dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("could not create {}: {}", dir.display(), e))?;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if entry.file_name().to_str().is_some_and(is_scene_file_name)
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    if removed > 0 {
        log!(
            "Scene dump: removed {} dump(s) left from an earlier run in {}",
            removed,
            dir.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up guest: memory from address 0x1000 up, a class per object,
    /// and ivar offsets per class name.
    struct FakeGuest {
        memory: Vec<u8>,
        classes: HashMap<u32, Rc<ClassInfo>>,
        /// Per class name, per ivar name.
        offsets: HashMap<String, HashMap<&'static str, u32>>,
        strings: HashMap<u32, String>,
    }

    const BASE: u32 = 0x1000;

    impl FakeGuest {
        fn new() -> Self {
            let mut offsets: HashMap<String, HashMap<&'static str, u32>> = HashMap::new();
            // Offsets as in a made-up layout; only the names matter.
            for (class, ivar, offset) in [
                ("CCNode", "tag_", 4),
                ("CCNode", "zOrder_", 8),
                ("CCNode", "position_", 12),
                ("CCNode", "anchorPoint_", 20),
                ("CCNode", "isRelativeAnchorPoint_", 28),
                ("CCNode", "scaleX_", 32),
                ("CCNode", "scaleY_", 36),
                ("CCNode", "rotation_", 40),
                ("CCNode", "visible_", 44),
                ("CCNode", "contentSize_", 48),
                ("CCNode", "children_", 56),
                ("CCSprite", "opacity_", 60),
                ("CCSprite", "color_", 61),
                ("CCSprite", "flipX_", 64),
                ("CCSprite", "flipY_", 65),
                ("CCSprite", "rect_", 68),
                ("CCSprite", "usesBatchNode_", 84),
                ("CCSprite", "texture_", 88),
                ("CCSprite", "textureAtlas_", 92),
                ("CCLabelTTF", "string_", 96),
                ("CCLabelTTF", "fontName_", 100),
                ("CCLabelTTF", "fontSize_", 104),
                ("CCLabelTTF", "dimensions_", 108),
                ("CCSprite", "colorUnmodified_", 120),
                ("CCSprite", "opacityModifyRGB_", 124),
                ("CCArray", "data", 4),
                ("CCTexture2D", "name_", 4),
                ("CCTexture2D", "size_", 8),
                ("CCTexture2D", "width_", 16),
                ("CCTexture2D", "height_", 20),
                ("CCTexture2D", "format_", 24),
                ("CCTexture2D", "maxS_", 28),
                ("CCTexture2D", "maxT_", 32),
                ("CCDirector", "isPaused_", 4),
                ("CCDirector", "runningScene_", 8),
                ("CCDirector", "nextScene_", 12),
                ("CCDirector", "winSizeInPoints_", 16),
            ] {
                offsets
                    .entry(class.to_string())
                    .or_default()
                    .insert(ivar, offset);
            }
            FakeGuest {
                memory: vec![0; 0x40000],
                classes: HashMap::new(),
                offsets,
                strings: HashMap::new(),
            }
        }

        fn put(&mut self, addr: u32, bytes: &[u8]) {
            let at = (addr - BASE) as usize;
            self.memory[at..at + bytes.len()].copy_from_slice(bytes);
        }

        fn put_word(&mut self, addr: u32, word: u32) {
            self.put(addr, &word.to_le_bytes());
        }

        fn put_f32(&mut self, addr: u32, value: f32) {
            self.put(addr, &value.to_bits().to_le_bytes());
        }

        /// Make `addr` an object of a class with the chain `chain` (its
        /// isa word is not used).
        fn object(&mut self, addr: u32, chain: &[&str]) {
            self.classes.insert(
                addr,
                Rc::new(ClassInfo {
                    name: chain[0].to_string(),
                    chain: chain.iter().map(|s| s.to_string()).collect(),
                }),
            );
        }

        /// The offset of `name` on the chain of `class`'s names.
        fn offset(&self, chain: &[String], name: &str) -> Option<u32> {
            chain
                .iter()
                .find_map(|c| self.offsets.get(c).and_then(|ivars| ivars.get(name)))
                .copied()
        }

        fn ivar(&self, object: u32, name: &str) -> u32 {
            object + self.offset(&self.classes[&object].chain, name).unwrap()
        }

        /// A CCArray at `array` (its ccArray struct at `array + 0x20`, its
        /// items at `array + 0x40`) holding `items`.
        fn array(&mut self, array: u32, items: &[u32]) {
            self.object(array, &["CCArray", "NSObject"]);
            let data = array + 0x20;
            let arr = array + 0x40;
            self.put_word(self.ivar(array, "data"), data);
            self.put_word(data, items.len() as u32);
            self.put_word(data + 4, items.len() as u32 + 3);
            self.put_word(data + 8, arr);
            for (i, &item) in items.iter().enumerate() {
                self.put_word(arr + 4 * i as u32, item);
            }
        }

        /// A node of `chain` at `node`, with `children` (in a CCArray at
        /// `node + 0x80`, if any).
        fn node(&mut self, node: u32, chain: &[&str], children: &[u32]) {
            self.object(node, chain);
            if !children.is_empty() {
                let array = node + 0x80;
                self.array(array, children);
                self.put_word(self.ivar(node, "children_"), array);
            }
        }
    }

    impl SceneMemory for FakeGuest {
        fn bytes(&self, addr: u32, count: u32) -> Result<&[u8], String> {
            let at = addr.checked_sub(BASE).ok_or("null page")? as usize;
            self.memory
                .get(at..)
                .and_then(|m| m.get(..count as usize))
                .ok_or_else(|| format!("{count} bytes at {addr:#x} are not readable"))
        }

        fn class_of(&mut self, object: u32) -> Result<Rc<ClassInfo>, String> {
            self.classes
                .get(&object)
                .cloned()
                .ok_or_else(|| format!("{object:#x} has no class"))
        }

        fn ivar_addr(&mut self, object: u32, name: &'static str) -> Result<u32, String> {
            let class = self.class_of(object)?;
            self.offset(&class.chain, name)
                .map(|offset| object + offset)
                .ok_or_else(|| format!("no ivar {name}"))
        }

        fn string_text(&self, string: u32) -> Option<String> {
            self.strings.get(&string).cloned()
        }
    }

    const SPRITE: [&str; 3] = ["CCSprite", "CCNode", "NSObject"];
    const LABEL: [&str; 4] = ["CCLabelTTF", "CCSprite", "CCNode", "NSObject"];
    const LAYER: [&str; 3] = ["CCLayer", "CCNode", "NSObject"];

    fn columns(line: &str) -> Vec<&str> {
        line.trim_end_matches('\n').split('\t').collect()
    }

    #[test]
    fn ccarray_items_reads_num_and_arr() {
        let mut g = FakeGuest::new();
        g.array(0x2000, &[0x3000, 0x3100, 0x3200]);
        assert_eq!(ccarray_items(&g, 0x2020), Ok(vec![0x3000, 0x3100, 0x3200]));
        // Empty.
        g.put_word(0x2500, 0);
        g.put_word(0x2504, 4);
        assert_eq!(ccarray_items(&g, 0x2500), Ok(vec![]));
        // num > max, a NULL struct, and an arr past the end of memory.
        g.put_word(0x2600, 5);
        g.put_word(0x2604, 4);
        assert!(ccarray_items(&g, 0x2600).unwrap_err().contains("max"));
        assert!(ccarray_items(&g, 0).is_err());
        g.put_word(0x2700, 2);
        g.put_word(0x2704, 2);
        g.put_word(0x2708, 0xffff_fff0);
        assert!(ccarray_items(&g, 0x2700).is_err());
        assert!(ccarray_items(&g, 0x0800).is_err());
    }

    #[test]
    fn the_column_line_names_every_column() {
        let line = column_line();
        assert!(line.starts_with("class\tdepth\taddr\ttag\tz\tx\ty\t"));
        assert!(line.ends_with("\tdim_w\tdim_h\n"));
        assert_eq!(columns(&line).len(), 43);
    }

    #[test]
    fn a_scene_is_walked_in_order_with_its_fields() {
        let mut g = FakeGuest::new();
        // scene -> layer -> (sprite, label); the layer has a second child
        // array slot that is nil.
        g.node(0x4000, &["CCScene", "CCNode", "NSObject"], &[0x5000]);
        g.node(0x5000, &LAYER, &[0x6000, 0x7000]);
        g.node(0x6000, &SPRITE, &[]);
        g.node(0x7000, &LABEL, &[]);
        // The sprite.
        g.put_word(g.ivar(0x6000, "tag_"), 7);
        g.put_word(g.ivar(0x6000, "zOrder_"), (-2i32) as u32);
        g.put_f32(g.ivar(0x6000, "position_"), 160.0);
        g.put_f32(g.ivar(0x6000, "position_") + 4, 452.5);
        g.put_f32(g.ivar(0x6000, "anchorPoint_"), 0.5);
        g.put_f32(g.ivar(0x6000, "anchorPoint_") + 4, 0.5);
        g.put(g.ivar(0x6000, "isRelativeAnchorPoint_"), &[1]);
        g.put_f32(g.ivar(0x6000, "scaleX_"), 1.0);
        g.put_f32(g.ivar(0x6000, "scaleY_"), 1.0);
        g.put(g.ivar(0x6000, "visible_"), &[1]);
        g.put_f32(g.ivar(0x6000, "contentSize_"), 64.0);
        g.put_f32(g.ivar(0x6000, "contentSize_") + 4, 32.0);
        // Opacity 128 with opacityModifyRGB_ on: color_ is the colour set
        // times 128/255, and the colour set is in colorUnmodified_.
        g.put(g.ivar(0x6000, "opacity_"), &[128]);
        g.put(g.ivar(0x6000, "color_"), &[128, 64, 0]);
        g.put(g.ivar(0x6000, "colorUnmodified_"), &[255, 128, 0]);
        g.put(g.ivar(0x6000, "opacityModifyRGB_"), &[1]);
        g.put_f32(g.ivar(0x6000, "rect_"), 0.0);
        g.put_f32(g.ivar(0x6000, "rect_") + 4, 32.0);
        g.put_f32(g.ivar(0x6000, "rect_") + 8, 64.0);
        g.put_f32(g.ivar(0x6000, "rect_") + 12, 32.0);
        g.put_word(g.ivar(0x6000, "texture_"), 0x8000);
        // Its texture.
        g.object(0x8000, &["CCTexture2D", "NSObject"]);
        g.put_word(g.ivar(0x8000, "name_"), 12);
        g.put_f32(g.ivar(0x8000, "size_"), 100.0);
        g.put_f32(g.ivar(0x8000, "size_") + 4, 50.0);
        g.put_word(g.ivar(0x8000, "width_"), 128);
        g.put_word(g.ivar(0x8000, "height_"), 64);
        g.put_word(g.ivar(0x8000, "format_"), 0);
        g.put_f32(g.ivar(0x8000, "maxS_"), 0.78125);
        g.put_f32(g.ivar(0x8000, "maxT_"), 0.78125);
        // The label: a text with a tab, no texture yet.
        g.strings.insert(0x9000, "Gold:\t120".to_string());
        g.strings.insert(0x9100, "Futura".to_string());
        g.put_word(g.ivar(0x7000, "string_"), 0x9000);
        g.put_word(g.ivar(0x7000, "fontName_"), 0x9100);
        g.put_f32(g.ivar(0x7000, "fontSize_"), 14.0);

        let files = HashMap::from([(0x8000, "hud.png".to_string())]);
        let walked = walk(&mut g, 0x4000, &files);
        assert_eq!(walked.nodes, 4);
        assert!(walked.notes.is_empty(), "{:?}", walked.notes);
        let lines: Vec<&str> = walked.lines.lines().collect();
        assert_eq!(lines.len(), 4);
        for line in &lines {
            assert_eq!(columns(line).len(), COLUMNS.len(), "{line}");
        }
        let scene = columns(lines[0]);
        assert_eq!(&scene[..3], ["CCScene", "0", "0x00004000"]);
        // A plain node: '-' from opacity on.
        assert!(scene[16..].iter().all(|&c| c == "-"));
        let layer = columns(lines[1]);
        assert_eq!(&layer[..3], ["  CCLayer", "1", "0x00005000"]);
        let sprite = columns(lines[2]);
        assert_eq!(
            sprite,
            [
                "    CCSprite",
                "2",
                "0x00006000",
                "7",
                "-2",
                "160",
                "452.5",
                "0.5",
                "0.5",
                "1",
                "1",
                "1",
                "0",
                "1",
                "64",
                "32",
                "128",
                "255",
                "128",
                "0",
                "1",
                "0",
                "0",
                "0",
                "32",
                "64",
                "32",
                "0",
                "0x00008000",
                "12",
                "128",
                "64",
                "100",
                "50",
                "0",
                "0.78125",
                "0.78125",
                "hud.png",
                "-",
                "-",
                "-",
                "-",
                "-",
            ]
        );
        let label = columns(lines[3]);
        assert_eq!(label[0], "    CCLabelTTF");
        // No texture: nil, and '-' for the rest of the texture columns.
        assert_eq!(label[28], "nil");
        assert!(label[29..38].iter().all(|&c| c == "-"));
        // The text escaped as a str peek writes it.
        assert_eq!(&label[38..], ["Gold:\\t120", "Futura", "14", "0", "0"]);
    }

    #[test]
    fn a_batch_sprite_takes_the_batch_texture() {
        let mut g = FakeGuest::new();
        g.offsets
            .entry("CCTextureAtlas".to_string())
            .or_default()
            .insert("texture_", 4);
        g.node(0x4000, &SPRITE, &[]);
        g.put(g.ivar(0x4000, "usesBatchNode_"), &[1]);
        g.put_word(g.ivar(0x4000, "texture_"), 0x8000);
        g.put_word(g.ivar(0x4000, "textureAtlas_"), 0x5000);
        g.object(0x5000, &["CCTextureAtlas", "NSObject"]);
        g.object(0x8000, &["CCTexture2D", "NSObject"]);
        g.object(0x8800, &["CCTexture2D", "NSObject"]);
        g.put_word(0x5004, 0x8800);
        let line = node_line(&mut g, 0x4000, 0, &HashMap::new());
        assert_eq!(columns(&line)[27], "1");
        assert_eq!(columns(&line)[28], "0x00008800");
        // Without the atlas, the sprite's own texture_.
        g.put_word(g.ivar(0x4000, "textureAtlas_"), 0);
        let line = node_line(&mut g, 0x4000, 0, &HashMap::new());
        assert_eq!(columns(&line)[28], "0x00008000");
    }

    #[test]
    fn the_colour_is_the_one_the_game_set() {
        let rgb = |line: &str| -> Vec<String> {
            columns(line)[16..21]
                .iter()
                .map(|c| c.to_string())
                .collect()
        };
        let mut g = FakeGuest::new();
        g.node(0x4000, &SPRITE, &[]);
        g.put(g.ivar(0x4000, "opacity_"), &[128]);
        g.put(g.ivar(0x4000, "color_"), &[128, 128, 128]);
        g.put(g.ivar(0x4000, "colorUnmodified_"), &[255, 255, 255]);
        // opacityModifyRGB_ on: what [sprite color] returns, not color_.
        g.put(g.ivar(0x4000, "opacityModifyRGB_"), &[1]);
        let line = node_line(&mut g, 0x4000, 0, &HashMap::new());
        assert_eq!(rgb(&line), ["128", "255", "255", "255", "1"]);
        // Off: color_ is the colour set.
        g.put(g.ivar(0x4000, "opacityModifyRGB_"), &[0]);
        let line = node_line(&mut g, 0x4000, 0, &HashMap::new());
        assert_eq!(rgb(&line), ["128", "128", "128", "128", "0"]);
        // A class with color_ only (CCLayerColor): color_, and '-' for
        // opacityModifyRGB_.
        g.offsets.insert(
            "CCLayerColor".to_string(),
            HashMap::from([("opacity_", 60), ("color_", 61)]),
        );
        g.node(0x5000, &["CCLayerColor", "CCLayer", "CCNode"], &[]);
        g.put(g.ivar(0x5000, "opacity_"), &[200]);
        g.put(g.ivar(0x5000, "color_"), &[10, 20, 30]);
        let line = node_line(&mut g, 0x5000, 0, &HashMap::new());
        assert_eq!(rgb(&line), ["200", "10", "20", "30", "-"]);
    }

    #[test]
    fn the_walk_stops_at_the_caps_and_loops() {
        // Wider than the node cap.
        let mut g = FakeGuest::new();
        let children: Vec<u32> = (0..MAX_NODES as u32 + 10)
            .map(|i| 0x1_0000 + 4 * i)
            .collect();
        g.node(0x4000, &LAYER, &[]);
        g.array(0x5000, &children);
        g.put_word(g.ivar(0x4000, "children_"), 0x5000);
        for &child in &children {
            g.object(child, &["CCNode", "NSObject"]);
        }
        let walked = walk(&mut g, 0x4000, &HashMap::new());
        assert_eq!(walked.nodes, MAX_NODES);
        assert_eq!(walked.lines.lines().count(), MAX_NODES);
        assert_eq!(walked.notes.len(), 1);
        assert!(walked.notes[0].starts_with("cut: the cap of 5000 nodes"));
        assert!(walked.notes[0].contains("11 more"), "{}", walked.notes[0]);

        // Deeper than the depth cap: a chain of nodes, each the only child
        // of the one before.
        let mut g = FakeGuest::new();
        let chain: Vec<u32> = (0..MAX_DEPTH as u32 + 5)
            .map(|i| 0x4000 + 0x100 * i)
            .collect();
        for pair in chain.windows(2) {
            g.node(pair[0], &LAYER, &[pair[1]]);
        }
        g.node(*chain.last().unwrap(), &LAYER, &[]);
        let walked = walk(&mut g, chain[0], &HashMap::new());
        assert_eq!(walked.nodes, MAX_DEPTH);
        let last = walked.lines.lines().last().unwrap();
        assert_eq!(columns(last)[1], (MAX_DEPTH - 1).to_string());
        assert_eq!(walked.notes.len(), 1);
        assert!(walked.notes[0].starts_with("cut: 1 node(s) 32 levels deep"));

        // A node that is its own child is written once.
        let mut g = FakeGuest::new();
        g.node(0x4000, &LAYER, &[0x4000, 0x6000]);
        g.node(0x6000, &LAYER, &[]);
        let walked = walk(&mut g, 0x4000, &HashMap::new());
        assert_eq!(walked.nodes, 2);
        assert_eq!(walked.notes.len(), 1);
        assert!(walked.notes[0].contains("a loop"));
    }

    #[test]
    fn a_cut_note_is_written_past_the_note_cap() {
        let nils = vec![0u32; MAX_NOTES + 10];
        // The node cap: nil children, then more nodes than the cap.
        let mut g = FakeGuest::new();
        let mut children = nils.clone();
        children.extend((0..MAX_NODES as u32 + 10).map(|i| 0x1_0000 + 4 * i));
        g.node(0x4000, &LAYER, &[]);
        g.array(0x5000, &children);
        g.put_word(g.ivar(0x4000, "children_"), 0x5000);
        for &child in &children[nils.len()..] {
            g.object(child, &["CCNode", "NSObject"]);
        }
        let walked = walk(&mut g, 0x4000, &HashMap::new());
        assert_eq!(walked.nodes, MAX_NODES);
        assert_eq!(walked.notes.len(), MAX_NOTES + 2);
        assert_eq!(walked.notes[MAX_NOTES], "more notes left out");
        let last = walked.notes.last().unwrap();
        assert!(last.starts_with("cut: the cap of 5000 nodes"), "{last}");

        // The depth cap: a chain too deep, its root with nil children.
        let mut g = FakeGuest::new();
        let chain: Vec<u32> = (0..MAX_DEPTH as u32 + 1)
            .map(|i| 0x4000 + 0x100 * i)
            .collect();
        let mut first = nils.clone();
        first.push(chain[1]);
        g.node(chain[0], &LAYER, &[]);
        g.array(0x8000, &first);
        g.put_word(g.ivar(chain[0], "children_"), 0x8000);
        for pair in chain[1..].windows(2) {
            g.node(pair[0], &LAYER, &[pair[1]]);
        }
        g.node(*chain.last().unwrap(), &LAYER, &[]);
        let walked = walk(&mut g, chain[0], &HashMap::new());
        assert_eq!(walked.nodes, MAX_DEPTH);
        assert_eq!(walked.notes.len(), MAX_NOTES + 2);
        let last = walked.notes.last().unwrap();
        assert!(last.starts_with("cut: 1 node(s) 32 levels deep"), "{last}");
    }

    #[test]
    fn unreadable_children_and_classes_give_dashes_and_notes() {
        let mut g = FakeGuest::new();
        // children_ points at something that is not a CCArray, and the
        // second node has no class at all.
        g.node(0x4000, &LAYER, &[]);
        g.object(0x5000, &["NSArray", "NSObject"]);
        g.put_word(g.ivar(0x4000, "children_"), 0x5000);
        let walked = walk(&mut g, 0x4000, &HashMap::new());
        assert_eq!(walked.nodes, 1);
        assert_eq!(
            walked.notes,
            ["children of 0x00004000 not read: children_ is a NSArray, not a CCArray"]
        );
        let line = node_line(&mut g, 0x0700, 3, &HashMap::new());
        let cols = columns(&line);
        assert_eq!(cols.len(), COLUMNS.len());
        assert_eq!(&cols[..3], ["      ?", "3", "0x00000700"]);
        assert!(cols[3..].iter().all(|&c| c == "-"));
    }

    #[test]
    fn a_dump_has_both_header_lines_and_a_count() {
        let mut g = FakeGuest::new();
        g.object(0x3000, &["CCDirectorTimer", "CCDirectorIOS", "CCDirector"]);
        g.put(g.ivar(0x3000, "isPaused_"), &[0]);
        g.put_word(g.ivar(0x3000, "runningScene_"), 0x4000);
        g.put_f32(g.ivar(0x3000, "winSizeInPoints_"), 320.0);
        g.put_f32(g.ivar(0x3000, "winSizeInPoints_") + 4, 480.0);
        g.node(0x4000, &["CCScene", "CCNode", "NSObject"], &[]);
        let info = DumpInfo {
            frame: 42,
            ms: 15873,
            trigger: "key F11",
            viewport: Some((0, 0, 320, 480)),
            textures: Some(12),
        };
        let text = dump_text(&mut g, &info, Some(0x3000), &HashMap::new());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], column_line().trim_end());
        assert_eq!(
            lines[1],
            "# frame=42\tms=15873\ttrigger=key F11\tdirector=0x00003000:CCDirectorTimer\tpaused=0\trunning_scene=0x00004000:CCScene\tnext_scene=nil\twin_size=320,480\tviewport=0,0,320,480\ttextures=12"
        );
        assert!(lines[2].starts_with("CCScene\t0\t0x00004000\t"));
        assert_eq!(lines[3], "# nodes=1");
        assert_eq!(lines.len(), 4);

        // Without a director: '-' and a note.
        let info = DumpInfo {
            viewport: None,
            textures: None,
            ..info
        };
        let text = dump_text(&mut g, &info, None, &HashMap::new());
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[1].contains("\tdirector=-\tpaused=-\trunning_scene=-\t"));
        assert!(lines[1].ends_with("\tviewport=-\ttextures=-"));
        assert_eq!(lines[2], "# no CCDirector found, nothing walked");
        assert_eq!(lines[3], "# nodes=0");
    }

    #[test]
    fn scene_file_names_line_up_with_the_frames() {
        assert_eq!(scene_file_name(42, 1234), "s000042_1234.txt");
        assert!(is_scene_file_name("s000042_1234.txt"));
        assert!(is_scene_file_name("s1234567_0.txt"));
        assert!(!is_scene_file_name("s42_1234.txt"));
        assert!(!is_scene_file_name("f000042_1234.png"));
        assert!(!is_scene_file_name("s000042_12.png"));
        assert!(!is_scene_file_name("notes.txt"));
    }
}
