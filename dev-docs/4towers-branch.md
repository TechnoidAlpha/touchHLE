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
- Home and closing the window. touchHLE turns the Home button (Android, or any host that sends the app to the background) and closing the window into an exit. Stock touchHLE then sends `applicationWillResignActive:` and `applicationWillTerminate:`. iPhone OS 4.1 with multitasking sends `applicationDidEnterBackground:` on Home instead (the game's `Info.plist` has no `UIApplicationExitsOnSuspend`), and 4Towers writes the level being played (the `savedData` key of its `NSUserDefaults`, with `isGameRunning` set) only in that method. The exit now sends `applicationWillResignActive:`, then `applicationDidEnterBackground:` if the delegate has it (and posts `UIApplicationDidEnterBackgroundNotification`), then `applicationWillTerminate:` as before. Each method runs to the end before the next, and the game calls `synchronize` itself, so the plist in the sandbox's `Library/Preferences` is written before touchHLE exits; touchHLE also synchronizes once more at the end. At its next launch the game finds `isGameRunning` and starts inside the saved level, as a suspended iPhone app would come back. Leaving from a menu saves progress (`saveAll`) instead, as on the phone. Checked on 11 October 2026 on a Windows laptop with the CI GNU bundle of 54ce51fa (`touchHLE UNOFFICIAL v0.3.0-41-g54ce51fa`): Level 1 was closed at wave 3 with two stations built and six creeps on the board. After the window was closed the log showed "User requested quit, exiting.", the game's "notified!!", touchHLE's "Sending applicationDidEnterBackground: before exiting.", the game's "saved" twice and "game dealloc", and touchHLE exited by itself in about 5.5 s with code 0. The plist then held `isGameRunning` true and a `savedData` of 28,470 bytes. The next launch came back inside Level 1 under the game's own pause menu (RESUME, RESTART, DATA, OPTIONS, QUIT), with no panic and no unimplemented message, and the scheduled timers (`charging:`, `startMoving:`) were ticking while that menu showed. So the archive and its unarchiving at launch work through touchHLE's `NSKeyedArchiver` and `NSKeyedUnarchiver`.
- Starting at the logo again. A trace run closed in the middle of a level makes the next run start inside that level, so close a trace run from a menu unless the resume is wanted. To start at the logo after all, set `isGameRunning` to false, or remove `savedData`, in the sandbox's `Library/Preferences/com.lambdamugames.4towers.plist`; on Android, clear the app's storage.

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

Every line the game prints with `NSLog` (the lines starting `4Towers_3[0] ` in `touchHLE_log.txt`, which stays as it was) also becomes a `trace.log` row while the trace is on, so the few markers the game prints itself (the wave spawner finishing, pause choices, saves) sit on the trace's timeline. Such a row has the usual `frame`, `ms` and `thread`, `nslog` in the `origin` column, `-` in `variant` and `class`, the message in the `selector` column (tabs, carriage returns and newlines turned into spaces, so a row is always one line), and `-` in the ten columns after it (`arg0` to `self`). A parser that reads `arg0` and the rest as numbers should check the `origin` first.

You can also list selectors one per line in `trace_selectors.txt` next to the other touchHLE files (`#` starts a comment). The option and the file add up. A selector can be narrowed to one class of receiver (`setPosition:@CCLabelTTF`, see Receiver class below). A line in the file can carry marker words and peeks after the selector, separated by spaces: the only marker so far is `png` (see Frames below), and peeks are described below. A word that is neither is ignored and logged once. With no selectors, and without `--capture-frames=`, nothing is traced and nothing is written. `trace.log` is replaced each run.

- **Windows:** `trace.log` is written in the folder you run `touchHLE.exe` from, next to `touchHLE_log.txt`.
- **Android:** the command line is not available, so either put `trace_selectors.txt` in touchHLE's files folder, or add the option to the game's line in `touchHLE_options.txt`. `trace.log` lands in the same folder, which is the app's `files` folder: `Android/data/org.touchhle.android.unofficial/files/` for this fork's own APK, or `Android/data/com.lambdamugames.fourtowers/files/` for the remake repo's standalone 4Towers APK (for example `adb pull /sdcard/Android/data/com.lambdamugames.fourtowers/files/trace.log`).

### Receiver class

A selector can be narrowed to one class of receiver by ending the word with `@` and the class name, with no space: `setPosition:@CCLabelTTF` traces `setPosition:` only when the receiver's class, or one of its superclasses, is named `CCLabelTTF`. Without `@` every receiver is traced, as before. The rule:

- The name is matched exactly (case included) against the receiver's class and each class above it, up to the root, as touchHLE's class table names them. So `@CCNode` takes every label, sprite and menu, and `@CCLabel` takes nothing (there is no class of that name).
- A class method's receiver is the class itself, and its class is the metaclass, which has the same name, as have the metaclasses above it. So `@CCLabelTTF` also takes `+[CCLabelTTF labelWithString:fontName:fontSize:]` and the class methods of `CCLabelTTF`'s subclasses. The `class` column tells the two apart: `+CCLabelTTF` for a class method, `CCLabelTTF` for an instance.
- It applies to every row of the selector: `send`, `stret`, `super2` and `imp` alike. For a `super2` row it is the receiver's own class that is matched, not the superclass named in the `class` column. A scheduled selector gets its `imp` stub whatever the class; the stub checks the receiver at each call and writes nothing for a receiver that does not match.
- The same selector may be listed more than once, with different classes, or with and without one: a call is traced if any of its lines takes the receiver. A `png` marker counts only for calls its own line takes. Peeks belong to the selector, not to a line, so every traced row of the selector has the peeks of all its lines (one `# peeks` line per selector, as below).
- The `# selectors:` line of the header lists the words as written, `@` and class included. A word with nothing before the `@`, or nothing (or another `@`) after it, is not understood: its line is ignored and logged.
- An item of `--trace-selectors=` takes the same words: `--trace-selectors=setPosition:@CCLabelTTF,setString:;arg0:str`.

The class of each receiver is looked up once per selector, so a filter costs a hash lookup per call after the first.

What each label says and where it is, for 4Towers' HUD:

```
initWithString:dimensions:alignment:fontName:fontSize: arg0:str
setPosition:@CCLabelTTF
```

The first line writes the text of each `CCLabelTTF` made with a box (`labelWithString:dimensions:alignment:fontName:fontSize:` goes through it), with the new label's address in `self`. The second writes each move of a `CCLabelTTF`, and of no other node. `setPosition:` takes a `CGPoint`, which arrives as two floats in r2 and r3, so the `arg0_f32` and `arg1_f32` columns already are its x and y in points, and its `self` is the label's address, which joins it to the label's text. Add `setString: arg0:str` for the changes of text (see the list of label selectors under Peeks).

### Peeks

A line of `trace_selectors.txt` can also carry peeks: fields of the receiver or of a pointer argument, read from the game's memory when the call is logged and written as extra columns. They turn the trace into the original's own numbers: a creep's `hp` and `expectedHp` at each hit, the gold at each change, a tower's level and timer at each shot. Peeks follow the selector, separated by spaces, in any order with the `png` marker:

```
dmg: self.hp:f32 self.expectedHp:f32 self.fullhp:f32
addCreeps self.sharedData->waveNum:i32 self.sharedData->gSpeed:f32
fireTarget: arg0.distMoved:f32 arg0.hp:f32 png
```

On the command line (or the game's line in `touchHLE_options.txt`, which cannot hold spaces inside one option) the words of an item are separated by `;` instead: `--trace-selectors=dmg:;self.hp:f32;self.expectedHp:f32,addCreeps`.

The grammar of one peek, a single word with no spaces:

```
<base>(.<ivar> | .+0xNN | ->(<ivar> | +0xNN))* :<type>
```

- `<base>` is `self` (the receiver), `arg0` or `arg1`: the words in the row's `arg0` and `arg1` columns (r2 and r3 for an ordinary send; for a `stret` send, r3 and the first stack word). A float argument is just a word, so read it from `arg0_f32` instead.
- `.name` is the instance variable `name` of the object, found by name on its class chain in the class table touchHLE built from the binary's Objective-C metadata. It may only come straight after the base or after `->`: a field is not an object.
- `.+0xNN` (or decimal `.+NN`) moves NN bytes further on, from the object or from the field reached so far, so `self.sprPoint.+4:f32` is the y of a `CGPoint` ivar and `self.+0x150:f32` reads a raw offset.
- `->` reads the 4-byte pointer reached so far and carries on in the object it points at, whose class is read from its own isa: `->name` or `->+0xNN`. It is not C's `->`: the base is already the object, so `self->hp` reads the pointer at `self` (the isa) and then looks for `hp` on the class object, which gives `-` with "has no ivar hp". Write `self.hp:f32`.
- `:<type>` is one of:

| type | read as | written as |
| --- | --- | --- |
| `i32` | 4 bytes, signed | decimal |
| `u32` | 4 bytes, unsigned | decimal |
| `f32` | 4 bytes, a float | six significant digits |
| `u8` | 1 byte (a `BOOL` or `char` ivar) | decimal |
| `ptr` | 4 bytes | `0x` and eight hex digits |
| `id` | 4 bytes, an object pointer | hex, a colon and the class name read from the object's isa, for example `0x0012a4c0:Creeps`, or `nil` |
| `str` | 4 bytes, an `NSString` pointer | the string's text, escaped and cut (below), or `nil`, or `-` |

- With no step at all, the base word itself is the value: `arg0:id` names the class of the first argument, and `arg0:str` is the text of a string passed as the first argument.

A `str` peek writes what the string says, so a label's text can be read off the trace. The text comes from touchHLE's own record of the string, without calling the game, so it works for every string touchHLE made (`stringWithFormat:` and the rest) and for the constant strings in the game's binary (an `@"..."` in the source becomes a constant `CFString`, which touchHLE turns into one of its own strings when it loads the binary). Anything else gives `-`, with the reason logged once: an object that is not a string, or a string class the game defines itself (it keeps its text where touchHLE cannot see it). The text is written on one line and in one column:

- A tab, newline, carriage return and backslash are written as `\t`, `\n`, `\r` and `\\`. Nothing else is changed.
- At most 200 bytes of that escaped text are written. A longer text is cut before the first character, or escape, that would go past 200 bytes, so a character or an escape is never split, and `...` is added: the column holds at most 203 bytes. A text that is exactly 200 bytes is written whole, without `...`.
- A string held as UTF-16 with a broken surrogate pair shows U+FFFD there.

Rows:

- Every row of a selector with peeks, `send` and `imp` alike, has one more column per peek after `self`, in the order listed. Rows of other selectors (and `nslog` rows) end at `self` as before, so **the column count differs per selector**. The header lists them, one line per selector with peeks: `# peeks dmg: self.hp:f32 self.expectedHp:f32 self.fullhp:f32`. Split that line on spaces: after `# peeks`, the first word is the selector and the rest name its columns. The same selector listed twice gets the peeks of both lines, without repeats.
- Values: `f32` with six significant digits (C's `%g`, with the exponent written `1.5e-7`), integers in decimal, `ptr` as `0x` and eight hex digits.
- Peeks are read when the call starts, **before the method runs**. A row of `dmg:` shows the hit points before that hit; the state after it is in the peeks of the next traced call (for example a peek on `checkHp`, which `dmg:` calls).

No-panic rule: a peek only reads memory, through touchHLE's non-panicking reader, and the class table. It never sends a message or calls the game. A nil base, a pointer into the null page or past the end of memory, an object whose isa is not a known class, or a class chain without that ivar gives `-` in the column, and the reason is logged once per selector and peek; the run carries on. Reads need not be aligned. A wrong type is not detected: it gives a wrong number, not a `-` (except `str`, which gives `-` for anything that is not a string). The offset of each ivar is looked up once per class and cached, so a row with four peeks costs a few extra memory reads.

A list for the labels of 4Towers' HUD is below. cocos2d's labels take their text as an `NSString` in the first argument, and every `labelWithString:...` the game calls goes through the matching `initWithString:...`, so these catch each label as it is made (`CCLabelTTF`, then `CCLabelBMFont` and `CCLabelAtlas`), and `setString:` each change of its text. The `self` column of an `init` row is the label's address, which the label's later rows share. An `init` may also send `setString:` itself, which gives a second row with the same text.

```
initWithString:dimensions:alignment:fontName:fontSize: arg0:str
initWithString:fontName:fontSize: arg0:str
initWithString:fntFile: arg0:str
initWithString:charMapFile:itemWidth:itemHeight:startCharMap: arg0:str
setString: arg0:str
```

A list for 4Towers, from the decompile's field list (the offsets in the comments are the armv7 binary's, in case a name lookup ever fails):

```
dmg: self.hp:f32 self.expectedHp:f32 self.fullhp:f32         # Creeps +0x150 +0x154 +0x158; arg0_f32 = the hit
minusExpected: self.expectedHp:f32                            # +0x154; the damage reserved at fire time
ice:dur: self.ms:f32 self.originalSpeed:f32 self.freezed:u8 self.freezeLoops:f32
checkNewPoint self.currPoint:i32 self.distMoved:f32 self.timeElapsed:f32 self.totalMovementTime:f32 self.ms:f32
checkHp self.hp:f32 self.stealthNow:u8 self.ability:i32 self.type:i32
setExpectedGold: self.expectedGold:f32 self.totalGold:f32     # SharedData +0x6c +0x68; arg0_f32 = the new value, lr = the cause
upgradeT: self.lvl:i32 self.refNum:i32 self.rate:f32 self.range:f32 self.dmg:i32 self.splash:f32 self.amtWorth:f32
fireTarget: self.timeElapsed:f32 self.currLoops:i32 self.totalLoops:i32 self.rate:f32 self.speedBuff:f32 self.canFire:u8 arg0.hp:f32 arg0.expectedHp:f32
checkCollision self.type:i32 self.damg:i32 self.splash:f32 self.buffDmg:f32 self.buffSplash:f32 self.slowPercent:i32 self.target:id
addCreeps self.sharedData->waveNum:i32 self.sharedData->gSpeed:f32 self.sharedData->numOfWaves:i32 self.sharedData->totalLeft:i32 self.sharedData->totalGold:f32
```

The types come from the decompile's headers (`i` is `i32`, `f` is `f32`, `c` is `u8`, `@` is `id`). The walk through guest memory needs a running game, so it is not covered by unit tests; the grammar and the value formatting are.

## Frames and touch log

These come with the message trace: they are written only while it is on, into the same folder as `trace.log`, and they use its clock (milliseconds since touchHLE started) and its frame numbers, so a row in one file can be found in the others.

### Touch log

`input.log` gets one row per finger for every touch event the game receives. It is replaced each run. Lines starting with `#` are the header (the touchHLE build, the app, the format, and last the column names); every other line is a tab-separated row:

| column | meaning |
| --- | --- |
| `frame` | frames presented so far, as in `trace.log` |
| `ms` | milliseconds since touchHLE started, three decimals, as in `trace.log` |
| `phase` | `down`, `move` or `up`, as the game receives it: a finger reported down while it is already down reaches the game as a move and is logged as `move` (the other fingers of that event never reach the game and are not logged), and a move or up for a finger that is not down is ignored by touchHLE and not logged |
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

What it costs: the read-back (`glReadPixels`) runs on the emulator thread and waits for the GPU to finish the frame, a few milliseconds per saved frame, which the game sees as time passing (its clocks are wall-clock); the `trace.log` header says so. Flipping, encoding and writing happen on one worker thread. If the worker falls eight frames behind, further frames are dropped and the log says how many. When touchHLE exits through UIKit's exit path (Home, closing the window), the frames still queued are written first, waiting at most 3 seconds; a crash or a killed process can lose the last few. A picture is roughly 100-250 KB (an estimate, not yet measured on 4Towers; call it 150 KB on average), so every 10th frame at 30 fps is about 3 frames a second, roughly 27 MB a minute.

`--capture-frames-max=M` caps a run at M saved frames (the default is 3000, about 450 MB at 150 KB a frame, or about 17 minutes of every 10th frame at 30 fps; 0 is no cap). When the cap is reached the capture stops, the log says so once, and no more frames are read back, so the rest of the run is not slowed by it. The trace and the touch log carry on.

Only frames presented straight from a full-screen `CAEAGLLayer` (touchHLE's fast path) are saved. When the game presents through the Core Animation compositor instead (a UIKit view on top of the game's layer, `--force-composition`, or no full-screen layer), no picture is saved and the `frame` number does not advance; touchHLE logs this once. A `png` marker waiting at that point is served by the next fast-path frame.

On Android the `frames` folder is next to `trace.log` in the app's files folder; the standalone launcher's Share Log does not share it yet.

## Builds

The build workflow on this branch clones this fork instead of upstream and also runs on pushes here (and to the `4towers-trace` working branch), so GitHub Actions builds the branch for Windows, macOS and Android. That is the only change to it.

The `4towers-gnu-ci` branch adds a second Windows job, `build-windows-gnu`, which builds the same sources for `x86_64-pc-windows-gnu` with the MinGW-w64 toolchain from MSYS2 and uploads `touchHLE_windows_x86_64_gnu` (the usual Windows bundle plus the MinGW runtime DLLs the exe needs). Every MSVC-built touchHLE, this fork's CI builds and upstream v0.3.0 alike, crashes with an access violation (0xC0000005) a few seconds after 4Towers' title screen on a laptop with Intel Iris Xe graphics, while a GNU-built exe plays the game. The MSVC job is unchanged.
