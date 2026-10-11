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
- Home and back, and closing the window: see [Home and back](#home-and-back) below. Home no longer ends the app.
- Starting at the logo again. A trace run closed in the middle of a level makes the next run start inside that level, so close a trace run from a menu unless the resume is wanted. To start at the logo after all, set `isGameRunning` to false, or remove `savedData`, in the sandbox's `Library/Preferences/com.lambdamugames.4towers.plist`; on Android, clear the app's storage.

## Home and back

iPhone OS 4 keeps an app with multitasking alive when Home is pressed (the game's `Info.plist` has no `UIApplicationExitsOnSuspend`), and touchHLE now does the same. Stock touchHLE, and this branch until round 4, turned the first background event into an exit, so on Android Home ended the game and the next Play started it again from its saved level, without its music (the level theme starts only in `Game::initAll`, and a resumed level goes through `Game::initOthers`).

iPhone OS 4 kept alive only apps built for multitasking. One whose `Info.plist` has `UIApplicationExitsOnSuspend` set, or whose delegate has no `applicationDidEnterBackground:` (an app built before iPhone OS 4), was ended on Home with `applicationWillTerminate:`, and touchHLE still ends such an app as before: when the background event arrives it takes the exit path described below instead of keeping the app in the background. The log says which way it went: "Lifecycle: the app is built for multitasking, so it stays alive.", or "Lifecycle: the app's Info.plist has UIApplicationExitsOnSuspend, so Home ends it." or "Lifecycle: the app delegate has no applicationDidEnterBackground:, so the app is not built for multitasking and Home ends it." 4Towers has the method and not the key, so it stays alive. touchHLE's own app picker has no such method, so Home there ends touchHLE, as it did before round 4. The exit path sends `UIApplicationDidEnterBackgroundNotification` (and `applicationDidEnterBackground:` to an app that opts out with the key but has the method) before `applicationWillTerminate:`, which iPhone OS 4 did not do for such an app; it is kept as the exit has always been.

What the app is told, in order, each message in its own autorelease pool and run to the end before the next:

| When | Delegate method (if it has it) and notification | What touchHLE does as well |
| --- | --- | --- |
| Home, first half | `applicationWillResignActive:`, `UIApplicationWillResignActiveNotification` | |
| Home, second half | `applicationDidEnterBackground:`, `UIApplicationDidEnterBackgroundNotification` | then `[[NSUserDefaults standardUserDefaults] synchronize]`, then pauses every OpenAL device |
| Back, first half | `applicationWillEnterForeground:`, `UIApplicationWillEnterForegroundNotification` | |
| Back, second half | `applicationDidBecomeActive:`, `UIApplicationDidBecomeActiveNotification` | resumes the OpenAL devices first |

Each step is sent once per trip: a repeat is ignored and logged, and a step that arrives without the one before it sends that one first. From the moment the background event arrives until the foreground one does, nothing is presented: `presentRenderbuffer:` returns at once (still keeping the frame rate limit), the Core Animation compositor does nothing and the window is not swapped. The first frame presented in the background is logged, and how many were skipped is logged on the way back. The trace's frame numbers do not advance then. On a computer (F9) the app keeps running between the halves (its timers and run loop carry on). On Android it runs only until SDL suspends touchHLE's thread, soon after the background methods (see below), and from then until Android resumes the activity nothing of the app runs at all.

What 4Towers does with each (from `_Towers_3AppDelegate.c` in the decompile):

- `applicationWillResignActive:` prints "notified!!", sets `justCameBack` if a level is running, and pauses the `CCDirector` (L254-270).
- `applicationDidEnterBackground:` in a level sets `pauseAll`, archives the level into the `savedData` key of its `NSUserDefaults` and prints "saved"; from a menu it calls `saveAll`. Either way it stores `isGameRunning`, calls `synchronize` and stops the `CCDirector`'s animation (L212-247). So the save still happens on Home, and touchHLE's own `synchronize` after it covers Android killing a process in the background without telling it.
- `applicationWillEnterForeground:` sets `justCameBack` again if a level is running and starts the animation (L38-50).
- `applicationDidBecomeActive:` asks GameKit to authenticate (a stub here), resumes the `CCDirector` and turns the idle timer off (L59-86). The level comes back under the game's own pause menu, as on the phone.

The music: cocos2d's `CDAudioManager` can pause and resume the background music itself on these notifications (`CDAudioManager.c` L422-470 and L497-570), but only after `setResignBehavior:autoHandle:` (L604-633), and nothing in the 4Towers decompile calls it. So the game leaves its music playing, and what stops it is touchHLE pausing the OpenAL devices: every device the app opened with `alcOpenDevice`, and the one behind Audio Toolbox, through which `AVAudioPlayer` (via audio queues), audio units and system sounds play. The pause is OpenAL Soft's `ALC_SOFT_pause_device` (`alcDevicePauseSOFT` and `alcDeviceResumeSOFT`, looked up once with `alcGetProcAddress`; if the OpenAL in use lacks them, touchHLE logs it once and audio is not paused). The calls return nothing, and touchHLE reads `alcGetError` afterwards only on its own Audio Toolbox device, as reading the error clears it and the error of a device the app opened is the app's to read. Mixing stops and the host's audio output is stopped, sources keep their state, and on resume they carry on (that is the extension's documented behaviour; on the laptop the music came back, see below, but whether from the same spot was not checked by ear). Audio queues are refilled only as OpenAL uses up buffers, so nothing is refilled while paused. A device the app opens in the background starts paused, and so does the one behind Audio Toolbox if it is first used then (touchHLE makes it on first use).

Closing the window (or quitting touchHLE otherwise), and Android ending the app (`onDestroy`, which SDL turns into a quit and `SDL_APP_TERMINATING`), still exit: the app is sent to the background first if it is not there already (resign, then background, as above), then `applicationWillTerminate:` and `UIApplicationWillTerminateNotification`, a last `synchronize`, the frames the capture still holds, and the exit. After a Home trip the app is not told about the background twice. At its next launch 4Towers finds `isGameRunning` and starts inside the saved level. Checked on 11 October 2026 on a Windows laptop with the CI GNU bundle of 54ce51fa, before the exit was split up: Level 1 was closed at wave 3 with two stations built and six creeps on the board. The log showed "User requested quit, exiting.", the game's "notified!!", touchHLE's "Sending applicationDidEnterBackground: before exiting." (now "Lifecycle: sending the app to the background before exiting." and "Lifecycle: sending applicationDidEnterBackground: to the app delegate."), the game's "saved" twice and "game dealloc", and touchHLE exited by itself in about 5.5 s with code 0. The plist then held `isGameRunning` true and a `savedData` of 28,470 bytes, and the next launch came back inside Level 1 under the game's pause menu (RESUME, RESTART, DATA, OPTIONS, QUIT), with no panic, the scheduled timers (`charging:`, `startMoving:`) ticking. So the archive and its unarchiving work through touchHLE's `NSKeyedArchiver` and `NSKeyedUnarchiver`.

**Where the events come from.** SDL turns the host's events into four app events, which touchHLE maps one to one: `SDL_APP_WILLENTERBACKGROUND` to resign, `SDL_APP_DIDENTERBACKGROUND` to background, `SDL_APP_WILLENTERFOREGROUND` to foreground and `SDL_APP_DIDENTERFOREGROUND` to active; `SDL_APP_TERMINATING` stays the exit. They go into a queue of their own that is handed out before touches.

On Android touchHLE uses SDL's blocking event pump, which suspends touchHLE's thread while the app is in the background. That is SDL's default: SDL 2.26.4 (`vendor/SDL`) reads `SDL_ANDROID_BLOCK_ON_PAUSE` once, when the video subsystem starts (`Android_CreateDevice`, `src/video/android/SDL_androidvideo.c` L112-117: `block_on_pause = SDL_GetHintBoolean(SDL_HINT_ANDROID_BLOCK_ON_PAUSE, SDL_TRUE);`, then `Android_PumpEvents_Blocking` or `Android_PumpEvents_NonBlocking`). Round 4's first cut set the hint to 0 to get the non-blocking pump, but only after `sdl_ctx.video()`, so it never took effect; the line is gone, and the blocking pump is kept on purpose. It is the safer suspension: the emulator does nothing while the app is away, and SDL hands the GL context back before touchHLE sees the foreground events. What happens, in order (line numbers in `src/video/android/SDL_androidevents.c` unless named):

1. On `onPause` (Home), `nativePause` posts a semaphore (`src/core/android/SDL_android.c` L1230-1238). The next event pump on touchHLE's thread sends `SDL_WINDOWEVENT_MINIMIZED`, `SDL_APP_WILLENTERBACKGROUND` and `SDL_APP_DIDENTERBACKGROUND` together (`Android_PumpEvents_Blocking`, L157-165), and does not block yet: "before we block ourselves, we need to make sure that the very last event (of the first pause sequence, if several) has reached the app" (L167-176, `if (SDL_NumberOfEvents(SDL_APP_DIDENTERBACKGROUND) > SDL_SemValue(Android_PauseSem))`).
2. touchHLE takes all three off SDL's queue in the same poll, and then stops pumping SDL until UIKit has handled the background event: the delegate's `applicationWillResignActive:` and `applicationDidEnterBackground:`, then the `synchronize` and the OpenAL pause above ("Lifecycle: received AppDidEnterBackground." is logged when it is queued). Taking an event off SDL's queue is not the same as the app having been told: touchHLE polls from its main loop, also while the guest runs, and the run loop hands events to UIKit only on its next turn. Without that hold SDL could block touchHLE two polls later (polls are at least 1/120 s apart), before or in the middle of the game's save.
3. The first pump after that finds `SDL_APP_DIDENTERBACKGROUND` gone and marks SDL paused (`videodata->isPaused = 1`, L174). The app runs on until the next poll, so this is the one stretch on Android where the present gate above matters: whatever the app draws then is not shown.
4. The pump after that backs up the GL context, "the last thing we do before pausing" (L118-123: `android_egl_context_backup`, L88-100, saves the context current on touchHLE's thread and makes none current, "so the EGLSurface can be freed"), pauses SDL's own audio devices (Android audio, OpenSL ES, AAudio, L126-128; touchHLE plays through OpenAL Soft, which SDL does not know about, hence the device pause above), and blocks in `SDL_SemWait(Android_ResumeSem)` (L130). touchHLE's whole emulator runs on that thread, so from here nothing of the app runs: no timers, no run loop, no audio queue refills.
5. The surface: `surfaceDestroyed` (Java) calls `onNativeSurfaceDestroyed`, which waits for that backup, 50 tries 10 ms apart, and then destroys only the window's EGL surface (`SDL_android.c` L1040-1081). If the app's background methods take longer than the 0.5 s, the surface goes while the context is still current on touchHLE's thread; SDL logs "Try to release egl_surface with context probably still active", and a later switch of context goes through with no surface, making nothing current (`src/video/SDL_egl.c` L1120-1131). A make-current that fails while touchHLE is in the background (on a computer too) is logged once per trip ("Lifecycle: could not make a GL context current in the background (...), skipping.") and leaves no context current, so the GL calls after it do nothing; in the foreground a failure still panics. The EGL context itself is kept, so textures and buffers survive. When the new surface arrives, `onNativeSurfaceCreated` fetches the native window (`SDL_android.c` L998-1014) and `onNativeSurfaceChanged` makes a new EGL surface (L1016-1037). `nativeResume` is only called after that (`SDLActivity.handleNativeState` waits for `mIsSurfaceReady`, the window focus and `onResume`).
6. On `onResume`, `nativeResume` posts the resume semaphore and the blocked pump wakes (L130-155): it sends `SDL_APP_WILLENTERFOREGROUND`, `SDL_APP_DIDENTERFOREGROUND` and `SDL_WINDOWEVENT_RESTORED`, resumes SDL's audio, and makes the saved context current again on the new surface (`android_egl_context_restore`, L71-86, unless `SDL_QUIT` is queued) before it returns. So touchHLE's foreground handlers run with a live context. If the context cannot be made current any more (the system threw it away), SDL makes a new one and sends `SDL_RENDER_DEVICE_RESET`; touchHLE does not handle that, and the game's textures would be gone. This has not been seen.
7. When the foreground event arrives touchHLE also makes no context current (`SDL_GL_MakeCurrent(NULL, NULL)`, logged as "Lifecycle: released the GL context."), so the next GL call, the game's or the compositor's, binds its context to the window's surface as it is then. This was needed with the non-blocking pump, where the app kept running between SDL's backup and its restore: a GL call then made the game's context current with no surface, SDL recorded it as current, and so skipped its restore and every later make-current, and nothing would ever have been drawn to the new surface. With the blocking pump touchHLE's thread is asleep between the two, so that cannot happen; the release is kept, as it costs one make-current and leaves SDL's record right whatever happened in the background. On a computer (F10) it costs the same.

Also:

- `onDestroy` sends `SDL_QUIT` and `SDL_APP_TERMINATING` after flushing the queue, then posts the resume semaphore (`SDL_android.c` L1178-1196), so a pump blocked in the background wakes and sends the foreground pair after them (and does not restore the GL context, as `SDL_QUIT` is queued: `SDL_androidevents.c` L145). touchHLE handles the terminate event first and exits as above. From the first quit or terminate event, and once its exit has started, touchHLE ignores foreground events ("Lifecycle: touchHLE is quitting, ignoring AppWillEnterForeground."), so the window stays in the background and nothing is presented before the process ends.
- Desktop SDL never sends these app events (in SDL 2.26.4 only the Android, iOS and WinRT back ends do), and touchHLE does not treat a window losing focus or being minimized as Home.

**Testing on a computer:** F9 simulates Home (resign, then background), F10 simulates coming back (foreground, then active). Each is logged ("F9 pressed, simulating going to the background."), and F9 while in the background or F10 while not is logged and ignored. F12 (the debugger), Backspace and Return keep their old meanings.

Checked on 11 October 2026 on a Windows laptop with the CI GNU bundles of c687e7f2 (on the title screen) and of 99864497, which releases the GL context on the way back (in Level 1), with `--capture-frames=10`, F9 and F10 sent to the window, and the laptop's audio output recorded through WASAPI loopback in quarter-second steps. In Level 1 before any wave, the level's music measured -16 to -21 dBFS. On F9 the log showed "Lifecycle: received AppWillResignActive.", "Lifecycle: received AppDidEnterBackground.", both delegate methods, the game's "notified!!" and "saved", and "Lifecycle: pausing 2 OpenAL device(s)."; the output was then digital silence for the 5 s recorded, and the captured frames stopped at frame 2480 (the game stops its own animation, so no frame was presented in the background and the "not shown" line never appeared). On F10 the log showed the foreground event, "Lifecycle: released the GL context.", the active event, "Lifecycle: resuming 2 OpenAL device(s).", and both delegate methods; the music was back at once at -16 to -19 dBFS, frames were presented again (2490 to 2660 in the next 5 s), and both the captured frame and the window showed the level under the game's pause menu (RESUME, RESTART, DATA, OPTIONS, QUIT). RESUME went back into the level, and there was no panic. The title screen behaved the same way (its music silent in the background and back after F10). The Android-only parts, SDL's context backup and restore and the surface going away, cannot be seen on a computer.

**In the trace:** with the message trace on, each step writes one `trace.log` row with origin `lifecycle` and the step in the `selector` column: `lifecycle: resign active`, `lifecycle: background`, `lifecycle: foreground`, `lifecycle: active`, and `lifecycle: terminate` before an exit. Like an `nslog` row it has the usual `frame`, `ms` and `thread`, `-` in `variant` and `class` and `-` in the ten columns after the selector. The row is written before the delegate's method is sent, so the method's own row (when its selector is traced, for example `applicationDidEnterBackground:`) and the game's `NSLog` lines ("notified!!", "saved") come after it.

**Not checked on a phone yet:** whether the music comes back after Home on Android, whether the first frame after coming back reaches the new surface, how long Android lets the paused process live, the blocking pump's suspension and wake-up as described above, and whether the background methods (4Towers archives its level in one) and the two polls after them fit in the 0.5 s SDL waits for the context backup (step 5).

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

Every line the game prints with `NSLog` (the lines starting `4Towers_3[0] ` in `touchHLE_log.txt`, which stays as it was) also becomes a `trace.log` row while the trace is on, so the few markers the game prints itself (the wave spawner finishing, pause choices, saves) sit on the trace's timeline. Such a row has the usual `frame`, `ms` and `thread`, `nslog` in the `origin` column, `-` in `variant` and `class`, the message in the `selector` column (tabs, carriage returns and newlines turned into spaces, so a row is always one line), and `-` in the ten columns after it (`arg0` to `self`). A parser that reads `arg0` and the rest as numbers should check the `origin` first. Rows with origin `lifecycle` (going to the background and back, see [Home and back](#home-and-back)) have the same shape.

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
- An item of `--trace-selectors=` takes the same words: `--trace-selectors=setPosition:@CCLabelTTF,setString:@CCLabelTTF;arg0:str`.

The class of each receiver is looked up once per selector, so a filter costs a hash lookup per call after the first.

What each label says and where it is, for 4Towers' HUD:

```
initWithString:dimensions:alignment:fontName:fontSize:@CCLabelTTF arg0:str
setPosition:@CCLabelTTF
```

The first line writes the text of each `CCLabelTTF` made with a box (`labelWithString:dimensions:alignment:fontName:fontSize:` goes through it), with the new label's address in `self`. The `@CCLabelTTF` on it is needed: a `CCLabelTTF` draws its text into a texture, and its `setString:` (which each of its `init`s sends, and which runs again at every change of text) makes that texture by sending the same selector to a new `CCTexture2D` (cocos2d's text category on `CCTexture2D` has `initWithString:dimensions:alignment:fontName:fontSize:` and `initWithString:fontName:fontSize:`). Without the class, each label's creation and each change of its text also writes a row with `CCTexture2D` in the `class` column and the texture's address in `self`, which no row of the label shares. The second line writes each move of a `CCLabelTTF`, and of no other node. `setPosition:` takes a `CGPoint`, which arrives as two floats in r2 and r3, so the `arg0_f32` and `arg1_f32` columns already are its x and y in points, and its `self` is the label's address, which joins it to the label's text. Add `setString:@CCLabelTTF arg0:str` for the changes of text (see the list of label selectors under Peeks).

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

A list for the labels of 4Towers' HUD is below. cocos2d's labels take their text as an `NSString` in the first argument, and every `labelWithString:...` the game calls goes through the matching `initWithString:...`, so these catch each label as it is made (`CCLabelTTF`, then `CCLabelBMFont` and `CCLabelAtlas`), and `setString:` each change of its text. The `self` column of an `init` row is the label's address, which the label's later rows share. Each of these `init`s also sends `setString:` to the new label, which gives a second row with the same text.

```
initWithString:dimensions:alignment:fontName:fontSize:@CCLabelTTF arg0:str
initWithString:fontName:fontSize:@CCLabelTTF arg0:str
initWithString:fntFile:@CCLabelBMFont arg0:str
initWithString:charMapFile:itemWidth:itemHeight:startCharMap:@CCLabelAtlas arg0:str
setString:@CCLabelTTF arg0:str
setString:@CCLabelBMFont arg0:str
setString:@CCLabelAtlas arg0:str
```

Keep the classes (see Receiver class above). Without them the two `CCLabelTTF` `init` lines also take each label's texture: `CCLabelTTF`'s `setString:` sends one of those two selectors to a new `CCTexture2D`, so each label's creation and each change of its text gives one more row, with `CCTexture2D` in the `class` column and the texture's address in `self`, which never joins to the label's rows. A bare `setString:` would also take other receivers: an `NSMutableString`'s (cocos2d's TMX map reader uses one) and a `CCMenuItemLabel`'s, which passes the text on to its label (the label's own `setString:` is a row of its own, with the label's address).

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

What it costs: the read-back (`glReadPixels`) runs on the emulator thread and waits for the GPU to finish the frame, a few milliseconds per saved frame, which the game sees as time passing (its clocks are wall-clock); the `trace.log` header says so. Flipping, encoding and writing happen on one worker thread. If the worker falls eight frames behind, further frames are dropped and the log says how many. When touchHLE exits through UIKit's exit path (closing the window, or Android ending the app), the frames still queued are written first, waiting at most 3 seconds; a crash or a killed process can lose the last few. A picture is roughly 100-250 KB (an estimate, not yet measured on 4Towers; call it 150 KB on average), so every 10th frame at 30 fps is about 3 frames a second, roughly 27 MB a minute.

`--capture-frames-max=M` caps a run at M saved frames (the default is 3000, about 450 MB at 150 KB a frame, or about 17 minutes of every 10th frame at 30 fps; 0 is no cap). When the cap is reached the capture stops, the log says so once, and no more frames are read back, so the rest of the run is not slowed by it. The trace and the touch log carry on.

Only frames presented straight from a full-screen `CAEAGLLayer` (touchHLE's fast path) are saved. When the game presents through the Core Animation compositor instead (a UIKit view on top of the game's layer, `--force-composition`, or no full-screen layer), no picture is saved and the `frame` number does not advance; touchHLE logs this once. A `png` marker waiting at that point is served by the next fast-path frame.

On Android the `frames` folder is next to `trace.log` in the app's files folder; the standalone launcher's Share Log does not share it yet.

## Builds

The build workflow on this branch clones this fork instead of upstream and also runs on pushes here (and to the `4towers-trace` working branch), so GitHub Actions builds the branch for Windows, macOS and Android. That is the only change to it.

The `4towers-gnu-ci` branch adds a second Windows job, `build-windows-gnu`, which builds the same sources for `x86_64-pc-windows-gnu` with the MinGW-w64 toolchain from MSYS2 and uploads `touchHLE_windows_x86_64_gnu` (the usual Windows bundle plus the MinGW runtime DLLs the exe needs). Every MSVC-built touchHLE, this fork's CI builds and upstream v0.3.0 alike, crashes with an access violation (0xC0000005) a few seconds after 4Towers' title screen on a laptop with Intel Iris Xe graphics, while a GNU-built exe plays the game. The MSVC job is unchanged.
