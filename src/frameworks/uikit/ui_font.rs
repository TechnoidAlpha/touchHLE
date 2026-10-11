/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIFont`.

use super::ui_graphics::UIGraphicsGetCurrentContext;
use crate::font::{face_names, FaceNames, Font, TextAlignment, WrapMode};
use crate::frameworks::core_graphics::cg_bitmap_context::CGBitmapContextDrawer;
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string::to_rust_string;
use crate::frameworks::foundation::NSInteger;
use crate::fs::GuestPath;
use crate::objc::{autorelease, id, msg, objc_classes, ClassExports, HostObject};
use crate::Environment;
use std::collections::HashMap;
use std::ops::Range;

#[derive(Default)]
pub(super) struct State {
    fonts: HashMap<FontKind, Font>,
    sans_regular_ja: Option<Font>,
    sans_bold_ja: Option<Font>,
    /// Fonts loaded from the app bundle, see [find_bundle_font]. A
    /// [FontKind::Bundle] is an index into this.
    bundle_fonts: Vec<Font>,
    /// What each name passed to `fontWithName:size:` found in the bundle:
    /// an index into `bundle_fonts`, or [None] if nothing.
    bundle_font_for_name: HashMap<String, Option<usize>>,
    /// The bundle font file and face index behind each of `bundle_fonts`, so
    /// a face asked for under two names is loaded once.
    bundle_font_for_face: HashMap<(String, u32), usize>,
    /// The font files in the bundle's root and the names of their faces,
    /// read the first time a name has to be looked up that way.
    bundle_face_names: Option<Vec<(String, FaceNames)>>,
}
impl State {
    fn get_font_by_kind(&mut self, font_kind: FontKind) -> &Font {
        if let FontKind::Bundle(index) = font_kind {
            return &self.bundle_fonts[index];
        }
        self.fonts
            .entry(font_kind)
            .or_insert_with(|| match font_kind {
                FontKind::MonoRegular => Font::mono_regular(),
                FontKind::MonoBold => Font::mono_bold(),
                FontKind::MonoBoldItalic => Font::mono_bold_italic(),
                FontKind::MonoItalic => Font::mono_italic(),
                FontKind::SansRegular => Font::sans_regular(),
                FontKind::SansBold => Font::sans_bold(),
                FontKind::SansBoldItalic => Font::sans_bold_italic(),
                FontKind::SansItalic => Font::sans_italic(),
                FontKind::SerifRegular => Font::serif_regular(),
                FontKind::SerifBold => Font::serif_bold(),
                FontKind::SerifBoldItalic => Font::serif_bold_italic(),
                FontKind::SerifItalic => Font::serif_italic(),
                FontKind::Bundle(_) => unreachable!(),
            })
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
enum FontKind {
    MonoRegular,
    MonoBold,
    MonoBoldItalic,
    MonoItalic,
    SansRegular,
    SansBold,
    SansBoldItalic,
    SansItalic,
    SerifRegular,
    SerifBold,
    SerifBoldItalic,
    SerifItalic,
    /// A font file shipped in the app bundle, loaded by
    /// `fontWithName:size:`. The index is into [State]'s `bundle_fonts`.
    Bundle(usize),
}

struct UIFontHostObject {
    size: CGFloat,
    kind: FontKind,
}
impl HostObject for UIFontHostObject {}

/// Line break mode.
///
/// This is put here for convenience since it's font-related.
/// Apple puts it in its own header, also in UIKit.
pub type UILineBreakMode = NSInteger;
pub const UILineBreakModeWordWrap: UILineBreakMode = 0;
pub const UILineBreakModeCharacterWrap: UILineBreakMode = 1;
#[allow(dead_code)]
pub const UILineBreakModeClip: UILineBreakMode = 2;
#[allow(dead_code)]
pub const UILineBreakModeHeadTruncation: UILineBreakMode = 3;
pub const UILineBreakModeTailTruncation: UILineBreakMode = 4;
#[allow(dead_code)]
pub const UILineBreakModeMiddleTruncation: UILineBreakMode = 5;

/// Text alignment.
///
/// This is put here for convenience since it's font-related.
/// Apple puts it in its own header, also in UIKit.
pub type UITextAlignment = NSInteger;
pub const UITextAlignmentLeft: UITextAlignment = 0;
pub const UITextAlignmentCenter: UITextAlignment = 1;
pub const UITextAlignmentRight: UITextAlignment = 2;

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIFont: NSObject

// Values are checked against iPhone 3GS, iOS 4.0.1
+ (CGFloat)labelFontSize {
    17.0
}
+ (CGFloat)buttonFontSize {
    18.0
}
+ (CGFloat)smallSystemFontSize {
    12.0
}
+ (CGFloat)systemFontSize {
    14.0
}

+ (id)systemFontOfSize:(CGFloat)size {
    let host_object = UIFontHostObject {
        size,
        kind: FontKind::SansRegular,
    };
    let new = env.objc.alloc_object(this, Box::new(host_object), &mut env.mem);
    autorelease(env, new)
}
+ (id)boldSystemFontOfSize:(CGFloat)size {
    let host_object = UIFontHostObject {
        size,
        kind: FontKind::SansBold,
    };
    let new = env.objc.alloc_object(this, Box::new(host_object), &mut env.mem);
    autorelease(env, new)
}
+ (id)italicSystemFontOfSize:(CGFloat)size {
    let host_object = UIFontHostObject {
        size,
        kind: FontKind::SansItalic,
    };
    let new = env.objc.alloc_object(this, Box::new(host_object), &mut env.mem);
    autorelease(env, new)
}
+ (id)fontWithName:(id)fontName // NSString*
            size:(CGFloat)fontSize {
    let font_name = to_rust_string(env, fontName).to_string();
    let kind = get_equivalent_font(&font_name)
        .or_else(|| find_bundle_font(env, &font_name))
        .unwrap_or_else(|| {
            log!("No replacement found for font {}. Using system font instead.", font_name);
            FontKind::SansRegular
        });
    let host_object = UIFontHostObject {
        kind,
        size: fontSize,
    };
    let new = env.objc.alloc_object(this, Box::new(host_object), &mut env.mem);
    autorelease(env, new)
}

- (CGFloat)ascender {
    let host_object = env.objc.borrow::<UIFontHostObject>(this);
    let font = env.framework_state.uikit.ui_font.get_font_by_kind(host_object.kind);
    font.ascent(host_object.size)
}
- (CGFloat)descender {
    let host_object = env.objc.borrow::<UIFontHostObject>(this);
    let font = env.framework_state.uikit.ui_font.get_font_by_kind(host_object.kind);
    font.descent(host_object.size)
}
- (CGFloat)leading {
    let host_object = env.objc.borrow::<UIFontHostObject>(this);
    let font = env.framework_state.uikit.ui_font.get_font_by_kind(host_object.kind);
    font.line_gap(host_object.size)
}

- (CGFloat)lineHeight {
    // This is calculated based on the documentation:
    // https://developer.apple.com/library/archive/documentation/TextFonts/Conceptual/CocoaTextArchitecture/FontHandling/FontHandling.html
    let ascender: CGFloat = msg![env; this ascender];
    let descender: CGFloat = msg![env; this descender];
    let leading: CGFloat = msg![env; this leading];
    assert!(descender <= 0.0);
    ascender + leading - descender
}

@end

};

fn convert_line_break_mode(ui_mode: UILineBreakMode) -> WrapMode {
    match ui_mode {
        UILineBreakModeWordWrap => WrapMode::Word,
        UILineBreakModeCharacterWrap => WrapMode::Char,
        // TODO: support this properly; fake support is so that UILabel works,
        // which has this as its default line break mode
        UILineBreakModeTailTruncation => WrapMode::Word,
        _ => unimplemented!("TODO: line break mode {}", ui_mode),
    }
}

#[rustfmt::skip]
fn get_font<'a>(state: &'a mut State, kind: FontKind, text: &str) -> &'a Font {
    // The default fonts (see font.rs) are the Liberation family, which are a
    // good substitute for Helvetica, the iPhone OS system font. Unfortunately,
    // there is no CJK support in these fonts. To support Super Monkey Ball in
    // Japanese, let's fall back to Noto Sans JP when necessary.
    // FIXME: This heuristic is incomplete and a proper font fallback system
    // should be used instead.
    for c in text.chars() {
        let c = c as u32;
        if (0x3000..=0x30FF).contains(&c) || // JA punctuation, kana
           (0xFF00..=0xFFEF).contains(&c) || // full-width/half-width chars
           (0x4e00..=0x9FA0).contains(&c) || // various kanji
           (0x3400..=0x4DBF).contains(&c) { // more kanji
            match kind {
                // CJK has no italic equivalent
                FontKind::MonoRegular | FontKind::MonoItalic | FontKind::SansRegular | FontKind::SansItalic | FontKind::SerifRegular | FontKind::SerifItalic | FontKind::Bundle(_) => {
                    if state.sans_regular_ja.is_none() {
                        state.sans_regular_ja = Some(Font::sans_regular_ja());
                    }
                    return state.sans_regular_ja.as_ref().unwrap();
                },
                FontKind::MonoBold | FontKind::MonoBoldItalic | FontKind::SansBold | FontKind::SansBoldItalic | FontKind::SerifBold | FontKind::SerifBoldItalic => {
                    if state.sans_bold_ja.is_none() {
                        state.sans_bold_ja = Some(Font::sans_bold_ja());
                    }
                    return state.sans_bold_ja.as_ref().unwrap();
                },
            }
        }
    }

    state.get_font_by_kind(kind)
}

/// Called by the `sizeWithFont:` method family on `NSString`.
pub fn size_with_font(
    env: &mut Environment,
    font: id,
    text: &str,
    constrained: Option<(CGSize, UILineBreakMode)>,
) -> CGSize {
    let host_object = env.objc.borrow::<UIFontHostObject>(font);

    let font = get_font(
        &mut env.framework_state.uikit.ui_font,
        host_object.kind,
        text,
    );

    let wrap = constrained.map(|(size, ui_mode)| (size.width, convert_line_break_mode(ui_mode)));

    let (width, height) = font.calculate_text_size(host_object.size, text, wrap);

    CGSize { width, height }
}

/// Determine how the text lines will be rendered given a constraint
pub fn break_lines_with_font<'a>(
    env: &mut Environment,
    font: id,
    text: &'a str,
    constrained: Option<(CGSize, UILineBreakMode)>,
) -> Vec<(f32, &'a str)> {
    let host_object = env.objc.borrow::<UIFontHostObject>(font);

    let font = get_font(
        &mut env.framework_state.uikit.ui_font,
        host_object.kind,
        text,
    );

    let wrap = constrained.map(|(size, ui_mode)| (size.width, convert_line_break_mode(ui_mode)));

    font.break_lines(host_object.size, text, wrap)
}

#[inline(always)]
pub fn draw_font_glyph(
    drawer: &mut CGBitmapContextDrawer,
    raster_glyph: crate::font::RasterGlyph,
    fill_color: (f32, f32, f32, f32),
    clip_x: Option<Range<f32>>,
    clip_y: Option<Range<f32>>,
) {
    let mut glyph_rect = {
        let (x, y) = raster_glyph.origin();
        let (width, height) = raster_glyph.dimensions();
        CGRect {
            origin: CGPoint { x, y },
            size: CGSize {
                width: width as f32,
                height: height as f32,
            },
        }
    };
    // The code in font.rs won't and can't clip glyphs hanging over the right
    // and bottom sides of the rect, so it has to be done here. Bear in mind
    // that this must not incorrectly affect the texture co-ordinates, otherwise
    // the glyphs become squashed instead.
    // Note that there isn't clipping for the other sides currently because it
    // doesn't seem to be needed.
    if let Some(clip_x) = clip_x {
        if glyph_rect.origin.x >= clip_x.end {
            return;
        }
        if glyph_rect.origin.x + glyph_rect.size.width > clip_x.end {
            glyph_rect.size.width = clip_x.end - glyph_rect.origin.x;
        }
    }
    if let Some(clip_y) = clip_y {
        if glyph_rect.origin.y >= clip_y.end {
            return;
        }
        if glyph_rect.origin.y + glyph_rect.size.height > clip_y.end {
            glyph_rect.size.height = clip_y.end - glyph_rect.origin.y;
        }
    }

    for ((x, y), (tex_x, tex_y)) in drawer.iter_transformed_pixels(glyph_rect) {
        // TODO: bilinear sampling
        let coverage = raster_glyph.pixel_at((
            (tex_x * glyph_rect.size.width - 0.5).round() as i32,
            (tex_y * glyph_rect.size.height - 0.5).round() as i32,
        ));
        let (r, g, b, a) = fill_color;
        let (r, g, b, a) = (r * coverage, g * coverage, b * coverage, a * coverage);
        drawer.put_pixel((x, y), (r, g, b, a), /* blend: */ true);
    }
}

/// Called by the `drawAtPoint:` method family on `NSString`.
pub fn draw_at_point(
    env: &mut Environment,
    font: id,
    text: &str,
    point: CGPoint,
    width_and_line_break_mode: Option<(CGFloat, UILineBreakMode)>,
) -> CGSize {
    let context = UIGraphicsGetCurrentContext(env);

    let host_object = env.objc.borrow::<UIFontHostObject>(font);

    let font = get_font(
        &mut env.framework_state.uikit.ui_font,
        host_object.kind,
        text,
    );

    let width_and_line_break_mode =
        width_and_line_break_mode.map(|(width, ui_mode)| (width, convert_line_break_mode(ui_mode)));
    let clip_x = width_and_line_break_mode.map(|(width, _)| point.x..(point.x + width));
    let (width, height) =
        font.calculate_text_size(host_object.size, text, width_and_line_break_mode);

    let mut drawer = CGBitmapContextDrawer::new(&env.objc, &mut env.mem, context);
    let fill_color = drawer.rgb_fill_color();

    font.draw(
        host_object.size,
        text,
        (point.x, point.y),
        width_and_line_break_mode,
        TextAlignment::Left,
        |raster_glyph| {
            draw_font_glyph(
                &mut drawer,
                raster_glyph,
                fill_color,
                clip_x.clone(),
                /* clip_y: */ None,
            )
        },
    );

    CGSize { width, height }
}

/// Called by the `drawInRect:` method family on `NSString`.
pub fn draw_in_rect(
    env: &mut Environment,
    font: id,
    text: &str,
    rect: CGRect,
    line_break_mode: UILineBreakMode,
    alignment: UITextAlignment,
) -> CGSize {
    let context = UIGraphicsGetCurrentContext(env);

    let text_size = size_with_font(env, font, text, Some((rect.size, line_break_mode)));

    let host_object = env.objc.borrow::<UIFontHostObject>(font);

    let font = get_font(
        &mut env.framework_state.uikit.ui_font,
        host_object.kind,
        text,
    );

    let mut drawer = CGBitmapContextDrawer::new(&env.objc, &mut env.mem, context);
    let fill_color = drawer.rgb_fill_color();

    let (origin_x_offset, alignment) = match alignment {
        UITextAlignmentLeft => (0.0, TextAlignment::Left),
        UITextAlignmentCenter => (rect.size.width / 2.0, TextAlignment::Center),
        UITextAlignmentRight => (rect.size.width, TextAlignment::Right),
        _ => unimplemented!(),
    };

    font.draw(
        host_object.size,
        text,
        (rect.origin.x + origin_x_offset, rect.origin.y),
        Some((rect.size.width, convert_line_break_mode(line_break_mode))),
        alignment,
        |raster_glyph| {
            draw_font_glyph(
                &mut drawer,
                raster_glyph,
                fill_color,
                /* clip_x: */ Some(rect.origin.x..(rect.origin.x + rect.size.width)),
                /* clip_y: */ Some(rect.origin.y..(rect.origin.y + rect.size.height)),
            )
        },
    );

    text_size
}

/// The file names to look for in the bundle's root for a name passed to
/// `fontWithName:size:`, in order: the name itself (4Towers passes
/// "Alien-Encounters-Bold.ttf"), then with `.ttf`, `.otf` and `.ttc` added
/// (it also passes "Alien-Encounters-Solid-Bold" and "Futura"). Empty for a
/// name that is empty or has a `/` in it.
fn font_file_candidates(name: &str) -> Vec<String> {
    if name.is_empty() || name.contains('/') {
        return Vec::new();
    }
    let mut candidates = vec![name.to_string()];
    for extension in [".ttf", ".otf", ".ttc"] {
        candidates.push(format!("{name}{extension}"));
    }
    candidates
}

/// Whether a bundle file name ends in a font extension (`.ttf`, `.otf` or
/// `.ttc`, any case).
fn is_font_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".ttf", ".otf", ".ttc"]
        .iter()
        .any(|extension| lower.ends_with(extension))
}

/// The face whose PostScript name, else full name, else family name is
/// exactly `name`. The faces are searched in the order given, so with two
/// matches the first file (and the first face in it) wins. Returns the index
/// into `faces` and the face index within that file.
fn match_face_by_name(name: &str, faces: &[(String, FaceNames)]) -> Option<(usize, u32)> {
    let fields: [fn(&FaceNames) -> &Vec<String>; 3] =
        [|f| &f.postscript, |f| &f.full, |f| &f.family];
    fields.iter().find_map(|field| {
        faces
            .iter()
            .position(|(_, face)| field(face).iter().any(|n| n == name))
            .map(|i| (i, faces[i].1.index))
    })
}

/// For a name `fontWithName:size:` does not know: a font file shipped in the
/// app bundle's root, loaded through the guest filesystem.
///
/// First the file names from [font_file_candidates], matching the case
/// exactly, then (with `--bundle-files-ignore-case`) the same names through
/// the filesystem's case-insensitive fallback. A `.ttc` found by file name
/// gives its first face. Then the PostScript, full and family names inside
/// every `.ttf`, `.otf` and `.ttc` in the bundle root (see
/// [match_face_by_name]).
///
/// The result is cached per name, and which file was taken is logged once
/// per name.
fn find_bundle_font(env: &mut Environment, name: &str) -> Option<FontKind> {
    if let Some(&found) = env
        .framework_state
        .uikit
        .ui_font
        .bundle_font_for_name
        .get(name)
    {
        return found.map(FontKind::Bundle);
    }

    let found = find_bundle_font_uncached(env, name);
    let state = &mut env.framework_state.uikit.ui_font;
    state.bundle_font_for_name.insert(name.to_string(), found);
    found.map(FontKind::Bundle)
}

/// The search behind [find_bundle_font], without the cache.
fn find_bundle_font_uncached(env: &mut Environment, name: &str) -> Option<usize> {
    let bundle_path = env.bundle.bundle_path().to_owned();
    let mut files: Vec<String> = match env.fs.enumerate(&bundle_path) {
        Ok(names) => names.map(String::from).collect(),
        Err(()) => return None,
    };
    files.retain(|file| env.fs.is_file(&bundle_path.join(file)));
    files.sort();

    // By file name.
    let candidates = font_file_candidates(name);
    let exact = candidates.iter().find(|c| files.contains(c)).cloned();
    let by_file = exact.or_else(|| {
        if !env.options.bundle_files_ignore_case {
            return None;
        }
        // With this option on, the filesystem takes a name that differs only
        // in case (and logs it).
        candidates
            .into_iter()
            .find(|c| env.fs.is_file(&bundle_path.join(c)))
    });
    if let Some(file) = by_file {
        if let Some(index) = load_bundle_font(env, &bundle_path, &file, 0) {
            log!(
                "Font {:?}: using {:?} from the app bundle (by file name)",
                name,
                file
            );
            return Some(index);
        }
    }

    // By the names inside the font files.
    if env
        .framework_state
        .uikit
        .ui_font
        .bundle_face_names
        .is_none()
    {
        let mut all = Vec::new();
        for file in files.iter().filter(|file| is_font_file_name(file)) {
            let Ok(bytes) = env.fs.read(bundle_path.join(file)) else {
                continue;
            };
            for face in face_names(&bytes) {
                all.push((file.clone(), face));
            }
        }
        env.framework_state.uikit.ui_font.bundle_face_names = Some(all);
    }
    let faces = env
        .framework_state
        .uikit
        .ui_font
        .bundle_face_names
        .as_ref()
        .unwrap();
    let (i, face_index) = match_face_by_name(name, faces)?;
    let file = faces[i].0.clone();
    let index = load_bundle_font(env, &bundle_path, &file, face_index)?;
    log!(
        "Font {:?}: using {:?} (face {}) from the app bundle (by the name inside the font)",
        name,
        file,
        face_index
    );
    Some(index)
}

/// Load face `face_index` of the bundle file `file` (once) and return its
/// index in [State]'s `bundle_fonts`.
fn load_bundle_font(
    env: &mut Environment,
    bundle_path: &GuestPath,
    file: &str,
    face_index: u32,
) -> Option<usize> {
    let key = (file.to_string(), face_index);
    if let Some(&index) = env
        .framework_state
        .uikit
        .ui_font
        .bundle_font_for_face
        .get(&key)
    {
        return Some(index);
    }
    let bytes = env.fs.read(bundle_path.join(file)).ok()?;
    let Some(font) = Font::from_bundle_bytes(bytes, face_index) else {
        log!(
            "Bundle file {:?} (face {}) could not be read as a font",
            file,
            face_index
        );
        return None;
    };
    let state = &mut env.framework_state.uikit.ui_font;
    state.bundle_fonts.push(font);
    let index = state.bundle_fonts.len() - 1;
    state.bundle_font_for_face.insert(key, index);
    Some(index)
}

fn get_equivalent_font(system_font: &str) -> Option<FontKind> {
    // Maps every font found in every font family in an iOS 2 Simulator
    match system_font {
        // Font Family: Courier
        "Courier" => None,
        "Courier-BoldOblique" => None,
        "Courier-Oblique" => None,
        "Courier-Bold" => None,
        // Font Family: AppleGothic
        "AppleGothic" => None,
        // Font Family: Arial
        "ArialMT" => Some(FontKind::SansRegular),
        "Arial-BoldMT" => Some(FontKind::SansBold),
        "Arial-BoldItalicMT" => Some(FontKind::SansBoldItalic),
        "Arial-ItalicMT" => Some(FontKind::SansItalic),
        // Font Family: STHeiti TC
        "STHeitiTC-Light" => None,
        "STHeitiTC-Medium" => None,
        // Font Family: Hiragino Kaku Gothic ProN
        "HiraKakuProN-W6" => None,
        "HiraKakuProN-W3" => None,
        // Font Family: Courier New
        "CourierNewPS-BoldMT" => Some(FontKind::MonoRegular),
        "CourierNewPS-ItalicMT" => Some(FontKind::MonoBold),
        "CourierNewPS-BoldItalicMT" => Some(FontKind::MonoBoldItalic),
        "CourierNewPSMT" => Some(FontKind::MonoItalic),
        // Font Family: Zapfino
        "Zapfino" => None,
        // Font Family: Arial Unicode MS
        "ArialUnicodeMS" => None,
        // Font Family: STHeiti SC
        "STHeitiSC-Medium" => None,
        "STHeitiSC-Light" => None,
        // Font Family: American Typewriter
        "AmericanTypewriter" => Some(FontKind::MonoRegular),
        "AmericanTypewriter-Bold" => Some(FontKind::MonoBold),
        // Font Family: Helvetica
        "Helvetica-Oblique" => None,
        "Helvetica-BoldOblique" => None,
        "Helvetica" => None,
        "Helvetica-Bold" => None,
        // Font Family: Marker Felt
        "MarkerFelt-Thin" => None,
        // Font Family: Helvetica Neue
        "HelveticaNeue" => None,
        "HelveticaNeue-Bold" => None,
        // Font Family: DB LCD Temp
        "DBLCDTempBlack" => None,
        // Font Family: Verdana
        "Verdana-Bold" => None,
        "Verdana-BoldItalic" => None,
        "Verdana" => None,
        "Verdana-Italic" => None,
        // Font Family: Times New Roman
        "TimesNewRomanPSMT" => Some(FontKind::SerifRegular),
        "TimesNewRomanPS-BoldMT" => Some(FontKind::SerifBold),
        "TimesNewRomanPS-BoldItalicMT" => Some(FontKind::SerifBoldItalic),
        "TimesNewRomanPS-ItalicMT" => Some(FontKind::SerifItalic),
        // Font Family: Georgia
        "Georgia-Bold" => None,
        "Georgia" => None,
        "Georgia-BoldItalic" => None,
        "Georgia-Italic" => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{font_file_candidates, is_font_file_name, match_face_by_name};
    use crate::font::FaceNames;

    #[test]
    fn font_file_candidates_add_the_extensions() {
        assert_eq!(
            font_file_candidates("Alien-Encounters-Solid-Bold"),
            [
                "Alien-Encounters-Solid-Bold",
                "Alien-Encounters-Solid-Bold.ttf",
                "Alien-Encounters-Solid-Bold.otf",
                "Alien-Encounters-Solid-Bold.ttc",
            ]
        );
        assert_eq!(
            font_file_candidates("Alien-Encounters-Bold.ttf")[0],
            "Alien-Encounters-Bold.ttf"
        );
        assert!(font_file_candidates("").is_empty());
        assert!(font_file_candidates("../Ozone.ttf").is_empty());
    }

    #[test]
    fn font_files_are_found_by_extension() {
        assert!(is_font_file_name("Ozone.ttf"));
        assert!(is_font_file_name("DisplayOTF.otf"));
        assert!(is_font_file_name("Futura.TTC"));
        assert!(!is_font_file_name("Default.png"));
    }

    fn face(index: u32, postscript: &str, full: &str, family: &str) -> FaceNames {
        FaceNames {
            index,
            postscript: vec![postscript.to_string()],
            full: vec![full.to_string()],
            family: vec![family.to_string()],
        }
    }

    #[test]
    fn faces_match_postscript_then_full_then_family() {
        let faces = vec![
            (
                "Alien-Encounters-Bold.ttf".to_string(),
                face(
                    0,
                    "AlienEncountersBold",
                    "Alien Encounters Bold",
                    "Alien Encounters",
                ),
            ),
            (
                "Futura.ttc".to_string(),
                face(0, "Futura-Medium", "Futura Medium", "Futura"),
            ),
            (
                "Futura.ttc".to_string(),
                face(1, "Futura-MediumItalic", "Futura Medium Italic", "Futura"),
            ),
            (
                "Odd.ttf".to_string(),
                face(0, "Futura", "Futura Odd", "Futura Odd"),
            ),
        ];
        assert_eq!(
            match_face_by_name("AlienEncountersBold", &faces),
            Some((0, 0))
        );
        assert_eq!(
            match_face_by_name("Alien Encounters Bold", &faces),
            Some((0, 0))
        );
        assert_eq!(
            match_face_by_name("Futura-MediumItalic", &faces),
            Some((2, 1))
        );
        // A PostScript name wins over an earlier family name.
        assert_eq!(match_face_by_name("Futura", &faces), Some((3, 0)));
        assert_eq!(match_face_by_name("Futura Medium", &faces), Some((1, 0)));
        // Without it, the first face of the family.
        assert_eq!(match_face_by_name("Alien Encounters", &faces), Some((0, 0)));
        assert_eq!(match_face_by_name("futura", &faces), None);
        assert_eq!(match_face_by_name("Helvetica", &faces), None);
    }
}
