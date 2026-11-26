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

    static let space: UInt16 = 0x2C
    static let printScreen: UInt16 = 0x46
    static let insert: UInt16 = 0x49

    /// F1 to F12 (`n` from 1 to 12).
    static func function(_ n: Int) -> UInt16 { UInt16(0x3A + (n - 1)) }

    /// A letter key, by its lowercase letter ("a" to "z").
    static func letter(_ c: Character) -> UInt16 {
        let v = c.lowercased().unicodeScalars.first!.value
        return UInt16(0x04 + Int(v) - Int(("a" as Unicode.Scalar).value))
    }

    /// A digit on the main row ("0" to "9").
    static func digit(_ c: Character) -> UInt16 {
        c == "0" ? 0x27 : UInt16(0x1E + Int(String(c))! - 1)
    }

    static let control: UInt16 = 0xE0
    static let shift: UInt16 = 0xE1
    static let alt: UInt16 = 0xE2
    /// Windows key on a PC, Command on a Mac.
    static let meta: UInt16 = 0xE3
}
