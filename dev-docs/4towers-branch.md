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

iPhone OS 4 kept alive only apps built for multitasking: it ended on Home, with `applicationWillTerminate:`, an app linked against an SDK older than 4.0, or one whose `Info.plist` sets `UIApplicationExitsOnSuspend`. touchHLE reads the key, but does not look at the SDK the binary was linked against; it uses the delegate instead, as its own heuristic: a delegate with no `applicationDidEnterBackground:` marks an app not written for multitasking. For such an app touchHLE takes the exit path described below when the background event arrives, as stock touchHLE did for every app, and nothing changes for 4Towers. The log says which way it went: "Lifecycle: the app is built for multitasking, so it stays alive.", or "Lifecycle: the app's Info.plist has UIApplicationExitsOnSuspend, so Home ends it." or "Lifecycle: the app delegate has no applicationDidEnterBackground:, so touchHLE takes it as not built for multitasking and Home ends it." 4Towers has the method and not the key, so it stays alive. touchHLE's own app picker has no such method, so Home there ends touchHLE, as it did before round 4. The exit path sends `UIApplicationDidEnterBackgroundNotification` (and `applicationDidEnterBackground:` to an app that opts out with the key but has the method) before `applicationWillTerminate:`, which iPhone OS 4 did not do for such an app; it is kept as the exit has always been.

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

On Android the resume crashed: OpenAL Soft 1.22.2's OpenSL backend frees its ring buffer in `OpenSLPlayback::stop()` (`mRing.reset()`) and `start()` begins with `mRing->reset()`, so `alcDeviceResumeSOFT` dereferenced a null pointer (seen 12 October 2026 on the Android emulator, API 30, x86_64: `RingBuffer::reset()+1` under `OpenSLPlayback::start()` under `alcDeviceResumeSOFT`). The `openal-soft` submodule now points at the fork `TechnoidAlpha/openal-soft`, branch `4towers`, one commit on top of the pinned `23c8a355`: `stop()` clears the buffer with `mRing->reset()` instead, which is what upstream's current `opensl.cpp` does. Only the OpenSL backend is touched; the desktop backends never freed the buffer in `stop()`.

Closing the window (or quitting touchHLE otherwise), and Android ending the app (`onDestroy`, which SDL turns into a quit and `SDL_APP_TERMINATING`), still exit: the app is sent to the background first if it is not there already (resign, then background, as above), then `applicationWillTerminate:` and `UIApplicationWillTerminateNotification`, a last `synchronize`, the frames the capture still holds, and the exit. After a Home trip the app is not told about the background twice. At its next launch 4Towers finds `isGameRunning` and starts inside the saved level. Checked on 11 October 2026 on a Windows laptop with the CI GNU bundle of 54ce51fa, before the exit was split up: Level 1 was closed at wave 3 with two stations built and six creeps on the board. The log showed "User requested quit, exiting.", the game's "notified!!", touchHLE's "Sending applicationDidEnterBackground: before exiting." (now "Lifecycle: sending the app to the background before exiting." and "Lifecycle: sending applicationDidEnterBackground: to the app delegate."), the game's "saved" twice and "game dealloc", and touchHLE exited by itself in about 5.5 s with code 0. The plist then held `isGameRunning` true and a `savedData` of 28,470 bytes, and the next launch came back inside Level 1 under the game's pause menu (RESUME, RESTART, DATA, OPTIONS, QUIT), with no panic, the scheduled timers (`charging:`, `startMoving:`) ticking. So the archive and its unarchiving work through touchHLE's `NSKeyedArchiver` and `NSKeyedUnarchiver`.

**Where the events come from.** SDL turns the host's events into four app events, which touchHLE maps one to one: `SDL_APP_WILLENTERBACKGROUND` to resign, `SDL_APP_DIDENTERBACKGROUND` to background, `SDL_APP_WILLENTERFOREGROUND` to foreground and `SDL_APP_DIDENTERFOREGROUND` to active; `SDL_APP_TERMINATING` stays the exit. They go into a queue of their own that is handed out before touches.

On Android touchHLE uses SDL's blocking event pump, which suspends touchHLE's thread while the app is in the background. That is SDL's default: SDL 2.26.4 (`vendor/SDL`) reads `SDL_ANDROID_BLOCK_ON_PAUSE` once, when the video subsystem starts (`Android_CreateDevice`, `src/video/android/SDL_androidvideo.c` L112-117: `block_on_pause = SDL_GetHintBoolean(SDL_HINT_ANDROID_BLOCK_ON_PAUSE, SDL_TRUE);`, then `Android_PumpEvents_Blocking` or `Android_PumpEvents_NonBlocking`). Round 4's first cut set the hint to 0 to get the non-blocking pump, but only after `sdl_ctx.video()`, so it never took effect; the line is gone, and the blocking pump is kept on purpose. It is the safer suspension: the emulator does nothing while the app is away, and SDL hands the GL context back before touchHLE sees the foreground events. What happens, in order (line numbers in `src/video/android/SDL_androidevents.c` unless named):

