import Foundation

/// Which kind of computer the keys are labelled for. The key codes sent are
/// the same either way (Alt is Option and Win is Command on a Mac); this
/// only changes the labels and the ready-made shortcuts. Kept per computer.
enum HostKeys: String, CaseIterable, Identifiable {
    case windows, mac

    var id: String { rawValue }

    var title: String {
        switch self {
        case .windows: return "Windows"
        case .mac: return "Mac"
        }
    }

    static func key(for fingerprint: String) -> String { "hostKeys.\(fingerprint)" }

    static func saved(for fingerprint: String) -> HostKeys {
        HostKeys(rawValue: UserDefaults.standard.string(forKey: key(for: fingerprint)) ?? "") ?? .windows
    }

    var altLabel: String { self == .mac ? "Opt" : "Alt" }
    var metaLabel: String { self == .mac ? "Cmd" : "Win" }

    /// The modifier used for everyday shortcuts (copy, paste...).
    var command: UInt16 { self == .mac ? HIDKey.meta : HIDKey.control }

    /// Ready-made key combinations for the extended keys panel.
    var shortcuts: [Shortcut] {
        let c = command
        let common: [Shortcut] = [
            Shortcut("Copy", [c], HIDKey.letter("c")),
            Shortcut("Paste", [c], HIDKey.letter("v")),
            Shortcut("Cut", [c], HIDKey.letter("x")),
            Shortcut("Undo", [c], HIDKey.letter("z")),
            Shortcut("Redo", self == .mac ? [c, HIDKey.shift] : [c], self == .mac ? HIDKey.letter("z") : HIDKey.letter("y")),
            Shortcut("Select all", [c], HIDKey.letter("a")),
            Shortcut("Find", [c], HIDKey.letter("f")),
            Shortcut("Save", [c], HIDKey.letter("s")),
        ]
        switch self {
        case .mac:
            return common + [
                Shortcut("Switch app", [HIDKey.meta], HIDKey.tab),
                Shortcut("Spotlight", [HIDKey.meta], HIDKey.space),
                Shortcut("Mission Control", [HIDKey.control], HIDKey.up),
                Shortcut("App windows", [HIDKey.control], HIDKey.down),
                Shortcut("Space left", [HIDKey.control], HIDKey.left),
                Shortcut("Space right", [HIDKey.control], HIDKey.right),
                Shortcut("Close window", [HIDKey.meta], HIDKey.letter("w")),
                Shortcut("Quit app", [HIDKey.meta], HIDKey.letter("q")),
                Shortcut("Full screen", [HIDKey.control, HIDKey.meta], HIDKey.letter("f")),
                Shortcut("Screenshot", [HIDKey.meta, HIDKey.shift], HIDKey.digit("4")),
                Shortcut("Force quit", [HIDKey.meta, HIDKey.alt], HIDKey.escape),
                Shortcut("Lock screen", [HIDKey.control, HIDKey.meta], HIDKey.letter("q")),
            ]
        case .windows:
            return common + [
                Shortcut("Switch app", [HIDKey.alt], HIDKey.tab),
                Shortcut("Start", [], HIDKey.meta),
                Shortcut("Task view", [HIDKey.meta], HIDKey.tab),
                Shortcut("Show desktop", [HIDKey.meta], HIDKey.letter("d")),
                Shortcut("Desktop left", [HIDKey.control, HIDKey.meta], HIDKey.left),
                Shortcut("Desktop right", [HIDKey.control, HIDKey.meta], HIDKey.right),
                Shortcut("Close window", [HIDKey.alt], HIDKey.function(4)),
                Shortcut("File Explorer", [HIDKey.meta], HIDKey.letter("e")),
                Shortcut("Run", [HIDKey.meta], HIDKey.letter("r")),
                Shortcut("Snip", [HIDKey.meta, HIDKey.shift], HIDKey.letter("s")),
                Shortcut("Task Manager", [HIDKey.control, HIDKey.shift], HIDKey.escape),
                Shortcut("Lock screen", [HIDKey.meta], HIDKey.letter("l")),
            ]
        }
    }
}

/// One named key combination: modifiers held around one key.
struct Shortcut: Identifiable {
    let title: String
    let modifiers: [UInt16]
    let key: UInt16
    var id: String { title }

    init(_ title: String, _ modifiers: [UInt16], _ key: UInt16) {
        self.title = title
        self.modifiers = modifiers
        self.key = key
    }
}

/// Viewer options in the control panel, kept between sessions.
enum ViewerSetting {
    /// Two-finger scrolling moves the content with the fingers (like iOS).
    static let naturalScroll = "naturalScroll"
    /// Trackpad pointer speed multiplier, 0.5 to 2.
    static let pointerSpeed = "pointerSpeed"
    /// A light tap on the phone for right clicks and drags.
    static let haptics = "haptics"
    /// Shrink the picture to sit above the on-screen keyboard.
    static let fitAboveKeyboard = "fitAboveKeyboard"
    /// Frame rate, bit rate and size in a corner of the screen.
    static let showStats = "showStats"
    /// Keep the phone from locking while connected.
    static let keepAwake = "keepAwake"

    static func bool(_ key: String, default value: Bool) -> Bool {
        UserDefaults.standard.object(forKey: key) as? Bool ?? value
    }
}
