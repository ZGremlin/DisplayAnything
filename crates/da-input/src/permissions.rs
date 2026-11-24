//! OS permission checks.

/// macOS: whether this app may watch and synthesise input (Accessibility).
/// With `prompt`, macOS shows its "allow in System Settings" dialog the
/// first time. Always true on other platforms.
#[cfg(target_os = "macos")]
pub fn accessibility_trusted(prompt: bool) -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::{CFString, CFStringRef};

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrustedWithOptions(
            options: core_foundation::dictionary::CFDictionaryRef,
        ) -> bool;
        static kAXTrustedCheckOptionPrompt: CFStringRef;
    }
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let value = if prompt {
            CFBoolean::true_value()
        } else {
            CFBoolean::false_value()
        };
        let opts = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), value.as_CFType())]);
        AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef())
    }
}

/// macOS: whether this app may see keystrokes from other apps (Input
/// Monitoring). Without it, macOS still delivers mouse events to our event
/// tap but quietly withholds key events. With `prompt`, macOS asks and adds
/// the app to the Input Monitoring list. Always true on other platforms.
#[cfg(target_os = "macos")]
pub fn input_monitoring_granted(prompt: bool) -> bool {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightListenEventAccess() -> bool;
        fn CGRequestListenEventAccess() -> bool;
    }
    unsafe {
        if CGPreflightListenEventAccess() {
            true
        } else if prompt {
            CGRequestListenEventAccess()
        } else {
            false
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn input_monitoring_granted(_prompt: bool) -> bool {
    true
}

/// Everything keyboard and mouse sharing needs on this OS.
pub fn input_permissions_ok(prompt: bool) -> bool {
    let ax = accessibility_trusted(prompt);
    let listen = input_monitoring_granted(prompt);
    ax && listen
}

#[cfg(not(target_os = "macos"))]
pub fn accessibility_trusted(_prompt: bool) -> bool {
    true
}