1. Android tells SDL to pause from its UI thread, and when depends on the version. From Android 7 (API 24), which covers every phone that matters (`minSdk` is 21), `SDLActivity` pauses the native thread in `onStop`, not in `onPause`: `mHasMultiWindow = (Build.VERSION.SDK_INT >= 24)` (`SDLActivity.java` L173), `onPause` pauses only without it (`SDLActivity.java` L456-467) and `onStop` only with it (`SDLActivity.java` L482-489). The surface can go first: `SDLSurface.surfaceDestroyed` sets the paused state itself (`SDLSurface.java` L92-100). Below API 24 the pause comes in `onPause`, and also when the window loses focus (`onWindowFocusChanged(false)`, `SDLActivity.java` L530-551), so there a notification shade or a system dialog sends the app the whole trip: it saves, and SDL blocks touchHLE until the focus comes back. Either way `nativePause` posts a semaphore (`src/core/android/SDL_android.c` L1230-1238). The next event pump on touchHLE's thread sends `SDL_WINDOWEVENT_MINIMIZED`, `SDL_APP_WILLENTERBACKGROUND` and `SDL_APP_DIDENTERBACKGROUND` together (`Android_PumpEvents_Blocking`, L157-165), and does not block yet: "before we block ourselves, we need to make sure that the very last event (of the first pause sequence, if several) has reached the app" (L167-176, `if (SDL_NumberOfEvents(SDL_APP_DIDENTERBACKGROUND) > SDL_SemValue(Android_PauseSem))`).
2. touchHLE takes all three off SDL's queue in the same poll and stops taking events the moment the background event is queued (a poll after SDL's end-of-poll sentinel has gone, which can happen after a touch event, pumps again, `src/events/SDL_events.c` L1044-1046). From then it does not poll SDL at all until UIKit has handled the background event: the delegate's `applicationWillResignActive:` and `applicationDidEnterBackground:`, then the `synchronize` and the OpenAL pause above ("Lifecycle: received AppDidEnterBackground." is logged when it is queued). Taking an event off SDL's queue is not the same as the app having been told: touchHLE polls from its main loop, also while the guest runs, and the run loop hands events to UIKit only on its next turn. Without that hold SDL could block touchHLE two polls later (polls are at least 1/120 s apart), before or in the middle of the game's save.
3. The first pump after that finds `SDL_APP_DIDENTERBACKGROUND` gone and marks SDL paused (`videodata->isPaused = 1`, L174). The app runs on until the next poll. The present gate above is in force for longer than that, though: `backgrounded` is set when the background event is queued, inside a poll that `run_inner` makes in the middle of whatever the guest is doing, and UIKit only handles the event at the start of the main run loop's next turn (`src/frameworks/foundation/ns_run_loop.rs` L349-351). So the gate covers the rest of the current timer or display link callback (which may present a frame), the resign and background methods themselves, and this last stretch: whatever the app draws in any of them is not shown.
4. The pump after that backs up the GL context, "the last thing we do before pausing" (L118-123: `android_egl_context_backup`, L88-100, saves the context current on touchHLE's thread and makes none current, "so the EGLSurface can be freed"), pauses SDL's own audio devices (Android audio, OpenSL ES, AAudio, L126-128; touchHLE plays through OpenAL Soft, which SDL does not know about, hence the device pause above), and blocks in `SDL_SemWait(Android_ResumeSem)` (L130). touchHLE's whole emulator runs on that thread, so from here nothing of the app runs: no timers, no run loop, no audio queue refills.
5. The surface: `surfaceDestroyed` (Java) calls `onNativeSurfaceDestroyed`, which waits for that backup, 50 tries 10 ms apart, and then destroys only the window's EGL surface (`SDL_android.c` L1040-1081). `surfaceDestroyed` sets the paused state (posting the pause semaphore if `onStop` has not already) and calls `onNativeSurfaceDestroyed` straight after, so the 0.5 s starts at, or just after, the pause signal. It has to cover touchHLE reaching its next poll, the rest of the run loop's current turn, the resign and background methods (4Towers archives its level with `NSKeyedArchiver` in one), the `synchronize`, the OpenAL pause and the two further pumps. If they take longer, the surface goes while the context is still current on touchHLE's thread. SDL does not say so where a tester would see it: "Try to release egl_surface with context probably still active" is only an `SDL_SetError` on the Java UI thread (`SDL_android.c` L1057), printed only when SDL's error log category is at debug level (`src/SDL_error.c` L56; the default is critical, `src/SDL_log.c` L48). So touchHLE logs how long the app took: "Lifecycle: the background event was handled N ms after it arrived." The event arrives on the first pump after the pause signal, and SDL's backup comes three polls (at least 1/60 s: one drains what was left before SDL's sentinel without a pump, one pumps and marks the pause, one backs up and blocks) after that line, so an N of about 480 or more means the surface probably went first; N starts when the event was queued, so it leaves out the time from Android's pause signal to SDL's first pump. Then a later switch of context still succeeds as far as SDL is concerned: if the device cannot make a context current without a surface, SDL makes nothing current (`src/video/SDL_egl.c` L1120-1124); if it can (EGL 1.5 or `EGL_KHR_surfaceless_context`, plus `GL_OES_surfaceless_context`, as on most current phones, `SDL_egl.c` L1064-1076), SDL makes the context current with no surface. Either way `SDL_GL_MakeCurrent` returns 0 and records the context as current (`src/video/SDL_video.c` L4120-4125), and GL calls draw nowhere. So a make-current fails only on a real `eglMakeCurrent` error, for example a stale surface handle while the Java thread is destroying it (`Android_GLES_MakeCurrent` reads the window's `egl_surface` without `Android_ActivityMutex`, `src/video/android/SDL_androidgl.c` L40-47). Such a failure while touchHLE is in the background (on a computer too), for the game's contexts or for touchHLE's own compositor context (which a composite can reach after the background event, when a guest callback in the middle of it lets a poll queue the event), is logged once per trip ("Lifecycle: could not make a GL context current in the background (...), skipping.") and leaves no context current, so the GL calls after it do nothing; in the foreground a failure still panics. The EGL context itself is kept, so textures and buffers survive. When the new surface arrives, `onNativeSurfaceCreated` fetches the native window (`SDL_android.c` L998-1014) and `onNativeSurfaceChanged` makes a new EGL surface (L1016-1037). `nativeResume` is only called after that (`SDLActivity.handleNativeState` waits for `mIsSurfaceReady`, the window focus and `resumeNativeThread`, which is `onStart` from API 24 and `onResume` below it, `SDLActivity.java` L443-453).
6. When the activity comes back (see step 5), `nativeResume` posts the resume semaphore and the blocked pump wakes (L130-155): it sends `SDL_APP_WILLENTERFOREGROUND`, `SDL_APP_DIDENTERFOREGROUND` and `SDL_WINDOWEVENT_RESTORED`, resumes SDL's audio, and makes the saved context current again on the new surface (`android_egl_context_restore`, L71-86, unless `SDL_QUIT` is queued) before it returns. So touchHLE's foreground handlers run with a live context. If the context cannot be made current any more (the system threw it away), SDL makes a new one and sends `SDL_RENDER_DEVICE_RESET`; touchHLE does not handle that, and the game's textures would be gone. This has not been seen.
7. When the foreground event arrives touchHLE also makes no context current (`SDL_GL_MakeCurrent(NULL, NULL)`, logged as "Lifecycle: released the GL context."), so the next GL call, the game's or the compositor's, binds its context to the window's surface as it is then. This was needed with the non-blocking pump, where the app kept running between SDL's backup and its restore: a GL call then made the game's context current with no surface, SDL recorded it as current, and so skipped its restore and every later make-current, and nothing would ever have been drawn to the new surface. With the blocking pump touchHLE's thread is asleep between the two, so that cannot happen; the release is kept, as it costs one make-current and leaves SDL's record right whatever happened in the background. On a computer (F10) it costs the same.

Also:

- `onDestroy` sends `SDL_QUIT` and `SDL_APP_TERMINATING` after flushing the queue, then posts the resume semaphore (`SDL_android.c` L1178-1196), so a pump blocked in the background wakes and sends the foreground pair after them (and does not restore the GL context, as `SDL_QUIT` is queued: `SDL_androidevents.c` L145). touchHLE handles the terminate event first and exits as above. From the first quit or terminate event, and once its exit has started, touchHLE ignores foreground events ("Lifecycle: touchHLE is quitting, ignoring AppWillEnterForeground."), so the window stays in the background and nothing is presented before the process ends.
- Desktop SDL never sends these app events (in SDL 2.26.4 only the Android, iOS and WinRT back ends do), and touchHLE does not treat a window losing focus or being minimized as Home.

**Testing on a computer:** F9 simulates Home (resign, then background), F10 simulates coming back (foreground, then active). Each is logged ("F9 pressed, simulating going to the background."), and F9 while in the background or F10 while not is logged and ignored. F12 (the debugger), Backspace and Return keep their old meanings.

Checked on 11 October 2026 on a Windows laptop with the CI GNU bundles of c687e7f2 (on the title screen) and of 99864497, which releases the GL context on the way back (in Level 1), with `--capture-frames=10`, F9 and F10 sent to the window, and the laptop's audio output recorded through WASAPI loopback in quarter-second steps. In Level 1 before any wave, the level's music measured -16 to -21 dBFS. On F9 the log showed "Lifecycle: received AppWillResignActive.", "Lifecycle: received AppDidEnterBackground.", both delegate methods, the game's "notified!!" and "saved", and "Lifecycle: pausing 2 OpenAL device(s)."; the output was then digital silence for the 5 s recorded, and the captured frames stopped at frame 2480 (the game stops its own animation, so no frame was presented in the background and the "not shown" line never appeared). On F10 the log showed the foreground event, "Lifecycle: released the GL context.", the active event, "Lifecycle: resuming 2 OpenAL device(s).", and both delegate methods; the music was back at once at -16 to -19 dBFS, frames were presented again (2490 to 2660 in the next 5 s), and both the captured frame and the window showed the level under the game's pause menu (RESUME, RESTART, DATA, OPTIONS, QUIT). RESUME went back into the level, and there was no panic. The title screen behaved the same way (its music silent in the background and back after F10). The Android-only parts, SDL's context backup and restore and the surface going away, cannot be seen on a computer.

**In the trace:** with the message trace on, each step writes one `trace.log` row with origin `lifecycle` and the step in the `selector` column: `lifecycle: resign active`, `lifecycle: background`, `lifecycle: foreground`, `lifecycle: active`, and `lifecycle: terminate` before an exit. Like an `nslog` row it has the usual `frame`, `ms` and `thread`, `-` in `variant` and `class` and `-` in the ten columns after the selector. The row is written before the delegate's method is sent, so the method's own row (when its selector is traced, for example `applicationDidEnterBackground:`) and the game's `NSLog` lines ("notified!!", "saved") come after it.

**Not checked on a phone yet:** whether the music comes back after Home on Android, whether the first frame after coming back reaches the new surface, how long Android lets the paused process live, the blocking pump's suspension and wake-up as described above, and whether everything from the pause signal to SDL's context backup (the rest of the run loop's turn, the background methods with 4Towers' level archive, and two more polls) fits in the 0.5 s SDL waits for it (step 5; the "handled N ms after it arrived" line answers it).

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

## OpenGL ES

- **No texture image without pixels.** When the game calls `glTexImage2D` with a width or height of 0 (the other not negative), touchHLE uploads one texel instead: 1 by 1, every byte 0, in the format and type the game gave, so the texture is complete and samples as transparent (as black for `RGB` and `LUMINANCE`, which have no alpha). `glCompressedTexImage2D` with a side of 0 gets the same texel as `RGBA` with `UNSIGNED_BYTE`, as a compressed format has no 1 by 1 image. The first time in a run it logs "GLES: glTexImage2D of a 0x0 image (format ...) uploads one transparent texel instead, ..."; later ones are not logged. The game's own record of the texture is not touched: cocos2d's `CCTexture2D` keeps the 0 by 0 it asked for, so a scene dump's `tex_w` and `tex_h` still say 0 (and `tex_max_s`, `tex_max_t` `nan`).
  - Why: an empty `CCLabelTTF` has a 0 by 0 texture (`-[CCTexture2D initWithString:...]` makes one for an empty string; in 4Towers, the wave label before the first wave). On the Fold 7, Qualcomm's own OpenGL ES 1.1 driver ("OpenGL ES-CM 1.1 / Qualcomm / Adreno (TM) 830") then draws the next textured quad, the gold number, as a solid block; through Android's ANGLE the same frames draw right. The game repo's `docs/TOUCHHLE.md` (Known Visual Quirks, "The white box over the gold number") has the dumps.
  - Where: in the functions the guest calls (`src/frameworks/opengles/gles_guest.rs`), which every call goes through on the way to either backend (`gles1_native.rs`, the host's OpenGL ES 1.1, used on Android, and `gles1_on_gl2.rs`, OpenGL 2.1 on a computer), so one check covers both.
  - `glTexSubImage2D` is left as it is: an update of 0 by 0 changes no texel and makes no image.
  - Unit tests cover which sizes are replaced (0 on either side; not 1 by 1, not a negative size, which still gets the host's error) and that the texel is zeros and big enough for the widest format. The GL calls themselves need a context and are not tested. Not yet seen on the Fold 7.

## Message trace

`--trace-selectors=` takes a comma-separated list of Objective-C selectors, for example `--trace-selectors=schedule:interval:,setPosition:`. Every time the game (or touchHLE itself) calls one of those methods, one line goes into `trace.log`: the frame number, milliseconds since start, the thread, who sent it, how it was called (the `variant` column), the receiver's class, the selector, the first two arguments (as hex and as a float), four words from the stack, the return address and, in the last column (`self`), the receiver's address, which tells two objects of the same class apart. The first lines of the file say which touchHLE build, which app and which selectors. It is there to time things the decompile cannot, like how long a station takes to charge.

Two kinds of call are traced. Ordinary message sends go through `objc_msgSend` and show up as `send` (or `stret`, `super2`). Methods run by cocos2d's scheduler (`schedule:interval:` timers such as `charging:` or `tLoop:`) do not: the scheduler asks for the method's address once with `methodForSelector:` and calls it directly from then on. For a traced selector, `methodForSelector:` and `instanceMethodForSelector:` hand back a small stub instead, which writes the line and then jumps into the real method, so those calls show up as `imp`. A method that returns a struct or a union is left unwrapped and its `imp` calls are not traced, because such a method can be called with the return buffer in the receiver's register and the stub cannot tell; touchHLE logs it once. Nothing changes for selectors that are not traced, or when the trace is off.

Every line the game prints with `NSLog` (the lines starting `4Towers_3[0] ` in `touchHLE_log.txt`, which stays as it was) also becomes a `trace.log` row while the trace is on, so the few markers the game prints itself (the wave spawner finishing, pause choices, saves) sit on the trace's timeline. Such a row has the usual `frame`, `ms` and `thread`, `nslog` in the `origin` column, `-` in `variant` and `class`, the message in the `selector` column (tabs, carriage returns and newlines turned into spaces, so a row is always one line), and `-` in the ten columns after it (`arg0` to `self`). A parser that reads `arg0` and the rest as numbers should check the `origin` first. Rows with origin `lifecycle` (going to the background and back, see [Home and back](#home-and-back)) have the same shape.

You can also list selectors one per line in `trace_selectors.txt` next to the other touchHLE files (`#` starts a comment). The option and the file add up. A selector can be narrowed to one class of receiver (`setPosition:@CCLabelTTF`, see Receiver class below). A line in the file can carry marker words and peeks after the selector, separated by spaces: the markers are `png` (see Frames below) and `scene` (see Scene dump below), and peeks are described below. A word that is neither is ignored and logged once. With no selectors, and without `--capture-frames=` or `--dump-scene-every=`, nothing is traced and nothing is written. `trace.log` is replaced each run.

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

### Profile row

The `profile` marker (`initOthers@Game profile`) writes one row with origin `profile` at each call, before the method runs, laid out like an `nslog` row. Its selector column holds one line of JSON read from the game's `SharedData` singleton without calling the game: `kind` (`profile`), `addr`, then `lvlNum` and `gameMode`, the buff multipliers `dmgBuffed`, `rangeBuffed`, `speedBuffed`, `splashBuffed` and `utilBuffed`, the perk flags `fireBonus`, `fireCrit`, `fireMissle`, `splashDmgBonus`, `splashExtraGold`, `splashReduceCost`, `snipeDmg`, `snipeStun` and `snipeReduceCost` (0 or 1), and the arrays of numbers `warningCreeps`, `unlockArray`, `lvlArray`, `lvlArray1`, `lvlArray2`, `equipmentArray` and `tricketArray`. A field that cannot be read is `null`; with no `SharedData` found the row is `{"kind":"profile","error":"no SharedData found"}`. The live mirror reads the level start's profile and gear from it instead of a save file and a plan (LIVE8 of the game repo's `docs/plans/SIDE_BY_SIDE.md`).

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
| `finger` | which finger: `Mouse`, `Touch(n)` (n is the host's finger id), `VirtualCursor`, `ButtonToTouch(button)`, `StickToTouch`, `DpadToTouch`, or `Script` for a touch from the input script (see [Scripted input](#scripted-input)) |
| `x`, `y` | the position in guest points (the screen is 320 x 480 points), unrotated: what the game's `UITouch` reports, whatever the window's rotation or scale |

An event with two fingers gives two rows with the same `frame`, `ms` and `phase`. The rows are written as the events reach the game's run loop, each event in one write, so a crash keeps every row before it.

### Frames

Three ways to save the picture the game presented, as PNG files:

- `--capture-frames=N` (N of 1 or more; absent or 0 is off) saves every Nth frame: frames 0, N, 2N and so on. Like any option it can go on the game's line in `touchHLE_options.txt`. It turns the trace on by itself, so `trace.log` and `input.log` are written even with no selectors.
- A `png` marker on a line of `trace_selectors.txt`, for example `powerFire png`, saves the first frame presented after each call of that selector: the frame that shows its result. Several calls before the next frame give one picture.
- A scene dump (see Scene dump below) saves the picture of the frame it describes.

Each picture is `frames/f{frame}_{ms}.png` in the same folder as `trace.log`, for example `frames/f000042_15873.png`:

- `frame` has at least six digits and is the `frame` column of the `trace.log` (and `input.log`) rows whose drawing the picture shows. The first frame presented is frame 0. A picture saved for a `png` selector carries the same frame number as the row of the call that asked for it.
- `ms` is the whole milliseconds since touchHLE started (the `ms` clock of `trace.log`, cut to a whole number) when the frame was presented.

The picture is the game's own renderbuffer as the game drew it, 8-bit RGB with the alpha channel dropped: 320 x 480 for 4Towers, not rotated, without the window's letterbox or the virtual cursor. With `--scale-hack=N` it is the scaled renderbuffer, N times as wide and N times as high. Earlier pictures (files named like these) are removed from `frames/` when a run that saves frames starts, so the folder matches that run's `trace.log`.

What it costs: the read-back (`glReadPixels`) runs on the emulator thread and waits for the GPU to finish the frame, a few milliseconds per saved frame, which the game sees as time passing (its clocks are wall-clock); the `trace.log` header says so. Flipping, encoding and writing happen on one worker thread. If the worker falls eight frames behind, further frames are dropped and the log says how many. When touchHLE exits through UIKit's exit path (closing the window, or Android ending the app), the frames still queued are written first, waiting at most 3 seconds; a crash or a killed process can lose the last few. A picture is roughly 100-250 KB (an estimate, not yet measured on 4Towers; call it 150 KB on average), so every 10th frame at 30 fps is about 3 frames a second, roughly 27 MB a minute.

`--capture-frames-max=M` caps a run at M saved frames (the default is 3000, about 450 MB at 150 KB a frame, or about 17 minutes of every 10th frame at 30 fps; 0 is no cap). When the cap is reached the capture stops, the log says so once, and no more frames are read back, so the rest of the run is not slowed by it. The trace and the touch log carry on.

Only frames presented straight from a full-screen `CAEAGLLayer` (touchHLE's fast path) are saved. When the game presents through the Core Animation compositor instead (a UIKit view on top of the game's layer, `--force-composition`, or no full-screen layer), no picture is saved and the `frame` number does not advance; touchHLE logs this once. A `png` marker waiting at that point is served by the next fast-path frame.

On Android the `frames` folder is next to `trace.log` in the app's files folder; the standalone launcher's Share Log does not share it yet.

## Builds

The build workflow on this branch clones this fork instead of upstream and also runs on pushes here (and to the `4towers-trace` and `4towers-instruments` working branches), so GitHub Actions builds the branch for Windows, macOS and Android. That is the only change to it.

The `4towers-gnu-ci` branch adds a second Windows job, `build-windows-gnu`, which builds the same sources for `x86_64-pc-windows-gnu` with the MinGW-w64 toolchain from MSYS2 and uploads `touchHLE_windows_x86_64_gnu` (the usual Windows bundle plus the MinGW runtime DLLs the exe needs). Every MSVC-built touchHLE, this fork's CI builds and upstream v0.3.0 alike, crashes with an access violation (0xC0000005) a few seconds after 4Towers' title screen on a laptop with Intel Iris Xe graphics, while a GNU-built exe plays the game. The MSVC job is unchanged.

## Scene dump

The trace shows calls and fields, and the frames show pixels. A scene dump says what is on screen as cocos2d objects: one line per node of the running scene, with its class, place, scale, texture (and the file it came from), a label's text and more, read from the game's memory at the moment a frame is presented. It answers where each HUD element sits and which texture rect a sprite shows, and it names a node whose texture is missing.

### Asking for one

All three need the message trace on (see Message trace above), and all three do the same thing at the next frame:

- **F11** on a computer. On Android a key event with `KEYCODE_F11`, which SDL turns into the same key, does the same, so the standalone launcher can send it as it sends F9 and F10. Each press is logged ("F11 pressed, DumpScene event queued." and "Scene dump asked for (key F11), taken when the next frame is presented"). With the trace off, F11 writes nothing and that is logged once.
- **The `scene` marker** on a line of `trace_selectors.txt`, like `png`: `startWave scene` dumps the scene at the first frame presented after each call of `startWave`. It follows the line's receiver class (`setString:@CCLabelTTF scene`), and in `--trace-selectors=` it is written `startWave;scene`.
- **`--dump-scene-every=N`** (N of 1 or more; absent or 0 is off) dumps frames 0, N, 2N and so on. Like `--capture-frames=`, it turns the trace on by itself, and it can go on the game's line in `touchHLE_options.txt`.

A dump is taken when the next frame is presented on the CAEAGLLayer fast path, the same moment a `png` marker's picture is taken, so the dump shows the state the frame was drawn from and never the middle of a call. Several requests before one frame give one dump. A frame presented through the Core Animation compositor gives no dump, and a request waiting then is served by the next fast-path frame.

### What each dump writes

1. A `trace.log` row with origin `mark`, laid out like an `nslog` row: the usual `frame`, `ms` and `thread`, `-` in `variant` and `class`, then in the `selector` column `scene:`, the reasons and the file, for example `scene: key F11 (scene/s000042_15873.txt)`, and `-` in the ten columns after it. The reasons are `key F11`, `selector <name>` and `periodic`, joined by `, ` when one frame has several.
2. The frame's picture, `frames/f{frame:06}_{ms}.png`, as a `png` marker saves it (see Frames above). A run with neither `--capture-frames=` nor a `png` marker starts the frame capture at its first dump (which empties `frames/` of an earlier run's pictures, once). `--capture-frames-max=` still applies: past the cap a dump has no picture.
3. The dump, `scene/s{frame:06}_{ms}.txt`, in a `scene` folder next to `frames/` (the app's files folder on Android). `frame` and `ms` are those of the picture, so `s000042_15873.txt` goes with `f000042_15873.png` and with the `trace.log` rows of frame 42. The first dump of a run removes the dumps an earlier run left in that folder.

### The file

Tab-separated text:

- Line 1 names the columns (below). It does not start with `#`.
- Line 2 starts with `# ` and holds `key=value` pairs, separated by tabs: `frame`, `ms`, `trigger` (the reasons), `director` (address and class, for example `0x0012a4c0:CCDirectorTimer`), `paused` (its `isPaused_`), `running_scene` and `next_scene` (`runningScene_` and `nextScene_`, address and class, or `nil`), `win_size` (`winSizeInPoints_`, width and height), `viewport` (the window's area that shows the game, `x,y,width,height` in host pixels) and `textures` (how many textures the texture cache holds). `-` for what cannot be read.
- Then one line per node.
- Then one line per texture in the texture cache, sorted by key: `# cache: <key>\t<tex_name>\t<w>x<h>`. The key is the file name the game cached the texture under (written as `text` is, see Text below; a key that is not one of touchHLE's strings is its address), `tex_name` the `CCTexture2D`'s `name_` (its GL texture name) and `w`, `h` its `width_` and `height_`, so a node's `tex_name` can be matched to a cached file. `-` for what cannot be read, and for all three when the value is not a `CCTexture2D`. There are as many as `textures=` says, and no cap; a dump without a texture cache has none.
- Then notes, each on a line starting with `# `: what was cut by a cap, and children that could not be read.
- Last, `# nodes=N`, the number of node lines.

To read it: take the first line as the column names and skip every other line that starts with `#`. A node line never starts with `#` (it starts with spaces or a class name). The texture cache is the lines that start with `# cache: `, split on tabs.

The nodes come in depth-first order from the director's `runningScene_` (depth 0): each node, then its children, each child's own children before the next child. Children are in the order of the node's `children_` array, which cocos2d keeps sorted by `zOrder_` (ties in the order they were added). cocos2d draws a node's children with a negative `z` before the node itself and the others after it.

The columns, in order. `f32` is written with six significant digits as in peeks, integers in decimal, addresses as `0x` and eight hex digits:

| column | type | read from | for |
| --- | --- | --- | --- |
| `class` | text | the node's isa, indented by two spaces per level | every node |
| `depth` | integer | levels below the running scene | every node |
| `addr` | address | the node's address, the same as the trace's `self` column | every node |
| `tag` | i32 | `tag_` | every node |
| `z` | i32 | `zOrder_` | every node |
| `x`, `y` | f32 | `position_` (a `CGPoint`, points, in the parent's space) | every node |
| `anchor_x`, `anchor_y` | f32 | `anchorPoint_` | every node |
| `rel_anchor` | u8 | `isRelativeAnchorPoint_` | every node |
| `scale_x`, `scale_y` | f32 | `scaleX_`, `scaleY_` | every node |
| `rotation` | f32 | `rotation_` (degrees) | every node |
| `visible` | u8 | `visible_` | every node |
| `width`, `height` | f32 | `contentSize_` (a `CGSize`, points) | every node |
| `opacity` | u8 | `opacity_` | `CCSprite`, `CCAtlasNode`, `CCLabelBMFont`, `CCLayerColor` |
| `r`, `g`, `b` | u8 | the colour the game set, as `color` returns it: `colorUnmodified_` when the node has it and `opacityModifyRGB_` is on, otherwise `color_` (a `ccColor3B`) | the same |
| `opacity_rgb` | u8 | `opacityModifyRGB_`: 1 when cocos2d draws the colour multiplied by the opacity | `CCSprite`, `CCAtlasNode`, `CCLabelBMFont` (`-` for `CCLayerColor`) |
| `flip_x`, `flip_y` | u8 | `flipX_`, `flipY_` | `CCSprite` |
| `rect_x`, `rect_y`, `rect_w`, `rect_h` | f32 | `rect_` (a `CGRect`, the part of the texture shown, in points) | `CCSprite` |
| `batch` | u8 | `usesBatchNode_` | `CCSprite` |
| `tex` | address | the texture: `nil` if there is none | `CCSprite`, `CCAtlasNode`, `CCSpriteBatchNode` |
| `tex_name` | u32 | the texture's `name_`, its GL texture name | the same |
| `tex_w`, `tex_h` | u32 | `width_`, `height_`: the texture's size in pixels | the same |
| `tex_size_w`, `tex_size_h` | f32 | `size_`: the size of the picture in it, in pixels | the same |
| `tex_format` | i32 | `format_`, cocos2d's `CCTexture2DPixelFormat` as stored | the same |
| `tex_max_s`, `tex_max_t` | f32 | `maxS_`, `maxT_`: `nan` for a 0 by 0 texture (an empty `CCLabelTTF`'s), as cocos2d divides by its size | the same |
| `tex_file` | text | the key the texture cache holds the texture under: the file name the game asked for | the same |
| `text` | text | `string_` | `CCLabelTTF`, `CCLabelAtlas`, `CCLabelBMFont` |
| `font` | text | `fontName_` | `CCLabelTTF` |
| `font_size` | f32 | `fontSize_` | `CCLabelTTF` |
| `dim_w`, `dim_h` | f32 | `dimensions_` (the label's box, 0 by 0 for none) | `CCLabelTTF` |

**The `-` rule.** Which columns apply is decided by the node's class chain: a column whose "for" names a class applies when the node's class is that class or a subclass of it (the names are matched as in Receiver class above). A column that does not apply is `-`. One that applies but cannot be read is also `-`: a field the class chain does not have, a nil or unreadable pointer, an object whose class touchHLE does not know. A text that is nil is `nil`. The walk goes on after a `-`.

**The texture.** A `CCSprite` drawn by a batch node (`usesBatchNode_` set) shows the batch's texture, read through its `textureAtlas_` (a `CCTextureAtlas`) and that atlas's `texture_`; any other sprite (a `CCLabelTTF` is one) shows its own `texture_`. A `CCAtlasNode` (so `CCLabelAtlas`) and a `CCSpriteBatchNode` (so `CCLabelBMFont`) show their `textureAtlas_`'s `texture_`. If what is found is not a `CCTexture2D`, `tex` has its address and the other texture columns are `-`. `tex_file` is `-` for a texture the cache does not hold: a `CCLabelTTF`'s text is drawn into a texture of its own that never goes into the cache, and so is any texture the game made from an image it built itself. A sprite with `tex` of `nil`, or a texture with a `tex_name` of 0, is one that draws without a picture.

**The colour.** A `CCSprite` (so a `CCLabelTTF`) and a `CCAtlasNode` start with `opacityModifyRGB_` on. Then `setColor:` keeps the colour in `colorUnmodified_` and stores `color_` multiplied by `opacity_ / 255`, and `setOpacity:` does the same again (`CCSprite.c` and `CCAtlasNode.c` in the decompile), so a white sprite at opacity 128 has a `color_` of 128,128,128. `r`, `g` and `b` are the colour as set (255,255,255 there), as `[sprite color]` returns it; the colour drawn is that times `opacity` / 255 when `opacity_rgb` is 1. A `CCLabelBMFont` has no `colorUnmodified_` and keeps the colour as set in `color_`.

**Text.** `text`, `font` and `tex_file` are written as a `str` peek writes a string (see Peeks above): tabs, newlines, carriage returns and backslashes escaped, cut at 200 bytes with `...`. A string of a class the game defines itself is `-`. An empty string (an empty label's text) is an empty field, nothing between the two tabs: neither `nil` nor `-`.

**Caps.** At most 5,000 node lines per dump; then the walk stops and a note says how many were still waiting. Nodes 32 levels below the running scene or deeper are not written (levels 0 to 31 are), and a note says how many. A node met twice (a loop in the children) is written once, with a note. At most 50 notes, then `more notes left out`; the notes that say a cap cut the walk (they start with `cut:`) come last and are always written.

### How it reads the game

Only memory and touchHLE's class table are read: the dump never sends a message to the game or calls into it. Every field is found by its ivar name on the object's class chain (as peeks are), so the offsets come from the binary's Objective-C metadata, and the types come from cocos2d 0.99.5's headers and the decompile (`CCNode.c`, `CCSprite.c`, `CCTexture2D.c`, `CCLabelTTF.c`, `CCDirector.c`); a wrong type would give a wrong number, not a `-`.

- **`children_`** is a `CCArray`. Its `data` ivar points at a `ccArray` struct: `num` (u32) at +0, `max` (u32) at +4 and `arr`, a pointer to `num` object pointers, at +8 (cocos2d 0.99.5's `ccCArray.h`; `CCArray::count` and `CCArray::objectAtIndex:` in `CCArray.c` read the same places). A `num` above `max` is taken as unreadable.
- **The director.** `+[CCDirector sharedDirector]` keeps the director in a file-static variable (`DAT_001512a0` in the decompile of 4Towers, `CCDirector.c` around line 404) that is not exported, and calling `sharedDirector` would run the game's code mid-frame. So the dump looks through the app binary's `__data`, `__bss` and `__common` sections, one 4-byte word at a time, for the first word that points at an instance (not a class) of `CCDirector` or a subclass. The director is one object, so any word that points at one points at it. The address of that word is kept and checked again at each dump; if it no longer points at a director, the sections are searched again. Before the director exists there is nothing to walk, and the file says so.
- **The texture cache** is found the same way (`+[CCTextureCache sharedTextureCache]` keeps it in `DAT_001512bc`, `CCTextureCache.c` line 172). Its `textures_` is an `NSMutableDictionary` made by touchHLE, so its keys and values are read from touchHLE's own record of the dictionary, once per dump, and each value (a `CCTexture2D`) is matched to the sprites' textures by address. The `# cache:` lines read each value's `name_`, `width_` and `height_` by ivar name, as the texture columns do.

The walk runs on the emulator thread while the frame is presented, so the game sees the time it takes; for a few hundred nodes that should be well under a millisecond (the first dump also searches the data sections), but it has not been measured.

**Not yet checked on a running game.** The walk is covered by unit tests on a made-up memory and class table (the `ccArray` reader, the column line, the escaping, the colour, the caps), and so are finding a singleton (a class object skipped, the kept slot read first, a search again when it goes stale) and matching the texture cache's keys to its textures and writing its `# cache:` lines. The dump was first run on 4Towers on two phones on 25 October 2026 (the game repo's `docs/TOUCHHLE.md`, Known Visual Quirks); the `# cache:` lines have not been run on the game yet.

## Instruments

Five more ways to read the running game, and one to play it, added in November 2026 on the `4towers-instruments` branch. Each one writes into the message trace's folder, on its clock and with its frame numbers, and each one turns the trace on by itself, so `trace.log` is written even with no selectors. All of them are off unless asked for. The rows they write in `trace.log` have new origins (`rng`, `addr`, `watch`, `input`); like `nslog` rows they start with the usual `frame`, `ms`, `thread` and `origin` columns, so a reader that sorts rows by origin first (as `parse_trace.py` does: any origin but `guest` and `host` is read as a text row) reads them without change.

### Random numbers

`--trace-rng` writes a row with origin `rng` for every random number the game draws and every seed it sets. touchHLE's C library has the generators (`src/libc/stdlib.rs`): `rand`, `rand_r`, `random` and `arc4random` draw, `srand`, `srandom` and `sranddev` seed. It implements no other entry point (`arc4random_uniform`, `drand48` and the rest are not there; 4Towers' decompile calls only `arc4random`, `rand` and `random`). cocos2d's `CCRANDOM_0_1()` is a macro over `random()`, so it shows up as `random`.

The row has the 17 columns of a call row:

| column | value |
| --- | --- |
| `frame`, `ms`, `thread` | as for any row |
| `origin` | `rng` |
| `variant` | `gen` (the generator's value), `replay` (a value from `--rng-replay=`, below) or `seed` |
| `class` | `-` |
| `selector` | the function: `arc4random`, `rand`, `rand_r`, `random`, `srand`, `srandom` or `sranddev` |
| `arg0` | the value returned (or the seed), in hex |
| `arg0_f32` | the same in decimal: unsigned for `arc4random` and the seeds, signed for the functions that return an `int` (always 0 or more) |
| `arg1` | the generator's state after the call, in hex; `-` for a seed |
| `arg1_f32`, `sp0` to `sp3` | `-` |
| `lr` | the caller's return address, which names the game's function that drew (look it up in the decompile) |
| `self` | `rand_r`'s seed pointer, `-` otherwise |

For example (from the unit test, tabs between the columns): `12  3456.789  0  rng  gen  -  arc4random  0xdeadbeef  3735928559  0xdeadbeef  -  -  -  -  -  0x0000a1b3  -`.

The generators are touchHLE's own (xorshift32), not Apple's, so the numbers are not the ones the phone drew; the row says when the game drew, how many times, from where, and what it got under touchHLE.

`--rng-replay=FILE` hands the values listed in `FILE` (in the user data directory, next to `trace.log`) to the draws instead, in order, one per draw whatever the function. A line holds:

- a value: decimal (a negative one is taken as its 32 bits) or hex with `0x`;
- a function name and a value (`arc4random 0x5d1f3c08`): the name is checked against the function that draws it, and a mismatch is logged (once) but the value is still used;
- or an `rng` row of a `trace.log`, as written above: its function and `arg0` are read, and its seed rows are skipped. So the `rng` rows of one run, cut out of its trace, replay that run's draws.

`#` starts a comment, and a line that is not understood is logged and skipped. The functions that return an `int` take the value masked with `RAND_MAX` (`0x7fffffff`). The generator still runs at each draw, so its state (in the `arg1` column) is the one it would have had, and when the values run out (logged once) the draws carry on with the generator's own values from there. Replay does not need the trace, but with `--trace-rng` each replayed draw is a row with `replay` in the variant column. The file is read once, at startup.

What it costs: nothing when off (one `Option` check per draw). With `--trace-rng` each draw is one unbuffered write to `trace.log`, like a traced call.

The row format and the replay file's grammar are covered by unit tests (`rng_rows_have_the_call_row_columns` in `src/objc/trace.rs`, and `src/libc/stdlib/rng.rs`).

### Address hooks

A hook writes a row with origin `addr` every time execution reaches a chosen guest address: a decompiled function's entry (`SharedData::asbs:b: @ 00006c1c`), a line in the middle of one, or a C function the selector trace cannot see. The game does not pause: the row is written and execution carries on at once.

Hooks are listed in `trace_addresses.txt` next to the other touchHLE files, one per line (`#` starts a comment), or in `--trace-addresses=` (items separated by commas, the words of an item by `;`, as for `--trace-selectors=`). The two add up. A line:

```
<address> [arm|thumb] [<name>] [<peek>...]
```

- `<address>` is hex, with or without `0x`; a leading `@` (the decompile's spelling) is allowed. An odd address means Thumb code at the address one below, as a function pointer with the Thumb bit does (the class dumps' `IMP: 0x54ed` can be pasted as it is).
- `arm` or `thumb` says which instruction set the hook is for; without it (and with an even address) it is for both. An instruction is Arm or Thumb according to how it is reached, and the same bytes are almost never run both ways, so the word only matters for odd cases.
- `<name>` is any word that is not a peek; it goes in the row's `selector` column. Without one the address is the name.
- Peeks are written as for the selector trace (see Peeks above), but start at a register, `r0` to `r3` or `sp`, as it is when the address is reached, or at a fixed address in hex: `r0.hp:f32` (an ivar of the object in r0), `sp.+8:u32` (the third word on the stack), `0x1512a0.+0:i32` (the word at that address), `0x1512a0->waveNum:i32` (an ivar of the object a static variable points at). At a function's entry r0 to r3 are its first four arguments, and for an Objective-C method r0 is `self` and r1 the selector.

For example:

```
00006c1c thumb asbs r2:f32 r3:f32       # SharedData::asbs:b:, the two points' x
0x0005bb24 spawnCreep r0:id r0.type:i32
```

A second line for the same address adds its instruction sets and peeks to the first. The header of `trace.log` has a line per hook: `# addr <name> <address> <arm|thumb|any> r2 r3 sp <peeks...>`.

The row has the 17 columns of a call row and then three more, then the peeks:

| column | value |
| --- | --- |
| `frame`, `ms`, `thread` | as for any row |
| `origin` | `addr` |
| `variant` | `arm` or `thumb`: how the instruction ran |
| `class` | `-` |
| `selector` | the hook's name |
| `arg0`, `arg0_f32` | r0, in hex and as a float |
| `arg1`, `arg1_f32` | r1, the same |
| `sp0` to `sp3` | the four words at sp |
| `lr` | the link register: at a function's entry, its caller's return address |
| `self` | the hooked address |
| then | r2, r3 and sp in hex, then a column per peek, in the order of the `# addr` line |

Everything is read before the instruction at the address runs. For example (from the unit test): `5  12.500  0  addr  arm  -  spawn  0x3f800000  1  0x00000002  ...  0x00004001  0x00002d24  0x00000003  0x00000004  0x00fff000  25`.

How it works: the CPU (dynarmic, a JIT) offers a hook when it reads an instruction to translate it. A hooked address always starts a block of translated code (the block it would have been in ends just before it), and the block's first operation is a write to a probe address in the null page, which has no page table entry, so the write goes through touchHLE's memory callback. The callback halts the CPU, and dynarmic's check after a callback stores the exact PC and returns, so touchHLE stops before the hooked instruction with every register as it was. The row is written, then that one instruction is run as a single step with the probe let through once, and the run carries on. Adding a hook clears the translated code at its address, so a hook takes effect at once. Details:

- A conditional instruction gates its whole block, so a hook on one fires only when its condition passes (when the instruction really runs).
- A hook on an address that is not the start of an instruction (the middle of a 32-bit Thumb instruction) never fires.
- Hooks need the probe address to have no page table entry. That holds for every app binary, whose first page is its `__PAGEZERO`, and with `--disable-direct-memory-access` every access goes through the callbacks anyway. Otherwise the hooks are not set and the log says so.
- What it costs: nothing for code that is not hooked. A hit leaves the JIT and comes back, about the cost of a host function call, plus the row's write.

### Coverage

`--coverage` records every instruction address the JIT translates and writes the app binary's ones to `coverage.txt`, next to `trace.log`: at exit (touchHLE's exit path, as for the frames), when F8 is pressed (or sent by the Android wrapper, like F11), and every 1800 frames presented (about 30 to 60 seconds), so a run that crashes or is killed still leaves a recent file. Each write replaces the file and has everything since the start.

The file is tab-separated text: lines starting with `# ` are the header (the build, the app, when and why it was written, `instructions=`, `ranges=` and `outside_the_app=`, the addresses outside the app binary, which are touchHLE's own stubs and dylibs), then a line of column names, `start end set section`, then one line per range: the first address, the address after the last instruction (hex), `arm` or `thumb`, and the binary's section (`__text` for nearly all). Instructions of one instruction set that follow each other without a gap make one range, so a function that ran from start to end is one line, and a literal pool or a branch not taken splits it. For example (from the unit test): `0x00002d24  0x00002d6a  thumb  __text`.

It is the JIT's view: an instruction counts once it is translated, which happens the first time execution reaches the block it is in. So it says which code ever ran, with one caveat: in a block that ran, a conditional instruction whose condition failed counts too (dynarmic translates a straight run of instructions up to a branch, and a condition only skips an instruction). Code translated again after its cache was cleared counts once. To map ranges to functions, look up each start in the decompile; a function whose entry is not in any range never ran.

What it costs: one append to a vector per translated instruction, at translation only, so it does not slow code that already ran; a write of the file sorts and merges what was gathered (a few hundred thousand addresses at most for this game).

### Watchpoints

A watchpoint writes a row with origin `watch` for every write the game makes to a chosen field, with the value before and after and the address of the instruction that wrote it, so "who changed the gold, from where" is one row naming the function. They are listed in `trace_watch.txt` (one per line, `#` starts a comment) or `--trace-watch=` (commas between items, `;` between the words of an item). A line:

```
<target>:<type> [<name>]
```

- `<target>` is an address in hex (with or without `0x`), or `Class.ivar`, optionally followed by `.+N` (bytes further on, decimal or `0x` hex, for a member of a struct ivar). A `Class.ivar` watch starts on the first object of that class (or of a subclass, matched by name on the class chain as for Receiver class above) allocated once the trace is on, with an `arm` row; when that object is freed (`-[NSObject dealloc]`), the watch stops with a `disarm` row and waits for the next one.
- `<type>` is how the value is read and written and how many bytes are watched: `u8`, `i8`, `u16`, `i16`, `u32`, `i32`, `f32`, `ptr`, `u64`, `i64` or `f64`.
- `<name>` goes in the row's `selector` column; without one the target as written is the name.

For example: `SharedData.totalGold:f32 gold`, `SharedData.waveNum:i32`, `0x0012a4c8:f32`. The header of `trace.log` has a line per watch: `# watch <name> <target> <type>`.

The row has the 17 columns of a call row:

| column | `write` row | `arm` row | `disarm` row |
| --- | --- | --- | --- |
| `variant` | `write` | `arm` | `disarm` |
| `class` | the type | the type | the type |
| `selector` | the name | the name | the name |
| `arg0` | the watched address | the same | the same |
| `arg0_f32` | the value before, as its type | the value now | `-` |
| `arg1` | the object (`-` for a fixed address) | the object | the object |
| `arg1_f32` | the value after | `-` | `-` |
| `sp0`, `sp1` | the write's address and size in bytes | `-` | `-` |
| `sp2`, `sp3` | the watched bytes before and after, in hex | the bytes now, `-` | `-` |
| `lr` | the link register when the write ran | `-` | `-` |
| `self` | the address of the writing instruction | `-` | `-` |

A write is logged whenever it touches a watched byte, also when it writes the same value, and also when it covers more than the field (an `strd`, an `stm`) or only part of it; `sp0` and `sp1` say what it wrote. For example (from the unit test): `7  100.250  0  watch  write  f32  gold  0x0012a4c8  30  -  0  0x0012a4c8  4  0x41f00000  0x00000000  0x0003a001  0x0003a4e6`: the gold went from 30 to 0, written by the instruction at `0x3a4e6`.

How it works: guest memory is reached through dynarmic's page table, one entry per 4 KiB page. The page holding a watched field is taken out of the table, so every access to it, by every instruction, goes through touchHLE's read and write callbacks; the other pages keep their direct access. A write that touches a watched range is made, its old and new bytes are recorded, and the CPU halts the way a hook does: dynarmic's check after the callback stores the exact PC of the writing instruction. The row is written, the rest of that instruction is run as a single step (it makes its writes again; the one already logged is skipped, any other watched write it makes is logged with the same PC), and execution carries on. When the last watch on a page stops, the page gets its entry back. So that a run can resume from the middle of a block, a run with watchpoints turns off one optimization of dynarmic's arm64 back end (register get/set elimination); on x64 dynarmic already skips it.

Limits:

- Only the guest's own writes are seen. Writes made by touchHLE's host code into guest memory (its `memcpy`, `memset`, `objc_setProperty`, `setValue:forKey:`, Foundation, the zeroing of freed memory) do not go through the CPU and are not logged; a field the game copies with `memcpy` changes without a row.
- A store-exclusive (`strex`) to a watched field is logged without halting (running it again would fail and make the guest retry on the new value), so its row has `-` in `lr` and `self`, and it is written at the next presented frame.
- `Class.ivar` watches only the first instance at a time, and only objects allocated through `+[NSObject allocWithZone:]` (every class of the game's).
- What it costs: every access to a watched page, read or write, goes through a callback (a function call instead of one instruction), and each write to the watched bytes leaves the JIT twice. A field that is written every frame is fine; a page that holds a hot array is slower. Other pages are not affected, and with no watchpoint nothing changes.

The parsing of the three files' lines, the rows, the header lines, the range merging and the watch bookkeeping (which writes touch which ranges, the halt and the step after it, the store-exclusive case) are covered by unit tests (`src/objc/trace/addr.rs`, `coverage.rs`, `watch.rs`, `src/cpu/watch.rs`, and the hook peeks in `src/objc/trace/peek.rs`).

### State stream

`--state-every=N` writes, every Nth presented frame (counting from frame 0), a snapshot of every live object of the chosen classes, with every instance variable by name, to `state.jsonl` next to `trace.log`. It is made for comparing the game's state with the remake's: positions, hit points, timers, flags, as the game holds them, on the trace's frame clock. 30 is once a second at the game's 30 frames a second.

The classes are `--state-classes=` (names separated by commas), or by default the game's: `Game`, `towersLayer`, `towerGrid` (the towers on the map), `Tower`, `Creeps`, `Bullets` and `SharedData` (the names in the class dump, `decomp/headers/`). A class is matched on the class chain, as for Receiver class above, so a subclass of a chosen class is written too (`--state-classes=CCNode` writes every node).

Objects are found without calling the game, from three places:

- the cocos2d scene: every node from the `CCDirector`'s `runningScene_` down, as for the scene dump;
- the app binary's data sections (`__data`, `__bss`, `__common`): every word that points at an instance of a chosen class, such as the static variable that holds `SharedData`;
- the objects found so far: each object-typed instance variable of a chosen object (also inside a struct or a C array, like `towersLayer`'s `towers` grid) that points at another chosen object, or at an array (touchHLE's `NSArray` and `NSMutableArray`, cocos2d's `CCArray`) or a dictionary (touchHLE's) whose items are. So the creeps and bullets in `SharedData`'s `mcreepsArray`, `bulletsArray` and the rest are found even when they are not in the scene.

The instance variables come from each class's ivar list in the app binary (cocos2d is linked into it, so `CCNode`'s are there too): name, type encoding, size, and the offset touchHLE settled on. An object line has those of its class and every superclass, superclasses first, which is their order in memory.

The file is JSON Lines: one JSON object per line, each with a `kind`:

- `header`, the first line: `schema` (`touchHLE-state-1`; it changes if the format does), `touchhle` (the build), `app`, `every`, `classes`, and the caps `max_objects` and `max_elements`.
- `snapshot`, one per snapshot, before its objects: `frame` and `ms` (the trace's clock: frames presented so far, milliseconds since touchHLE started, as in `trace.log`), `objects` (how many were written), `by_class` (how many of each class), `found` (how many were found in the scene, in a static variable and through another object's instance variable) and `notes` (what could not be read, and a note if a cap cut the snapshot).
- `class`, the first time a class is written in the file, before its first object: `class`, `chain` (the class and its superclasses), `size` (of an instance), `ivars` (one entry per instance variable: `key`, the key in an object line, which is the name, or `Class.name` if a superclass has one of that name too; `class`, the class that declares it; `name`; `type`, the type encoding as the binary has it; `offset`; `size`; `decoded`, false if the value is written as hex bytes) and `notes`.
- `object`, one per object: `frame`, `class`, `addr` (the object's address, as a string in hex), `via` (where it was found: `scene`, `static 0x00151230`, or the holding object's class and the path to it, as `SharedData.mcreepsArray[3]` or `towersLayer.towers[2][5]`; `{}` for a dictionary's value), `ivars` (by key), and `cut: true` if a C array or hex value was cut at `max_elements`. If the class's ivars could not be read, `ivars` is `null` and `error` says why.

Values follow the type encoding:

| type | value |
| --- | --- |
| integers, `BOOL` (`c C s S i I l L q Q B`) | a number (a `BOOL` is a signed `char`, so `1`, or `-1` for a 255) |
| `float`, `double` | a number, or `"nan"`, `"inf"`, `"-inf"` |
| object (`@`), class, selector, any other pointer | its address as a string, `"0x0015bb50"` (an object of a chosen class has its own line, with that `addr`) |
| struct | a JSON object by field name, as `{"x": 160, "y": 240}` (by index, `"0"`, `"1"`, when the encoding has no names) |
| C array | a JSON array, at most `max_elements` (1024) elements |
| bitfield (`bN`) | a number: the binary gives only the byte offset, shared by the bitfields declared next to each other, so the bits are counted from the lowest, the first of a run at one offset from bit 0 and each next one where the one before ended (`CCNode`'s `isTransformDirty_`, `isInverseDirty_` and `isTransformGLDirty_` are bits 0, 1 and 2 of one byte) |
| a union, or a type not decoded | its bytes in hex, in memory order, as `"01020304"` |

For example (from a run on the title screen with `--state-classes=SharedData,MainMenu,CCMenu,CCDirector`): `{"kind":"snapshot","frame":120,"ms":6057,"objects":6,"by_class":{"CCDirectorTimer":1,"CCMenu":3,"MainMenu":1,"SharedData":1},"found":{"scene":4,"static":2,"ivar":0},"notes":[]}`, and an object line starts `{"kind":"object","frame":120,"class":"MainMenu","addr":"0x0015ac40","via":"scene","ivars":{"rotation_":0,"scaleX_":1,"scaleY_":1,"position_":{"x":0,"y":0},...`.

Caps: at most 5000 objects per snapshot (`max_objects`; the snapshot's notes say if it was reached), 1024 elements of a C array or bytes of a hex value per instance variable, and the scene walk's caps (5000 nodes, 32 levels).

Limits: an object is only found if it is in the scene, in a static variable, or reachable from another chosen object through an object pointer, an array or a dictionary (not a set, a C array of pointers typed `void *`, or a container inside a container). A value is read as it is in memory when the frame is presented, which is between the game's updates.

What it costs: nothing when off. A snapshot walks the scene, scans the data sections (a few KiB for this game), and reads each object's instance variables; the class layouts are read once. Each snapshot is one write to `state.jsonl`. With the default classes on the title screen it is a few objects; in a level, a few hundred, each line a few KiB.

The type decoding, the layout of the values, the search through the scene, the statics, arrays and instance variables, and the lines are covered by unit tests (`src/objc/trace/state.rs`).

### Live stream

`--live-port=N` (`src/objc/trace/live.rs`) sends the rows of `trace.log`, `input.log` and `state.jsonl` to one reader on `127.0.0.1:N` as they are written, for the 4Towers side-by-side app, where the remake follows the original frame by frame (the Technoid4Towers repo's `docs/plans/SIDE_BY_SIDE.md`, LIVE2).

- Lines: `H\t{json}` once per connection (schema `4towers-live-1`, the build, the frame now, `trace_rng`, `state_every`), `T\t<row>`, `I\t<row>`, `S\t<line>` (only for a reader whose first line is `hello state`), and `F\t<frame>\t<ms>` at the end of each presented frame, after its state snapshot. Rows keep their bytes. A row of the next frame (the input script's) can come before a frame's `F` line, so a reader files each row under its own frame number.
- `trace.log` goes through `TraceOut`, which writes the file as before and copies each row to the stream; the file's bytes do not depend on a reader (test `a_trace_written_with_a_reader_has_the_same_bytes_as_one_without`).
- The game thread makes no socket call. A listener thread accepts and reads the hello line; a frame's rows go to a writer thread through a queue of 900 frames with `try_send`. A reader whose queue is full is dropped and its socket shut down at once (no gap in a stream); a reader that goes away is dropped at the next frame; a new reader replaces the old one. On a normal close the queue is sent before the socket closes.
- Checked by hand on 10 November 2026: 900 frames of the title screen with `--live-port=47100 --trace-rng --state-every=30` and a reader sending `hello state`: one `H` line, 899 `F` lines in order, and the `T` and `S` lines equal to the files' rows from the reader's first frame on.

### Scripted input

`--input-script=FILE` plays touches listed in `FILE` (in the user data directory, next to `trace.log`, as for `--rng-replay=`) at chosen frames, so a run can play the game unattended: on a laptop whose desktop session is not attached, synthetic mouse clicks never reach the window, so the touches are made inside touchHLE. Without the option, `input_script.txt` next to the other touchHLE files is used if it is there. `--quit-at-frame=F` exits at presented frame F, with or without a script. The file is read once, at startup. Either one turns the trace on by itself.

One command per line, words separated by spaces; `#` starts a comment, and a line that is not understood is logged with its number and skipped:

```
[@F | +N] tap X Y
[@F | +N] down X Y
[@F | +N] move X Y
[@F | +N] up X Y
[@F | +N] wait <target>:<type> <op> <value> [timeout T]
[@F | +N] quit
```

- `@F` runs the command at presented frame F, the frame numbers of `trace.log` and `input.log`. `+N` runs it N frames after the command before it ran; with neither, it is `+0`. Commands run one after another, in the order written: an `@F` whose frame has gone by (because a `wait` before it held the script) runs at once.
- `tap X Y` is a `down` at (X, Y) and an `up` at the same point 3 frames later. `down`, `move` and `up` are one touch each, for a drag. X and Y are guest points, unrotated, exactly as `input.log` writes them (the screen is 320 x 480 points).
- `wait` holds the script until the value is true, read at each presented frame. `<target>:<type>` is written as for a watchpoint (see Watchpoints above): `Class.ivar` (with an optional `.+N`) or an address in hex, and a type, `u8` to `f64`. A `Class.ivar` is read on the instance a static variable of the app binary points at (found as the scene dump finds the director, which takes `SharedData`), or else on the first node of that class in the running cocos2d scene. `<op>` is one of `>=`, `<=`, `==`, `>`, `<` and `!=`, and `<value>` a number. When it passes, the next `+N` counts from that frame. With `timeout T`, after T frames of waiting the script carries on as if it had passed, and the log says so. A target that cannot be read yet (no instance, no such ivar) counts as not true, logged once per wait.
- `quit` exits touchHLE through its usual exit path, the one closing the window takes (see [Home and back](#home-and-back)), so the frames still queued and `coverage.txt` are written as at any exit.

A row of `input.log` is also a line: `frame ms phase finger x y` is read as `@frame phase x y` (the finger and the time are ignored), so the rows of one run, header and all, play its touches again: `cp input.log input_script.txt`, or `awk -F'\t' '!/^#/ {print "@" $1, $3, $5, $6}' input.log` for a script to edit.

Every touch the script makes is handed to UIKit as a host touch is, with the finger `Script`: it goes into the window's event queue when frame F - 1 is presented, and the run loop hands it to the game before frame F is presented, so the game gets it, and `input.log` has it, with frame F. There is one script finger, so a `down` while it is still down reaches the game as a move (see the `phase` column of the touch log): leave at least 3 frames between taps, or write the `down` and `up` yourself.

Each command also writes a `trace.log` row with origin `input`, laid out like an `nslog` row: the usual `frame`, `ms` and `thread`, `-` in `variant` and `class`, the command as written (words joined by one space) in the `selector` column, and `-` in the ten columns after it. A tap's up has a row of its own, the command and `: up`; a `wait` has one when it starts (`: waiting`) and one when it passes (`: passed after N frames (value)`) or times out (`: timed out after N frames (last value)`); `--quit-at-frame=` writes `quit (--quit-at-frame=F)`. The header has a line `# input script: <path> (<n> commands); quit at frame F`.

An example, from title screen to Level 1 with a tower built, checked on 1 November 2026 (frames at 30 a second; the title is up by frame 700, and the game's save in that sandbox has every level open):

```
@750 tap 160 214      # PLAY on the title
@830 tap 160 214      # NORMAL
@1250 tap 84 60       # the Level 1 hex on the map
@1400 tap 234 412     # PLAY on the level card
+60 wait SharedData.totalGold:f32 >= 60 timeout 600
+10 tap 58 55         # a tower socket
+15 tap 36 93         # the red tower, 20 c
@4500 quit
```

Its `trace.log` rows read `1460 ... input ... +60 wait SharedData.totalGold:f32 >= 60 timeout 600: passed after 0 frames (90)`, the gold watch shows the tower bought (90 down to 70 in the frames after the tap at 1485) and the kills after it, and `state.jsonl` has the wave's `Creeps` from frame 1710. The game remembers a level it was closed in (see the UIKit list above), so a script that quits inside a level leaves the next run starting there; put the sandbox's `com.lambdamugames.4towers.plist` back between runs.

What it costs: nothing when off (one `Option` check per presented frame). With a script, each presented frame checks the next command's frame; a waiting `wait` reads its target once a frame (a word read once the static variable is known; a scene walk for a class found only in the scene). The parser and the scheduler (the timing of `@` and `+`, the tap's up, waits that pass, time out or cannot read, `quit` and `--quit-at-frame=`, the `input.log` row form) are covered by unit tests (`src/objc/trace/script.rs`).

## Messages to freed objects

*4Towers* sends messages to objects it has already freed (seen in run `20261101_204120_win_L18` on the S22U: a `Creeps dmg:` row took creep `0x00207030` to -1661.25 HP at frame 46543, and a `Bullets moveB` messaged it 150 frames later, at frame 46698). On an iPhone the freed memory keeps its isa, so the send still finds the method. Here the isa reads 0, and `objc_msgSend` stopped on `assertion failed: orig_class != nil` (`src/objc/messages.rs`).

- A message to a non-nil receiver whose isa is 0 now acts as a message to nil: R0 and R1 are cleared and the call returns, as for a nil receiver. The first send of each selector is logged with `log!` (receiver address and selector); later ones are silent.
- A super call (`msg_send_super2`) with a nil class still stops on the assert, since that would be a real bug.
- With a trace on, each such send also writes a `trace.log` row with origin `zombie`, laid out like an `nslog` row, with `zombie: <selector> to <receiver>` in the selector column. The header names the origin.
- A freed object whose memory has been reused has a non-zero isa and is not caught here.
- The row layout is covered by a unit test (`zombie_rows_fill_every_column` in `src/objc/trace.rs`). The send itself needs a running guest and has no unit test.
