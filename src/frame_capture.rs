/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Frames saved as PNG files for the message trace (`--capture-frames=` and
//! the `png` marker in `trace_selectors.txt`, see [crate::objc]'s trace
//! module).
//!
//! The emulator thread only reads the pixels back (in
//! `present_renderbuffer`) and hands the buffer over. One worker thread
//! flips the rows, drops the alpha channel, encodes the PNG and writes
//! `frames/f{frame:06}_{ms}.png` in the user data directory. The encoder is
//! a minimal one: 8-bit RGB, every row with the "Up" filter, deflated by
//! `flate2` at a moderate level, with `crc32fast` for the chunk checksums.

use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};

/// Folder in the user data directory the frames are written to.
pub const FRAMES_DIR: &str = "frames";

/// How many frames may wait for the worker. When it falls this far behind,
/// further frames are dropped (and counted in the log) rather than making
/// the emulator wait.
const QUEUE_LENGTH: usize = 8;

/// The zlib level: moderate, so a phone keeps up.
const COMPRESSION_LEVEL: u32 = 5;

/// One frame read back from the app's renderbuffer.
struct CapturedFrame {
    frame: u32,
    ms: u64,
    width: u32,
    height: u32,
    /// RGBA, 8 bits per channel, bottom row first (as `glReadPixels` gives
    /// it).
    rgba: Vec<u8>,
}

/// The emulator thread's end of the frame capture.
pub struct FrameCapture {
    sender: SyncSender<CapturedFrame>,
    /// Frames dropped because the worker was behind.
    dropped: u64,
}

impl FrameCapture {
    /// Create (or empty of earlier captures) `dir` and start the worker.
    pub fn start(dir: PathBuf) -> Result<FrameCapture, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {}", dir.display(), e))?;
        remove_earlier_frames(&dir);
        let (sender, receiver) = sync_channel(QUEUE_LENGTH);
        std::thread::Builder::new()
            .name("touchHLE frame capture".to_string())
            .spawn(move || worker(&dir, receiver))
            .map_err(|e| format!("could not start the frame capture thread: {e}"))?;
        Ok(FrameCapture { sender, dropped: 0 })
    }

    /// Hand a frame to the worker. `rgba` is what `glReadPixels` returned
    /// for a `width` by `height` area in `GL_RGBA`/`GL_UNSIGNED_BYTE`.
    /// Never blocks: if the worker is behind, the frame is dropped.
    pub fn send(&mut self, frame: u32, ms: u64, width: u32, height: u32, rgba: Vec<u8>) {
        let captured = CapturedFrame {
            frame,
            ms,
            width,
            height,
            rgba,
        };
        match self.sender.try_send(captured) {
            Ok(()) => (),
            Err(TrySendError::Full(_)) => {
                self.dropped += 1;
                // Powers of two, so a long stall does not flood the log.
                if self.dropped.is_power_of_two() {
                    log!(
                        "Frame capture is behind: frame {} not saved ({} dropped so far)",
                        frame,
                        self.dropped
                    );
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                log_once!("Frame capture thread has stopped, frames are no longer saved");
            }
        }
    }
}

/// The file name a captured frame is saved under: the trace's frame number
/// (six digits or more) and the milliseconds since startup.
pub fn frame_file_name(frame: u32, ms: u64) -> String {
    format!("f{frame:06}_{ms}.png")
}

/// Whether `name` looks like one of [frame_file_name]'s.
fn is_frame_file_name(name: &str) -> bool {
    let Some(stem) = name
        .strip_prefix('f')
        .and_then(|rest| rest.strip_suffix(".png"))
    else {
        return false;
    };
    let Some((frame, ms)) = stem.split_once('_') else {
        return false;
    };
    let is_number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    frame.len() >= 6 && is_number(frame) && is_number(ms)
}

/// Delete the frames an earlier run left in `dir`, so the folder matches
/// this run's `trace.log`. Only files named like [frame_file_name] are
/// touched.
fn remove_earlier_frames(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_str().is_some_and(is_frame_file_name)
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    if removed > 0 {
        log!(
            "Frame capture: removed {} frame(s) left from an earlier run in {}",
            removed,
            dir.display()
        );
    }
}

/// The worker thread: encode and write each frame until the emulator's end
/// of the channel goes away.
fn worker(dir: &Path, receiver: Receiver<CapturedFrame>) {
    let mut failed = false;
    while let Ok(captured) = receiver.recv() {
        let path = dir.join(frame_file_name(captured.frame, captured.ms));
        let expected_len = (captured.width as usize)
            .checked_mul(captured.height as usize)
            .and_then(|n| n.checked_mul(4));
        if captured.width == 0 || captured.height == 0 || expected_len != Some(captured.rgba.len())
        {
            log!(
                "Frame capture: frame {} has an unexpected size ({}x{}, {} bytes), not saved",
                captured.frame,
                captured.width,
                captured.height,
                captured.rgba.len()
            );
            continue;
        }
        let png = encode_png(captured.width, captured.height, &captured.rgba);
        if let Err(e) = std::fs::write(&path, png) {
            // Once per run: a full disk would otherwise log every frame.
            if !failed {
                log!(
                    "Frame capture: could not write {}: {} (further failures are not logged)",
                    path.display(),
                    e
                );
                failed = true;
            }
        }
    }
}

