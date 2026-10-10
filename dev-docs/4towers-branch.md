# The `4towers` branch

A personal fork branch. The changes here were written with AI assistance and are not for upstream (see the note at the top of the README). They sit on top of upstream commit `d34530b5`.

Each item is something *4Towers: Onslaught* 1.01 (iOS 4.1, cocos2d) called that stock touchHLE did not have, or stopped on.

## Foundation

- `-[NSMutableArray removeObjectIdenticalTo:]` and `-[NSMutableArray removeObjectsInArray:]`.
- `-[NSMutableArray removeObject:]` removes from the back, so the indexes it collected stay valid.
- `-[NSNumber encodeWithCoder:]` for the long long, short, char, unsigned and float kinds.
- `NSKeyedArchiver`: `encodeBool:forKey:`, `encodeInt32:forKey:`, `encodeInt64:forKey:`, `encodeFloat:forKey:`, `encodeDouble:forKey:` and `encodeCGPoint:forKey:`. Encoding the same key twice now overwrites instead of stopping the emulator.
- `NSSet` and `NSMutableSet`: `copyWithZone:` and `mutableCopyWithZone:`.
- `NSObject`: `methodForSelector:`, `instanceMethodForSelector:` and `methodSignatureForSelector:`. `instanceMethodSignatureForSelector:` returns nil for a selector it does not know.

## AudioToolbox and CoreGraphics

- `ExtAudioFileGetProperty` answers `kExtAudioFileProperty_FileLengthFrames`.
- `CGBitmapContextCreate` works out the row size when it is passed 0 with a caller's buffer.

## UIKit

- `-[UIApplication statusBarStyle]` and `-[UIApplication statusBarFrame]`.
- `-[UIViewController init]` and a logging stub for `presentModalViewController:animated:`.

## GameKit

These are stubs. Nothing talks to Game Center.

- `GKLeaderboardViewController` and `GKAchievementViewController`. Setting the delegate calls its "did finish" method straight away, so the game carries on as if the screen had been closed.
- `GKAchievement` and `GKScore` accept a value and a report call and do nothing.
- `+[GKLocalPlayer localPlayer]` returns an object, and `authenticateWithCompletionHandler:` does nothing.

## Builds

The build workflow on this branch clones this fork instead of upstream and also runs on pushes here, so GitHub Actions builds the branch for Windows, macOS and Android. That is the only change to it.
