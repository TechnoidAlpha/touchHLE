/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `GKLeaderboardViewController`.

use crate::objc::{id, msg, nil, objc_classes, ClassExports, SEL};

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation GKLeaderboardViewController: UIViewController

- (id)init {
    this
}

- (())setLeaderboardDelegate:(id)delegate {
    log_dbg!("[(GKLeaderboardViewController*){:?} setLeaderboardDelegate:{:?}]", this, delegate);
    let sel: SEL = env.objc.register_host_selector("leaderboardViewControllerDidFinish:".to_string(), &mut env.mem);
    let responds: bool = msg![env; delegate respondsToSelector:sel];
    if responds {
        log_dbg!("Auto-dismissing GKLeaderboardViewController via leaderboardViewControllerDidFinish:");
        () = msg![env; delegate leaderboardViewControllerDidFinish:this];
    }
}

- (())setCategory:(id)_category {
}

- (())setTimeScope:(i32)_scope {
}

@end

};
