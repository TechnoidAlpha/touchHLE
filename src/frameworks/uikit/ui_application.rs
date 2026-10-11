/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIApplication` and `UIApplicationMain`.

use super::ui_device::*;
use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::frameworks::core_graphics::{CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string::{from_rust_string, get_static_str};
use crate::frameworks::foundation::{ns_array, ns_string, NSInteger, NSUInteger};
use crate::mem::MutPtr;
use crate::objc::{
    autorelease, id, msg, msg_class, msg_send, nil, objc_classes, release, retain,
    todo_objc_setter, ClassExports, HostObject, NSZonePtr,
};
use crate::window::DeviceOrientation;
use crate::Environment;

#[derive(Default)]
pub struct State {
    /// [UIApplication sharedApplication]
    shared_application: Option<id>,
    pub(super) status_bar_hidden: bool,
    /// `applicationWillResignActive:` has been sent, and
    /// `applicationDidBecomeActive:` not since.
    resigned: bool,
    /// `applicationDidEnterBackground:` has been sent, and
    /// `applicationWillEnterForeground:` not since.
    in_background: bool,
}
impl State {
    fn get(env: &mut Environment) -> &mut Self {
        &mut env.framework_state.uikit.ui_application
    }
}

struct UIApplicationHostObject {
    delegate: id,
    delegate_is_retained: bool,
}
impl HostObject for UIApplicationHostObject {}

pub type UIInterfaceOrientation = UIDeviceOrientation;
#[allow(unused)]
pub const UIInterfaceOrientationPortrait: UIInterfaceOrientation = UIDeviceOrientationPortrait;
#[allow(unused)]
pub const UIInterfaceOrientationPortraitUpsideDown: UIInterfaceOrientation =
    UIDeviceOrientationPortraitUpsideDown;
// These are intentionally swapped and documented as such (the UI on the device
// rotates in the opposite direction to how the device is rotated).
pub const UIInterfaceOrientationLandscapeLeft: UIInterfaceOrientation =
    UIDeviceOrientationLandscapeRight;
pub const UIInterfaceOrientationLandscapeRight: UIInterfaceOrientation =
    UIDeviceOrientationLandscapeLeft;

type UIRemoteNotificationType = NSUInteger;
type UIStatusBarAnimation = NSInteger;
type UIStatusBarStyle = NSInteger;

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIApplication: UIResponder

// This should only be called by UIApplicationMain
+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(UIApplicationHostObject {
        delegate: nil,
        delegate_is_retained: false,
    });
    env.objc.alloc_static_object(this, host_object, &mut env.mem)
}

+ (id)sharedApplication {
    env.framework_state.uikit.ui_application.shared_application.unwrap_or(nil)
}

// This should only be called by UIApplicationMain
- (id)init {
    assert!(env.framework_state.uikit.ui_application.shared_application.is_none());
    env.framework_state.uikit.ui_application.shared_application = Some(this);
    this
}

// This is a singleton, it shouldn't be deallocated.
- (id)retain { this }
- (id)autorelease { this }
- (())release {}

- (id)delegate {
    env.objc.borrow::<UIApplicationHostObject>(this).delegate
}
- (())setDelegate:(id)delegate { // something implementing UIApplicationDelegate
    let host_object = env.objc.borrow_mut::<UIApplicationHostObject>(this);
    // This property is quasi-non-retaining: https://stackoverflow.com/a/14271150/736162
    let old_delegate = std::mem::replace(&mut host_object.delegate, delegate);
    if host_object.delegate_is_retained {
        host_object.delegate_is_retained = false;
        if delegate != old_delegate {
            release(env, old_delegate);
        }
    }
}

- (bool)isStatusBarHidden {
    env.framework_state.uikit.ui_application.status_bar_hidden
}
- (())setStatusBarHidden:(bool)hidden {
    env.framework_state.uikit.ui_application.status_bar_hidden = hidden;
}
- (())setStatusBarHidden:(bool)hidden
                animated:(bool)_animated {
    // TODO: animation
    msg![env; this setStatusBarHidden:hidden]
}
- (())setStatusBarHidden:(bool)hidden
           withAnimation:(UIStatusBarAnimation)_animation {
    // TODO: animation
    msg![env; this setStatusBarHidden:hidden]
}

- (())setStatusBarStyle:(UIStatusBarStyle)style {
    todo_objc_setter!(this, style);
}
- (())setStatusBarStyle:(UIStatusBarStyle)style
               animated:(bool)_animated {
    // TODO: animation
    msg![env; this setStatusBarStyle:style]
}

- (UIStatusBarStyle)statusBarStyle {
    0
}

- (CGRect)statusBarFrame {
    if env.framework_state.uikit.ui_application.status_bar_hidden {
        CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize { width: 0.0, height: 0.0 },
        }
    } else {
        let (width, height) = env.window().device_family().portrait_size();
        let orientation: UIInterfaceOrientation = msg![env; this statusBarOrientation];
        const STATUS_BAR_HEIGHT: f32 = 20.0;
        match orientation {
            UIDeviceOrientationPortrait | UIDeviceOrientationPortraitUpsideDown => CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize { width: width as f32, height: STATUS_BAR_HEIGHT },
            },
            UIDeviceOrientationLandscapeLeft | UIDeviceOrientationLandscapeRight => CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize { width: STATUS_BAR_HEIGHT, height: height as f32 },
            },
            _ => CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize { width: width as f32, height: STATUS_BAR_HEIGHT },
            },
        }
    }
}

- (UIInterfaceOrientation)statusBarOrientation {
    match env.window().current_rotation() {
        DeviceOrientation::Portrait => UIDeviceOrientationPortrait,
        DeviceOrientation::PortraitUpsideDown => UIDeviceOrientationPortraitUpsideDown,
        DeviceOrientation::LandscapeLeft => UIDeviceOrientationLandscapeLeft,
        DeviceOrientation::LandscapeRight => UIDeviceOrientationLandscapeRight
    }
}
- (())setStatusBarOrientation:(UIInterfaceOrientation)orientation {
    let prev_orientation = env.window().current_rotation();
    env.on_parent_stack_in_coroutine(|window, _| {window.rotate_device(match orientation {
        UIDeviceOrientationPortrait => DeviceOrientation::Portrait,
        UIDeviceOrientationPortraitUpsideDown => DeviceOrientation::PortraitUpsideDown,
        UIDeviceOrientationLandscapeLeft => DeviceOrientation::LandscapeLeft,
        UIDeviceOrientationLandscapeRight => DeviceOrientation::LandscapeRight,
        _ => unimplemented!("Orientation {} not handled yet", orientation),
    })});
    if prev_orientation != env.window().current_rotation() {
        generate_device_orientation_notification(env);
    }
}
- (())setStatusBarOrientation:(UIInterfaceOrientation)orientation
                     animated:(bool)_animated {
    // TODO: animation
    msg![env; this setStatusBarOrientation:orientation]
}

- (bool)isIdleTimerDisabled {
    !env.window().is_screen_saver_enabled()
}
- (())setIdleTimerDisabled:(bool)disabled {
    env.on_parent_stack_in_coroutine(|window, _| window.set_screen_saver_enabled(!disabled))
}

- (())setNetworkActivityIndicatorVisible:(bool)visible {
    todo_objc_setter!(this, visible);
}

- (bool)openURL:(id)url { // NSURL
    let ns_string = msg![env; url absoluteString];
    let url_string = ns_string::to_rust_string(env, ns_string);
    if let Err(e) = crate::window::open_url(env, &url_string) {
        echo!("App opened URL {:?} unsuccessfully ({}), exiting.", url_string, e);
    } else {
        echo!("App opened URL {:?}, exiting.", url_string);
    }

    // iPhone OS doesn't really do multitasking, so the app expects to close
    // when a URL is opened, e.g. Super Monkey Ball keeps opening the URL every
    // frame! Super Monkey Ball also doesn't check whether opening failed, so
    // it's probably best to always exit.
    exit(env);
    true
}

// TODO: ignore touches
-(())beginIgnoringInteractionEvents {
    log!("TODO: ignoring beginIgnoringInteractionEvents");
}
- (bool)isIgnoringInteractionEvents {
    false
}
-(())endIgnoringInteractionEvents {
    log!("TODO: ignoring endIgnoringInteractionEvents");
}

- (id)keyWindow {
    let Some(key_window) = env
        .framework_state
        .uikit
        .ui_view
        .ui_window
        .key_window else {
        return nil;
    };
    assert!(env
        .framework_state
        .uikit
        .ui_view
        .ui_window
        .windows
        .contains(&key_window));
    key_window
}

- (id)windows {
    let windows: Vec<id> = (*env
        .framework_state
        .uikit
        .ui_view
        .ui_window
        .windows).to_vec();
    for window in &windows {
        retain(env, *window);
    }
    let windows = ns_array::from_vec(env, windows);
    autorelease(env, windows)
}

- (())registerForRemoteNotificationTypes:(UIRemoteNotificationType)types {
    log!("TODO: ignoring registerForRemoteNotificationTypes:{}", types);
}

- (NSInteger)applicationIconBadgeNumber {
    0 // default value
}
- (())setApplicationIconBadgeNumber:(NSInteger)bn {
    log!("TODO: ignoring setApplicationIconBadgeNumber:{}", bn);
}

- (bool)applicationSupportsShakeToEdit {
    true // default value
}
- (())setApplicationSupportsShakeToEdit:(bool)enable {
    log!("TODO: ignoring setApplicationSupportsShakeToEdit:{}", enable);
}

// UIResponder implementation
// From the Apple UIView docs regarding [UIResponder nextResponder]:
// "The shared UIApplication object normally returns nil, but it returns its
//  app delegate if that object is a subclass of UIResponder and hasn’t
//  already been called to handle the event."
- (id)nextResponder {
    let delegate = msg![env; this delegate];
    let app_delegate_class = msg![env; delegate class];
    let ui_responder_class = env.objc.get_known_class("UIResponder", &mut env.mem);
    if env.objc.class_is_subclass_of(app_delegate_class, ui_responder_class) {
        // TODO: Send nil if it's already been called to handle the event
        delegate
    } else {
        nil
    }
}

- (())cancelAllLocalNotifications {
    log!("TODO: [(UIApplication*){:?} cancelAllLocalNotifications", this);
}
- (())scheduleLocalNotification:(id)local_notif { // UILocalNotification *
    log!("TODO: [(UIApplication*){:?} scheduleLocalNotification:{:?}", this, local_notif);
}

@end

};

/// `UIApplicationMain`, the entry point of the application.
///
/// This function should never return.
pub(super) fn UIApplicationMain(
    env: &mut Environment,
    _argc: i32,
    _argv: MutPtr<MutPtr<u8>>,
    principal_class_name: id, // NSString*
    delegate_class_name: id,  // NSString*
) {
    // UIKit creates and drains autorelease pools when handling events.
    // It's not clear what granularity this should happen with, but this
    // granularity has already caught several bugs. :)

    let ui_application = {
        let pool: id = msg_class![env; NSAutoreleasePool new];

        let principal_class = if principal_class_name != nil {
            let name = ns_string::to_rust_string(env, principal_class_name);
            env.objc.get_known_class(&name, &mut env.mem)
        } else {
            env.objc.get_known_class("UIApplication", &mut env.mem)
        };
        let ui_application: id = msg![env; principal_class new];

        let device_family = env.options.device_family;
        if let Some(main_nib_filename) = env.bundle.main_nib_filename(device_family) {
            let ns_main_nib_filename = from_rust_string(env, main_nib_filename.to_string());
            // We need to check first if main nib file exists,
            // as `UINib nibWithNibName:bundle:` will crash on nonexistent
            // nib otherwise
            let type_: id = get_static_str(env, "nib");
            let bundle: id = msg_class![env; NSBundle mainBundle];
            let res: id = msg![env; bundle pathForResource:ns_main_nib_filename ofType:type_];
            if res != nil {
                let nib: id = msg_class![env; UINib nibWithNibName:ns_main_nib_filename bundle:nil];
                release(env, ns_main_nib_filename);
                let _: id = msg![env; nib instantiateWithOwner:ui_application
                                               options:nil];
            } else {
                log!(
                    "Warning: couldn't load main nib file {:?}",
                    env.bundle.main_nib_filename(device_family)
                );
            }
        }

        if env.bundle.status_bar_hidden() {
            let _: () = msg![env; ui_application setStatusBarHidden:true];
        }

        let delegate: id = msg![env; ui_application delegate];
        if delegate != nil {
            // The delegate was created while loading the nib file.
            // Retain it so it doesn't get deallocated when the autorelease pool
            // is drained. (See discussion in `setDelegate:`.)
            env.objc
                .borrow_mut::<UIApplicationHostObject>(ui_application)
                .delegate_is_retained = true;
            retain(env, delegate);
        } else {
            assert!(delegate_class_name != nil);
            if msg![env; delegate_class_name isEqual:principal_class_name] {
                // If same non-nil class name is used for both principal and
                // delegate, it means that app is using itself as a delegate
                let _: () = msg![env; ui_application setDelegate:ui_application];
            } else {
                // We have to construct the delegate.
                let name = ns_string::to_rust_string(env, delegate_class_name);
                let class = env.objc.get_known_class(&name, &mut env.mem);
                let delegate: id = msg![env; class new];
                let _: () = msg![env; ui_application setDelegate:delegate];
                assert!(delegate != nil);
            }
        };
        // We can't hang on to the delegate, the guest app may change it at any
        // time.

        let _: () = msg![env; pool drain];

        ui_application
    };

    {
        let pool: id = msg_class![env; NSAutoreleasePool new];
        let delegate: id = msg![env; ui_application delegate];
        // iOS 3+ apps usually use application:didFinishLaunchingWithOptions:,
        // and it seems to be prioritized over applicationDidFinishLaunching:.
        if env.objc.object_has_method_named(
            &env.mem,
            delegate,
            "application:didFinishLaunchingWithOptions:",
        ) {
            let empty_dict: id = msg_class![env; NSDictionary dictionary];
            () = msg![env; delegate application:ui_application didFinishLaunchingWithOptions:empty_dict];
        } else if env.objc.object_has_method_named(
            &env.mem,
            delegate,
            "applicationDidFinishLaunching:",
        ) {
            () = msg![env; delegate applicationDidFinishLaunching:ui_application];
        }

        let center: id = msg_class![env; NSNotificationCenter defaultCenter];
        let notif_name = get_static_str(env, UIApplicationDidFinishLaunchingNotification);
        // TODO: launch options in `userInfo` if it'll ever become a concern
        () = msg![env; center postNotificationName:notif_name object:ui_application userInfo:nil];

        let _: () = msg![env; pool drain];
    }

    // Call layoutSubviews on all views in the view hierarchy.
    // See https://medium.com/geekculture/uiview-lifecycle-part-5-faa2d44511c9
    let views = env.framework_state.uikit.ui_view.views.clone();
    for view in views {
        () = msg![env; view layoutSubviews];
    }

    // Send applicationDidBecomeActive now that the application is ready to
    // become active.
    {
        let pool: id = msg_class![env; NSAutoreleasePool new];
        let delegate: id = msg![env; ui_application delegate];
        if env
            .objc
            .object_has_method_named(&env.mem, delegate, "applicationDidBecomeActive:")
        {
            () = msg![env; delegate applicationDidBecomeActive:ui_application];
        }

        let center: id = msg_class![env; NSNotificationCenter defaultCenter];
        let notif_name = get_static_str(env, UIApplicationDidBecomeActiveNotification);
        () = msg![env; center postNotificationName:notif_name object:ui_application userInfo:nil];

        if env
            .framework_state
            .uikit
            .ui_device
            .is_generating_device_orientation_notifications()
        {
            // This is a bit hacky...
            //
            // Some apps (e.g. "Dead Space") setup window and views only after
            // receiving a device orientation change notification.
            // Setup for this is usually done by calling
            // `[UIDevice beginGeneratingDeviceOrientationNotifications]` and
            // registering for UIDeviceOrientationDidChangeNotification
            // notification in `application:didFinishLaunchingWithOptions:`.
            //
            // Here we're helping by seeding a first device orientation change
            // just after the application becomes active.
            generate_device_orientation_notification(env);
        }

        let _: () = msg![env; pool drain];
    }

    // FIXME: There are more messages we should send.

    // TODO: It might be nicer to return from this function (even though it's
    // conceptually noreturn) and set some global flag that changes how the
    // execution works from this point onwards, though the only real advantages
    // would be a prettier backtrace and maybe the quit button not having to
    // panic.
    let run_loop: id = msg_class![env; NSRunLoop mainRunLoop];
    let _: () = msg![env; run_loop run];
}

/// Send one of the delegate's life-cycle methods (which take the
/// application) if the delegate has it, then post the matching
/// notification, in an autorelease pool of its own. The method runs to the
/// end (`msg_send` runs the guest method until it returns) before this does.
fn tell_app(env: &mut Environment, selector: &str, notification: &'static str) {
    let ui_application: id = msg_class![env; UIApplication sharedApplication];
    let pool: id = msg_class![env; NSAutoreleasePool new];

    let delegate: id = msg![env; ui_application delegate];
    let has_method = env
        .objc
        .object_has_method_named(&env.mem, delegate, selector);
    if let Some(sel) = env.objc.lookup_selector(selector).filter(|_| has_method) {
        log!("Lifecycle: sending {} to the app delegate.", selector);
        () = msg_send(env, (delegate, sel, ui_application));
    }

    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    let notif_name = get_static_str(env, notification);
    () = msg![env; center postNotificationName:notif_name object:ui_application userInfo:nil];

    let _: () = msg![env; pool drain];
}

/// Write the app's `NSUserDefaults` to the sandbox.
fn synchronize_user_defaults(env: &mut Environment) {
    // Skip NSUserDefaults code while in the app picker, otherwise we get
    // a strange error when existing touchHLE due to the fake bundle.
    if env.is_app_picker {
        return;
    }
    let pool: id = msg_class![env; NSAutoreleasePool new];
    let user_defaults: id = msg_class![env; NSUserDefaults standardUserDefaults];
    let _: bool = msg![env; user_defaults synchronize];
    let _: () = msg![env; pool drain];
}

// Home and back. iPhone OS 4 tells an app with multitasking (one whose
// Info.plist has no UIApplicationExitsOnSuspend) the following, and the app
// stays in memory in between:
// - Home: applicationWillResignActive:, then applicationDidEnterBackground:
// - back: applicationWillEnterForeground:, then applicationDidBecomeActive:
// touchHLE gets these from the host (Android's onPause and onResume through
// SDL, or F9 and F10 on a computer), see crate::window::Event. Each message
// is sent once per trip: one that would repeat the last is ignored, and one
// that skips a step sends the step first.

/// The app is about to become inactive (the first half of Home).
pub(super) fn will_resign_active(env: &mut Environment) {
    if State::get(env).resigned {
        log!("Lifecycle: already inactive, ignoring.");
        return;
    }
    State::get(env).resigned = true;
    crate::objc::trace_lifecycle(env, "resign active");
    tell_app(
        env,
        "applicationWillResignActive:",
        UIApplicationWillResignActiveNotification,
    );
}

/// The app is now in the background (the second half of Home). It stays
/// alive, presents nothing (see [crate::window::Window::is_backgrounded])
/// and has its audio paused. On Android SDL then suspends touchHLE's thread
/// until the app comes back; on a computer (F9) the app keeps running.
pub(super) fn did_enter_background(env: &mut Environment) {
    if State::get(env).in_background {
        log!("Lifecycle: already in the background, ignoring.");
        return;
    }
    if !State::get(env).resigned {
        will_resign_active(env);
    }
    State::get(env).in_background = true;
    crate::objc::trace_lifecycle(env, "background");
    // 4Towers saves the level being played only here.
    tell_app(
        env,
        "applicationDidEnterBackground:",
        UIApplicationDidEnterBackgroundNotification,
    );
    // Android may kill a process in the background without telling it, so
    // what the app saved is written to the sandbox now.
    synchronize_user_defaults(env);
    crate::frameworks::openal::set_devices_paused(env, true);
}

/// The app is coming back from the background (the first half of coming
/// back).
pub(super) fn will_enter_foreground(env: &mut Environment) {
    if !State::get(env).in_background {
        log!("Lifecycle: not in the background, ignoring.");
        return;
    }
    State::get(env).in_background = false;
    crate::objc::trace_lifecycle(env, "foreground");
    tell_app(
        env,
        "applicationWillEnterForeground:",
        UIApplicationWillEnterForegroundNotification,
    );
}

/// The app is active again (the second half of coming back).
pub(super) fn did_become_active(env: &mut Environment) {
    if State::get(env).in_background {
        will_enter_foreground(env);
    }
    if !State::get(env).resigned {
        log!("Lifecycle: already active, ignoring.");
        return;
    }
    State::get(env).resigned = false;
    crate::frameworks::openal::set_devices_paused(env, false);
    crate::objc::trace_lifecycle(env, "active");
    tell_app(
        env,
        "applicationDidBecomeActive:",
        UIApplicationDidBecomeActiveNotification,
    );
}

/// Tell the app it's about to quit and then exit. Used when the window is
/// closed (or the user quits touchHLE otherwise), and when the host
/// terminates touchHLE (Android's onDestroy, which can follow Home).
///
/// The app is first sent to the background as by Home, unless it already
/// is: `applicationWillResignActive:` and `applicationDidEnterBackground:`
/// (see [did_enter_background]) are sent only if they have not been sent
/// for this trip. Then, as a suspended app is never resumed,
/// `applicationWillTerminate:`. Each runs to completion before the next, so
/// whatever the app saves in them, for example with
/// `-[NSUserDefaults synchronize]`, is written to the sandbox before
/// touchHLE exits. The defaults are also synchronized before the first
/// message and once more at the end, for an app that changes them in these
/// methods without synchronizing.
pub(super) fn exit(env: &mut Environment) {
    // Apple's docs (used to) vaguely mention that `synchronize` is
    // invoked on periodic intervals.
    // Second best - and implemented here - is to save before app exits.
    // TODO: call `synchronize` periodically
    synchronize_user_defaults(env);

    if !State::get(env).in_background {
        log!("Lifecycle: sending the app to the background before exiting.");
        did_enter_background(env);
    }

    crate::objc::trace_lifecycle(env, "terminate");
    tell_app(
        env,
        "applicationWillTerminate:",
        UIApplicationWillTerminateNotification,
    );
    synchronize_user_defaults(env);

    // Frames the message trace's capture still has queued are written first.
    crate::objc::trace_finish(env);

    std::process::exit(0);
}

/// App life-cycle notifications
const UIApplicationDidFinishLaunchingNotification: &str =
    "UIApplicationDidFinishLaunchingNotification";
const UIApplicationDidBecomeActiveNotification: &str = "UIApplicationDidBecomeActiveNotification";
const UIApplicationDidEnterBackgroundNotification: &str =
    "UIApplicationDidEnterBackgroundNotification";
const UIApplicationWillEnterForegroundNotification: &str =
    "UIApplicationWillEnterForegroundNotification";
const UIApplicationWillResignActiveNotification: &str = "UIApplicationWillResignActiveNotification";
const UIApplicationWillTerminateNotification: &str = "UIApplicationWillTerminateNotification";
/// Other app notifications
const UIApplicationLaunchOptionsRemoteNotificationKey: &str =
    "UIApplicationLaunchOptionsRemoteNotificationKey";
const UIApplicationDidReceiveMemoryWarningNotification: &str =
    "UIApplicationDidReceiveMemoryWarningNotification";

/// `UIApplicationLaunchOptionsKey` and `NSNotificationName` values.
/// (Both types are strings)
pub const CONSTANTS: ConstantExports = &[
    (
        "_UIApplicationDidFinishLaunchingNotification",
        HostConstant::NSString(UIApplicationDidFinishLaunchingNotification),
    ),
    (
        "_UIApplicationDidBecomeActiveNotification",
        HostConstant::NSString(UIApplicationDidBecomeActiveNotification),
    ),
    (
        "_UIApplicationDidEnterBackgroundNotification",
        HostConstant::NSString(UIApplicationDidEnterBackgroundNotification),
    ),
    (
        "_UIApplicationWillEnterForegroundNotification",
        HostConstant::NSString(UIApplicationWillEnterForegroundNotification),
    ),
    (
        "_UIApplicationWillResignActiveNotification",
        HostConstant::NSString(UIApplicationWillResignActiveNotification),
    ),
    (
        "_UIApplicationWillTerminateNotification",
        HostConstant::NSString(UIApplicationWillTerminateNotification),
    ),
    (
        "_UIApplicationDidReceiveMemoryWarningNotification",
        HostConstant::NSString(UIApplicationDidReceiveMemoryWarningNotification),
    ),
    (
        "_UIApplicationLaunchOptionsRemoteNotificationKey",
        HostConstant::NSString(UIApplicationLaunchOptionsRemoteNotificationKey),
    ),
];

pub const FUNCTIONS: FunctionExports = &[export_c_func!(UIApplicationMain(_, _, _, _))];