/// Encode an image as a PNG file: 8-bit RGB, no interlacing. `rgba` holds
/// `width` by `height` RGBA pixels, bottom row first, as `glReadPixels`
/// returns them; the PNG has the top row first, and the alpha channel is
/// dropped. `rgba.len()` must be `width * height * 4`.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    assert_eq!(rgba.len(), w * h * 4);

    // Each row is a filter type byte and the filtered RGB bytes. Filter 2
    // ("Up") stores each byte minus the byte above it, which suits game
    // frames with large flat areas.
    let stride = w * 3;
    let mut filtered = Vec::with_capacity((stride + 1) * h);
    let mut above = vec![0u8; stride];
    let mut row = vec![0u8; stride];
    for y in 0..h {
        let source = &rgba[(h - 1 - y) * w * 4..][..w * 4];
        for x in 0..w {
            row[x * 3..x * 3 + 3].copy_from_slice(&source[x * 4..x * 4 + 3]);
        }
        filtered.push(2);
        filtered.extend(row.iter().zip(&above).map(|(&b, &a)| b.wrapping_sub(a)));
        std::mem::swap(&mut row, &mut above);
    }

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(COMPRESSION_LEVEL));
    // Writing to a Vec cannot fail.
    encoder.write_all(&filtered).unwrap();
    let image_data = encoder.finish().unwrap();

    let mut header = [0u8; 13];
    header[0..4].copy_from_slice(&width.to_be_bytes());
    header[4..8].copy_from_slice(&height.to_be_bytes());
    // Bytes 10 to 12 (compression, filter method, interlacing) stay 0.
    header[8] = 8; // bits per channel
    header[9] = 2; // colour type: RGB

    let mut png = Vec::with_capacity(image_data.len() + 57);
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    write_chunk(&mut png, b"IHDR", &header);
    write_chunk(&mut png, b"IDAT", &image_data);
    write_chunk(&mut png, b"IEND", &[]);
    png
}

/// Append a PNG chunk: the data length (big-endian), the type, the data,
/// and the CRC-32 of the type and data.
fn write_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut crc = crc32fast::Hasher::new();
    crc.update(kind);
    crc.update(data);
    png.extend_from_slice(&crc.finalize().to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::{encode_png, frame_file_name, is_frame_file_name, write_chunk};
    use std::io::Read;

    #[test]
    fn chunks_carry_length_type_and_crc() {
        let mut png = Vec::new();
        write_chunk(&mut png, b"IEND", &[]);
        assert_eq!(
            png,
            [0, 0, 0, 0, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82]
        );
    }

    #[test]
    fn a_two_by_two_image_is_flipped_and_filtered() {
        // Bottom row first, as glReadPixels returns it.
        let rgba = [
            1, 2, 3, 255, 4, 5, 6, 255, // bottom row
            7, 8, 9, 255, 10, 11, 12, 128, // top row
        ];
        let png = encode_png(2, 2, &rgba);

        // Signature, then IHDR: 2x2, 8-bit, RGB (CRC from zlib.crc32).
        let start: [u8; 33] = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // signature
            0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, // IHDR
            0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, // 2 x 2
            0x08, 0x02, 0x00, 0x00, 0x00, // 8-bit RGB
            0xfd, 0xd4, 0x9a, 0x73, // CRC
        ];
        assert_eq!(png[..33], start);
        assert_eq!(
            png[png.len() - 12..],
            [0, 0, 0, 0, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82]
        );

        // IDAT holds the whole zlib stream; inflated, it is the top row
        // first, filter 2 ("Up") on each row, no alpha.
        let idat_len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        assert_eq!(&png[37..41], b"IDAT");
        assert_eq!(33 + 12 + idat_len + 12, png.len());
        let mut inflated = Vec::new();
        flate2::read::ZlibDecoder::new(&png[41..41 + idat_len])
            .read_to_end(&mut inflated)
            .unwrap();
        assert_eq!(
            inflated,
            [
                2, 7, 8, 9, 10, 11, 12, // top row, nothing above it
                2, 250, 250, 250, 250, 250, 250, // bottom row minus top row
            ]
        );
        let mut crc = crc32fast::Hasher::new();
        crc.update(&png[37..41 + idat_len]);
        assert_eq!(
            png[41 + idat_len..45 + idat_len],
            crc.finalize().to_be_bytes()
        );
    }

    #[test]
    fn frame_file_names_line_up_with_the_trace() {
        assert_eq!(frame_file_name(42, 1234), "f000042_1234.png");
        assert_eq!(frame_file_name(1234567, 0), "f1234567_0.png");
        assert!(is_frame_file_name("f000042_1234.png"));
        assert!(is_frame_file_name("f1234567_0.png"));
        assert!(!is_frame_file_name("f42_1234.png"));
        assert!(!is_frame_file_name("f000042_.png"));
        assert!(!is_frame_file_name("f000042_12.jpg"));
        assert!(!is_frame_file_name("notes.png"));
    }
}
