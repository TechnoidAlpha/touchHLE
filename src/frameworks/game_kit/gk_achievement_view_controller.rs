/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `GKAchievementViewController`.

use crate::objc::{id, msg, objc_classes, ClassExports, SEL};

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation GKAchievementViewController: UIViewController

- (id)init {
    this
}

- (())setAchievementDelegate:(id)delegate {
    log_dbg!("[(GKAchievementViewController*){:?} setAchievementDelegate:{:?}]", this, delegate);
    let sel: SEL = env.objc.register_host_selector("achievementViewControllerDidFinish:".to_string(), &mut env.mem);
    let responds: bool = msg![env; delegate respondsToSelector:sel];
    if responds {
        log_dbg!("Auto-dismissing GKAchievementViewController via achievementViewControllerDidFinish:");
        () = msg![env; delegate achievementViewControllerDidFinish:this];
    }
}

@end

};
