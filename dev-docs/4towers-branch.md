# The `4towers` branch

A personal fork branch. The changes here were written with AI assistance and are not for upstream (see the note at the top of the README). They sit on top of upstream commit `d34530b5`.

Each item is something *4Towers: Onslaught* 1.01 (iOS 4.1, cocos2d) called that stock touchHLE did not have, or stopped on.

## Foundation

- `-[NSMutableArray removeObjectIdenticalTo:]` and `-[NSMutableArray removeObjectsInArray:]`.
- `-[NSMutableArray removeObject:]` removes from the back, so the indexes it collected stay valid.
- `-[NSNumber encodeWithCoder:]` for the long long, short, char, unsigned and float kinds.
- `NSKeyedArchiver`: `encodeBool:forKey:`, `encodeInt32:forKey:`, `encodeInt64:forKey:`, `encodeFloat:forKey:`, `encodeDouble:forKey:` and `encodeCGPoint:forKey:`. Encoding the same key twice now overwrites instead of stopping the emulator.
- `NSSet` and `NSMutableSet`: `copyWithZone:` and `mutableCopyWithZone:`.
- `initWithDictionary:` with nil gives an empty dictionary instead of stopping the emulator (seen in `Game::addCreeps` on Android).
- `NSObject`: `methodForSelector:`, `instanceMethodForSelector:` and `methodSignatureForSelector:`. `instanceMethodSignatureForSelector:` returns nil for a selector it does not know.

## AudioToolbox and CoreGraphics

- `ExtAudioFileGetProperty` answers `kExtAudioFileProperty_FileLengthFrames`.
- `CGBitmapContextCreate` works out the row size when it is passed 0 with a caller's buffer.

## UIKit

- `-[UIApplication statusBarStyle]` and `-[UIApplication statusBarFrame]`.
- `-[UIViewController init]` and a logging stub for `presentModalViewController:animated:`.

## Fonts

`+[UIFont fontWithName:size:]` used to know only the fonts touchHLE maps to its own Liberation family, so every font 4Towers asked for came out as the system font. It now looks in the app bundle when the name is not one of those:

1. A file in the bundle's root named exactly what was asked for, then that name plus `.ttf`, `.otf` and `.ttc` (a `.ttc` gives its first face). The game asks both ways: `Alien-Encounters-Bold.ttf` as a file name, and `Alien-Encounters-Solid-Bold` and `Futura` as bare names. With `--bundle-files-ignore-case` on, the same names are then tried through the filesystem's case-insensitive fallback.
2. Otherwise, the names inside every `.ttf`, `.otf` and `.ttc` in the bundle's root: the PostScript name, then the full name, then the family name, matched exactly (case included). Files are searched in name order, so for a family name the first file's first face wins.

