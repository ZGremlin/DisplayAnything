/// USB HID usage codes (keyboard page 0x07) for the keys the phone sends
/// directly. Ordinary characters go through `Screen.typeText`.
enum HIDKey {
    static let enter: UInt16 = 0x28
    static let escape: UInt16 = 0x29
    static let backspace: UInt16 = 0x2A
    static let tab: UInt16 = 0x2B
    static let deleteForward: UInt16 = 0x4C
    static let home: UInt16 = 0x4A
    static let pageUp: UInt16 = 0x4B
    static let end: UInt16 = 0x4D
    static let pageDown: UInt16 = 0x4E
    static let right: UInt16 = 0x4F
    static let left: UInt16 = 0x50
    static let down: UInt16 = 0x51
    static let up: UInt16 = 0x52

    static let keypadDivide: UInt16 = 0x54
    static let keypadMultiply: UInt16 = 0x55
    static let keypadMinus: UInt16 = 0x56
    static let keypadPlus: UInt16 = 0x57
    static let keypadEnter: UInt16 = 0x58

    static let control: UInt16 = 0xE0
    static let shift: UInt16 = 0xE1
    static let alt: UInt16 = 0xE2
    /// Windows key on a PC, Command on a Mac.
    static let meta: UInt16 = 0xE3
}
