//! `sun.awt.windows.WToolkit` without a desktop. Windows always builds this toolkit, also
//! when headless (`Toolkit.getDefaultToolkit` wraps it in a `HeadlessToolkit`), and its
//! natives run a message pump on an "AWT-Windows" thread. rava runs AWT headless, so the
//! toolkit starts without a pump: its thread reports that it is initialized and ends, and the
//! settings it would forward to the system are dropped.

use crate::natives::{ret_bool, ret_int, ret_ref, ret_void, NativeRegistry};
use crate::runtime as rt;

const C: &str = "sun/awt/windows/WToolkit";
const PROPS: &str = "sun/awt/windows/WDesktopProperties";

pub(super) fn register(r: &mut NativeRegistry) {
    for (name, desc) in [
        ("initIDs", "()V"),
        ("postDispose", "()V"),
        ("startSecondaryEventLoop", "()V"),
        ("quitSecondaryEventLoop", "()V"),
        ("setExtraMouseButtonsEnabledNative", "(Z)V"),
    ] {
        r.add(C, name, desc, |_, _| ret_void());
    }
    for (name, desc) in [("eventLoop", "()V"), ("shutdown", "()V"), ("nativeSync", "()V"), ("beep", "()V"), ("setDynamicLayoutNative", "(Z)V"), ("loadSystemColors", "([I)V")] {
        r.add(C, name, desc, |_, _| ret_void());
    }
    r.add(C, "getWindowsVersion", "()Ljava/lang/String;", |t, _| ret_ref(rt::new_string(t, "Windows")?));
    // No native toolkit thread: WToolkit starts a Java one, whose `init` starts no pump.
    r.add(C, "startToolkitThread", "(Ljava/lang/Runnable;Ljava/lang/ThreadGroup;)Z", |_, _| ret_bool(false));
    // The toolkit thread is idle from the start: AWTAutoShutdown, told it is busy by the
    // constructor, may end the AWT threads (a pump reports this when its queue drains).
    r.add(C, "init", "()Z", |t, _| {
        crate::boot::call_static(t, "sun/awt/AWTAutoShutdown", "notifyToolkitThreadFree", "()V", &[])?;
        ret_bool(false)
    });
    r.add(C, "embeddedInit", "()Z", |_, _| ret_bool(false));
    r.add(C, "embeddedDispose", "()Z", |_, _| ret_bool(false));
    r.add(C, "isDynamicLayoutSupportedNative", "()Z", |_, _| ret_bool(false));
    r.add(C, "syncNativeQueue", "(J)Z", |_, _| ret_bool(false));
    r.add(C, "getLockingKeyStateNative", "(I)Z", |_, _| ret_bool(false));
    r.add(C, "getMaximumCursorColors", "()I", |_, _| ret_int(0));
    r.add(C, "getNumberOfButtonsImpl", "()I", |_, _| ret_int(3));
    // Desktop properties (system colors, fonts, sounds) stay unset, as on a headless toolkit.
    for (name, desc) in [("initIDs", "()V"), ("init", "()V"), ("getWindowsParameters", "()V"), ("playWindowsSound", "(Ljava/lang/String;)V")] {
        r.add(PROPS, name, desc, |_, _| ret_void());
    }
    // The graphics environment under the headless one: no displays.
    r.add("sun/java2d/windows/WindowsFlags", "initNativeFlags", "()Z", |_, _| ret_bool(false));
    let ge = "sun/awt/Win32GraphicsEnvironment";
    r.add(ge, "initDisplay", "()V", |_, _| ret_void());
    r.add(ge, "isVistaOS", "()Z", |_, _| ret_bool(true));
    r.add(ge, "getNumScreens", "()I", |_, _| ret_int(0));
    r.add(ge, "getDefaultScreen", "()I", |_, _| ret_int(0));
    r.add(ge, "getXResolution", "()I", |_, _| ret_int(96));
    r.add(ge, "getYResolution", "()I", |_, _| ret_int(96));
}