The file is read through the guest filesystem, so this works the same on Android. Each face is loaded once and kept, and which file a name took is logged once per name. A name found nowhere still logs "No replacement found for font ... Using system font instead." The font is then used everywhere the Liberation fonts were: `ascender`, `descender`, `leading` and `lineHeight`, `sizeWithFont:`, and drawing (`drawAtPoint:`, `drawInRect:`, which cocos2d's labels go through). A bundle font is sized the way iPhone OS sizes fonts (the point size is the em size), not with the eyeballed factor the Liberation stand-ins use.

4Towers ships eight `Alien-Encounters-*.ttf`, `Ozone.ttf`, `DisplayOTF.otf` and `Futura.ttc`. `Futura.ttc` names its faces only on the Macintosh platform, which touchHLE reads as plain ASCII.

**Not settled:** whether a real iPhone accepts a file-style name such as `Alien-Encounters-Bold.ttf` in `fontWithName:size:`. touchHLE now shows the bundle font for both forms, so this is not evidence either way, and the remake's spec has to decide from other evidence. What is known: the game's `Info.plist` has no `UIAppFonts` key, so iOS does not register the bundle fonts with UIKit, and the game's binary contains the FontLabel library (`FontManager`, `ZFont`), which cocos2d builds of that era can fall back to (when built with FontLabel support) when `fontWithName:size:` returns nil and which loads a `.ttf` from the bundle by file name. So on the phone the fonts were most likely drawn by FontLabel, not UIKit, but the binary has not been traced to confirm it. touchHLE's `fontWithName:size:` never returns nil, so that path never runs under touchHLE, and FontLabel's line metrics may differ slightly from the ones touchHLE uses. None of this has been checked on a device.

## Filesystem

- If creating the app's `Documents`, `Library` or `tmp` folder fails, touchHLE logs it (debug builds) and tries a plain `create_dir` instead of stopping. The Android build needed this.
- `--bundle-files-ignore-case` (off by default): a bundle file the app asks for under the wrong letter case is found anyway, if exactly one file matches, for lookups (`stat`, `access`, `NSFileManager`, audio) and for opening it to read. Folders and the app's own writeable files still need the exact name, and each substitution is logged once. 4Towers asks for `Powerup.mp3` and its bundle holds `PowerUp.mp3`. The iPhone's filesystem is case-sensitive, so the original game most likely never played that sound; the option is off so touchHLE matches the phone, and turning it on lets you hear what was meant. This has not been checked on a real device.

## GameKit

These are stubs. Nothing talks to Game Center.

- `GKLeaderboardViewController` and `GKAchievementViewController`. Setting the delegate calls its "did finish" method straight away, so the game carries on as if the screen had been closed.
- `GKAchievement` and `GKScore` accept a value and a report call and do nothing.
- `+[GKLocalPlayer localPlayer]` returns an object, and `authenticateWithCompletionHandler:` does nothing.

## Message trace

`--trace-selectors=` takes a comma-separated list of Objective-C selectors, for example `--trace-selectors=schedule:interval:,setPosition:`. Every time the game (or touchHLE itself) calls one of those methods, one line goes into `trace.log`: the frame number, milliseconds since start, the thread, who sent it, how it was called (the `variant` column), the receiver's class, the selector, the first two arguments (as hex and as a float), four words from the stack, the return address and, in the last column (`self`), the receiver's address, which tells two objects of the same class apart. The first lines of the file say which touchHLE build, which app and which selectors. It is there to time things the decompile cannot, like how long a station takes to charge.

Two kinds of call are traced. Ordinary message sends go through `objc_msgSend` and show up as `send` (or `stret`, `super2`). Methods run by cocos2d's scheduler (`schedule:interval:` timers such as `charging:` or `tLoop:`) do not: the scheduler asks for the method's address once with `methodForSelector:` and calls it directly from then on. For a traced selector, `methodForSelector:` and `instanceMethodForSelector:` hand back a small stub instead, which writes the line and then jumps into the real method, so those calls show up as `imp`. A method that returns a struct or a union is left unwrapped and its `imp` calls are not traced, because such a method can be called with the return buffer in the receiver's register and the stub cannot tell; touchHLE logs it once. Nothing changes for selectors that are not traced, or when the trace is off.

You can also list selectors one per line in `trace_selectors.txt` next to the other touchHLE files (`#` starts a comment). The option and the file add up. A line in the file can carry marker words after the selector, separated by spaces: the only one so far is `png` (see Frames below). A word that is not a marker is ignored and logged once. With no selectors, and without `--capture-frames=`, nothing is traced and nothing is written. `trace.log` is replaced each run.

- **Windows:** `trace.log` is written in the folder you run `touchHLE.exe` from, next to `touchHLE_log.txt`.
- **Android:** the command line is not available, so either put `trace_selectors.txt` in touchHLE's files folder, or add the option to the game's line in `touchHLE_options.txt`. `trace.log` lands in the same folder, which is the app's `files` folder: `Android/data/org.touchhle.android.unofficial/files/` for this fork's own APK, or `Android/data/com.lambdamugames.fourtowers/files/` for the remake repo's standalone 4Towers APK (for example `adb pull /sdcard/Android/data/com.lambdamugames.fourtowers/files/trace.log`).

## Frames and touch log

These come with the message trace: they are written only while it is on, into the same folder as `trace.log`, and they use its clock (milliseconds since touchHLE started) and its frame numbers, so a row in one file can be found in the others.

### Touch log

`input.log` gets one row per finger for every touch event the game receives. It is replaced each run. Lines starting with `#` are the header (the touchHLE build, the app, the format, and last the column names); every other line is a tab-separated row:

| column | meaning |
| --- | --- |
| `frame` | frames presented so far, as in `trace.log` |
| `ms` | milliseconds since touchHLE started, three decimals, as in `trace.log` |
| `phase` | `down`, `move` or `up` |
| `finger` | which finger: `Mouse`, `Touch(n)` (n is the host's finger id), `VirtualCursor`, `ButtonToTouch(button)`, `StickToTouch` or `DpadToTouch` |
| `x`, `y` | the position in guest points (the screen is 320 x 480 points), unrotated: what the game's `UITouch` reports, whatever the window's rotation or scale |

An event with two fingers gives two rows with the same `frame`, `ms` and `phase`. The rows are written as the events reach the game's run loop, each event in one write, so a crash keeps every row before it.

### Frames

Two ways to save the picture the game presented, as PNG files:

- `--capture-frames=N` (N of 1 or more; absent or 0 is off) saves every Nth frame: frames 0, N, 2N and so on. Like any option it can go on the game's line in `touchHLE_options.txt`. It turns the trace on by itself, so `trace.log` and `input.log` are written even with no selectors.
- A `png` marker on a line of `trace_selectors.txt`, for example `powerFire png`, saves the first frame presented after each call of that selector: the frame that shows its result. Several calls before the next frame give one picture.

Each picture is `frames/f{frame}_{ms}.png` in the same folder as `trace.log`, for example `frames/f000042_15873.png`:

- `frame` has at least six digits and is the `frame` column of the `trace.log` (and `input.log`) rows whose drawing the picture shows. The first frame presented is frame 0. A picture saved for a `png` selector carries the same frame number as the row of the call that asked for it.
- `ms` is the whole milliseconds since touchHLE started (the `ms` clock of `trace.log`, cut to a whole number) when the frame was presented.

The picture is the game's own renderbuffer as the game drew it, 8-bit RGB with the alpha channel dropped: 320 x 480 for 4Towers, not rotated, without the window's letterbox or the virtual cursor. With `--scale-hack=N` it is the scaled renderbuffer, N times as wide and N times as high. Earlier pictures (files named like these) are removed from `frames/` when a run that saves frames starts, so the folder matches that run's `trace.log`.

What it costs: the read-back (`glReadPixels`) runs on the emulator thread and waits for the GPU to finish the frame, a few milliseconds per saved frame, which the game sees as time passing (its clocks are wall-clock). Flipping, encoding and writing happen on one worker thread. If the worker falls eight frames behind, further frames are dropped and the log says how many. Frames still queued when touchHLE stops abruptly are lost. A picture is roughly 100-250 KB (an estimate, not yet measured on 4Towers), so every 10th frame at 30 fps is roughly 25 MB a minute.

Only frames presented straight from a full-screen `CAEAGLLayer` (touchHLE's fast path) are saved. When the game presents through the Core Animation compositor instead (a UIKit view on top of the game's layer, `--force-composition`, or no full-screen layer), no picture is saved and the `frame` number does not advance; touchHLE logs this once. A `png` marker waiting at that point is served by the next fast-path frame.

On Android the `frames` folder is next to `trace.log` in the app's files folder; the standalone launcher's Share Log does not share it yet.

## Builds

The build workflow on this branch clones this fork instead of upstream and also runs on pushes here (and to the `4towers-trace` working branch), so GitHub Actions builds the branch for Windows, macOS and Android. That is the only change to it.

The `4towers-gnu-ci` branch adds a second Windows job, `build-windows-gnu`, which builds the same sources for `x86_64-pc-windows-gnu` with the MinGW-w64 toolchain from MSYS2 and uploads `touchHLE_windows_x86_64_gnu` (the usual Windows bundle plus the MinGW runtime DLLs the exe needs). Every MSVC-built touchHLE, this fork's CI builds and upstream v0.3.0 alike, crashes with an access violation (0xC0000005) a few seconds after 4Towers' title screen on a laptop with Intel Iris Xe graphics, while a GNU-built exe plays the game. The MSVC job is unchanged.
