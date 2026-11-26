import SwiftUI
import UIKit

/// Sends what is typed on the phone's keyboard to the desktop, with a bar of
/// keys the phone keyboard lacks (Esc, Tab, Ctrl, Alt, Win/Cmd, arrows).
/// Modifiers on the bar latch for the next key, then let go.
final class KeyboardInputView: UIView, UIKeyInput {
    /// Called with text the person typed, and the modifiers latched for it.
    var onText: ((String, [UInt16]) -> Void)?
    /// Called for a single key press, with the latched modifiers.
    var onKey: ((UInt16, [UInt16]) -> Void)?
    /// Called when the keyboard is put away from its own bar.
    var onDismiss: (() -> Void)?

    private var latched: [UInt16] = []
    private var modifierButtons: [UInt16: UIButton] = [:]
    private lazy var bar: UIInputView = makeBar()

    // The phone keyboard shouldn't change what is typed before it is sent.
    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var spellCheckingType: UITextSpellCheckingType = .no
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no
    var smartInsertDeleteType: UITextSmartInsertDeleteType = .no
    var keyboardType: UIKeyboardType = .asciiCapable
    var returnKeyType: UIReturnKeyType = .default

    override var canBecomeFirstResponder: Bool { true }
    override var inputAccessoryView: UIView? { bar }

    var hasText: Bool { true }  // so Delete always reaches us

    func insertText(_ text: String) {
        let mods = takeLatched()
        if text == "\n" {
            onKey?(HIDKey.enter, mods)
        } else {
            onText?(text, mods)
        }
    }

    func deleteBackward() {
        onKey?(HIDKey.backspace, takeLatched())
    }

    private func takeLatched() -> [UInt16] {
        let mods = latched
        latched.removeAll()
        for (_, b) in modifierButtons { b.isSelected = false }
        return mods
    }

    private func makeBar() -> UIInputView {
        let bar = UIInputView(frame: CGRect(x: 0, y: 0, width: 0, height: 44), inputViewStyle: .keyboard)
        let stack = UIStackView()
        stack.axis = .horizontal
        stack.spacing = 6
        stack.translatesAutoresizingMaskIntoConstraints = false

        func button(_ title: String, symbol: String? = nil, action: @escaping (UIButton) -> Void) -> UIButton {
            var config = UIButton.Configuration.gray()
            if let symbol {
                config.image = UIImage(systemName: symbol)
            } else {
                config.title = title
            }
            config.contentInsets = NSDirectionalEdgeInsets(top: 6, leading: 9, bottom: 6, trailing: 9)
            let b = UIButton(configuration: config, primaryAction: UIAction { a in action(a.sender as! UIButton) })
            b.accessibilityLabel = title
            b.configurationUpdateHandler = { b in
                var c = b.configuration
                c?.baseBackgroundColor = b.isSelected ? .tintColor : nil
                c?.baseForegroundColor = b.isSelected ? .white : nil
                b.configuration = c
            }
            return b
        }

        func key(_ title: String, _ usage: UInt16, symbol: String? = nil) -> UIButton {
            button(title, symbol: symbol) { [weak self] _ in
                guard let self else { return }
                self.onKey?(usage, self.takeLatched())
            }
        }

        func modifier(_ title: String, _ usage: UInt16) -> UIButton {
            let b = button(title) { [weak self] b in
                guard let self else { return }
                if let i = self.latched.firstIndex(of: usage) {
                    self.latched.remove(at: i)
                    b.isSelected = false
                } else {
                    self.latched.append(usage)
                    b.isSelected = true
                }
            }
            modifierButtons[usage] = b
            return b
        }

        let items: [UIView] = [
            key("Esc", HIDKey.escape),
            key("Tab", HIDKey.tab),
            modifier("Ctrl", HIDKey.control),
            modifier("Alt", HIDKey.alt),
            modifier("Win", HIDKey.meta),
            modifier("Shift", HIDKey.shift),
            key("Left", HIDKey.left, symbol: "arrow.left"),
            key("Up", HIDKey.up, symbol: "arrow.up"),
            key("Down", HIDKey.down, symbol: "arrow.down"),
            key("Right", HIDKey.right, symbol: "arrow.right"),
            key("Delete", HIDKey.deleteForward, symbol: "delete.right"),
            key("Home", HIDKey.home),
            key("End", HIDKey.end),
            key("Page Up", HIDKey.pageUp, symbol: "chevron.up.2"),
            key("Page Down", HIDKey.pageDown, symbol: "chevron.down.2"),
        ]
        items.forEach(stack.addArrangedSubview)

        let scroll = UIScrollView()
        scroll.showsHorizontalScrollIndicator = false
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.addSubview(stack)

        let hide = button("Hide keyboard", symbol: "keyboard.chevron.compact.down") { [weak self] _ in
            self?.onDismiss?()
        }
        hide.translatesAutoresizingMaskIntoConstraints = false

        bar.addSubview(scroll)
        bar.addSubview(hide)
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: bar.leadingAnchor, constant: 6),
            scroll.trailingAnchor.constraint(equalTo: hide.leadingAnchor, constant: -6),
            scroll.topAnchor.constraint(equalTo: bar.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: bar.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor),
            stack.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
            hide.trailingAnchor.constraint(equalTo: bar.trailingAnchor, constant: -6),
            hide.centerYAnchor.constraint(equalTo: bar.centerYAnchor),
        ])
        return bar
    }
}

/// SwiftUI wrapper: shows the keyboard while `isActive` is true.
struct KeyboardInput: UIViewRepresentable {
    @Binding var isActive: Bool
    let onText: (String, [UInt16]) -> Void
    let onKey: (UInt16, [UInt16]) -> Void

    func makeUIView(context: Context) -> KeyboardInputView {
        KeyboardInputView()
    }

    func updateUIView(_ view: KeyboardInputView, context: Context) {
        view.onText = onText
        view.onKey = onKey
        view.onDismiss = { isActive = false }
        DispatchQueue.main.async {
            if isActive, !view.isFirstResponder {
                view.becomeFirstResponder()
            } else if !isActive, view.isFirstResponder {
                view.resignFirstResponder()
            }
        }
    }
}
