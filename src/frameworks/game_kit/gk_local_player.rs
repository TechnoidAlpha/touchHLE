/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `GKLocalPlayer`.

use crate::dyld::{ConstantExports, HostConstant};
use crate::objc::{id, msg, objc_classes, ClassExports};

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// TODO: proper inheritance chain
@implementation GKLocalPlayer: NSObject

+ (id)localPlayer {
    let instance: id = msg![env; this new];
    instance
}

+ (())authenticateWithCompletionHandler:(id)_handler {
}

- (())authenticateWithCompletionHandler:(id)_handler {
}

@end

};

pub const GKPlayerAuthenticationDidChangeNotificationName: &str =
    "GKPlayerAuthenticationDidChangeNotificationName";

/// `NSNotificationName` values.
pub const CONSTANTS: ConstantExports = &[(
    "_GKPlayerAuthenticationDidChangeNotificationName",
    HostConstant::NSString(GKPlayerAuthenticationDidChangeNotificationName),
)];
