import SwiftUI
import UIKit

/// Sends what is typed on the phone's keyboard to the desktop. A row of
/// keys sits on top of the keyboard: Esc, Tab, latching Ctrl/Alt/Win/Shift,
/// arrows, a switch to a number pad, and a button to put the keyboard away.
final class KeyboardInputView: UIView, UIKeyInput {
    /// Called with text the person typed.
    var onText: ((String) -> Void)?
    /// Called for a single key press (Return, Delete, Esc, arrows...).
    var onKey: ((UInt16) -> Void)?
    /// Called when a modifier on the row is tapped.
    var onToggleModifier: ((UInt16) -> Void)?
    /// Called whenever the keyboard goes away, however that happened.
    var onDismiss: (() -> Void)?

    /// Modifiers currently latched (shown highlighted on the row).
    var latched: [UInt16] = [] {
        didSet {
            guard latched != oldValue else { return }
            for (usage, b) in modifierButtons { b.isSelected = latched.contains(usage) }
        }
    }

    private var modifierButtons: [UInt16: UIButton] = [:]
    private var padButton: UIButton?
    private lazy var bar: UIInputView = makeBar()
    private lazy var numberPad: UIInputView = makeNumberPad()
    private var showingPad = false

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
    override var inputView: UIView? { showingPad ? numberPad : nil }

    var hasText: Bool { true }  // so Delete always reaches us

    func insertText(_ text: String) {
        if text == "\n" {
            onKey?(HIDKey.enter)
        } else {
            onText?(text)
        }
    }

    func deleteBackward() {
        onKey?(HIDKey.backspace)
    }

    @discardableResult
    override func resignFirstResponder() -> Bool {
        let done = super.resignFirstResponder()
        if done { onDismiss?() }
        return done
    }

    /// Put the keyboard away from its own row.
    @objc private func hide() {
        resignFirstResponder()
    }

    private func togglePad() {
        showingPad.toggle()
        var c = padButton?.configuration
        c?.title = showingPad ? "ABC" : "123"
        padButton?.configuration = c
        reloadInputViews()
    }

    // MARK: The row on top of the keyboard

    private func barButton(
        _ title: String, symbol: String? = nil, action: @escaping (UIButton) -> Void
    ) -> UIButton {
        var config = UIButton.Configuration.gray()
        if let symbol {
            config.image = UIImage(systemName: symbol)
        } else {
            config.title = title
        }
        config.buttonSize = .small
        config.contentInsets = NSDirectionalEdgeInsets(top: 6, leading: 10, bottom: 6, trailing: 10)
        let b = UIButton(configuration: config, primaryAction: UIAction { a in
            action(a.sender as! UIButton)
        })
        b.accessibilityLabel = title
        b.configurationUpdateHandler = { b in
            var c = b.configuration
            c?.baseBackgroundColor = b.isSelected ? .tintColor : nil
            c?.baseForegroundColor = b.isSelected ? .white : nil
            b.configuration = c
        }
        return b
    }

    private func makeBar() -> UIInputView {
        let bar = UIInputView(frame: CGRect(x: 0, y: 0, width: 0, height: 46), inputViewStyle: .keyboard)
        bar.allowsSelfSizing = false

        func key(_ title: String, _ usage: UInt16, symbol: String? = nil) -> UIButton {
            barButton(title, symbol: symbol) { [weak self] _ in self?.onKey?(usage) }
        }
        func modifier(_ title: String, _ usage: UInt16) -> UIButton {
            let b = barButton(title) { [weak self] _ in self?.onToggleModifier?(usage) }
            modifierButtons[usage] = b
            return b
        }

        let stack = UIStackView(arrangedSubviews: [
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
        ])
        stack.axis = .horizontal
        stack.spacing = 6
        stack.translatesAutoresizingMaskIntoConstraints = false

        let scroll = UIScrollView()
        scroll.showsHorizontalScrollIndicator = false
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.addSubview(stack)

        let pad = barButton("123") { [weak self] _ in self?.togglePad() }
        pad.accessibilityLabel = "Number pad"
        padButton = pad
        let hide = barButton("Hide keyboard", symbol: "keyboard.chevron.compact.down") { [weak self] _ in
            self?.hide()
        }
        let right = UIStackView(arrangedSubviews: [pad, hide])
        right.axis = .horizontal
        right.spacing = 6
        right.translatesAutoresizingMaskIntoConstraints = false

        bar.addSubview(scroll)
        bar.addSubview(right)
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: bar.safeAreaLayoutGuide.leadingAnchor, constant: 6),
            scroll.trailingAnchor.constraint(equalTo: right.leadingAnchor, constant: -6),
            scroll.topAnchor.constraint(equalTo: bar.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: bar.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor),
            stack.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
            right.trailingAnchor.constraint(equalTo: bar.safeAreaLayoutGuide.trailingAnchor, constant: -6),
            right.centerYAnchor.constraint(equalTo: bar.centerYAnchor),
        ])
        return bar
    }

    // MARK: Number pad

    private func makeNumberPad() -> UIInputView {
        let pad = UIInputView(frame: CGRect(x: 0, y: 0, width: 0, height: 260), inputViewStyle: .keyboard)
        pad.allowsSelfSizing = false

        func padKey(_ label: String, symbol: String? = nil, action: @escaping () -> Void) -> UIButton {
            var config = UIButton.Configuration.filled()
            config.baseBackgroundColor = .secondarySystemBackground
            config.baseForegroundColor = .label
            if let symbol {
                config.image = UIImage(systemName: symbol)
            } else {
                var title = AttributedString(label)
                title.font = .systemFont(ofSize: 24, weight: .regular)
                config.attributedTitle = title
            }
            let b = UIButton(configuration: config, primaryAction: UIAction { _ in
                UIDevice.current.playInputClick()
                action()
            })
            b.accessibilityLabel = label
            return b
        }
        func text(_ s: String) -> UIButton { padKey(s) { [weak self] in self?.onText?(s) } }
        func key(_ label: String, _ usage: UInt16, symbol: String? = nil) -> UIButton {
            padKey(label, symbol: symbol) { [weak self] in self?.onKey?(usage) }
        }

        // Operators and Enter are the computer's keypad keys (they work with
        // Num Lock on or off). Digits and the point are the main-row keys, so
        // they type numbers even when Num Lock is off.
        let rows: [[UIButton]] = [
            [text("7"), text("8"), text("9"), key("/", HIDKey.keypadDivide), text("(")],
            [text("4"), text("5"), text("6"), key("*", HIDKey.keypadMultiply), text(")")],
            [text("1"), text("2"), text("3"), key("-", HIDKey.keypadMinus),
             key("Backspace", HIDKey.backspace, symbol: "delete.left")],
            [text("0"), text("."), text("="), key("+", HIDKey.keypadPlus),
             key("Enter", HIDKey.keypadEnter, symbol: "return")],
        ]
        let grid = UIStackView(arrangedSubviews: rows.map { row in
            let r = UIStackView(arrangedSubviews: row)
            r.axis = .horizontal
            r.spacing = 6
            r.distribution = .fillEqually
            return r
        })
        grid.axis = .vertical
        grid.spacing = 6
        grid.distribution = .fillEqually
        grid.translatesAutoresizingMaskIntoConstraints = false
        pad.addSubview(grid)
        NSLayoutConstraint.activate([
            grid.leadingAnchor.constraint(equalTo: pad.safeAreaLayoutGuide.leadingAnchor, constant: 6),
            grid.trailingAnchor.constraint(equalTo: pad.safeAreaLayoutGuide.trailingAnchor, constant: -6),
            grid.topAnchor.constraint(equalTo: pad.topAnchor, constant: 8),
            grid.bottomAnchor.constraint(equalTo: pad.safeAreaLayoutGuide.bottomAnchor, constant: -8),
        ])
        return pad
    }
}

extension KeyboardInputView: UIInputViewAudioFeedback {
    var enableInputClicksWhenVisible: Bool { true }
}

/// SwiftUI wrapper: shows the keyboard while `isActive` is true.
struct KeyboardInput: UIViewRepresentable {
    @Binding var isActive: Bool
    let latched: [UInt16]
    let onText: (String) -> Void
    let onKey: (UInt16) -> Void
    let onToggleModifier: (UInt16) -> Void

    func makeUIView(context: Context) -> KeyboardInputView {
        KeyboardInputView()
    }

    func updateUIView(_ view: KeyboardInputView, context: Context) {
        view.onText = onText
        view.onKey = onKey
        view.onToggleModifier = onToggleModifier
        view.latched = latched
        let binding = $isActive
        view.onDismiss = {
            if binding.wrappedValue { binding.wrappedValue = false }
        }
        let wanted = isActive
        DispatchQueue.main.async {
            if wanted, !view.isFirstResponder, view.window != nil {
                view.becomeFirstResponder()
            } else if !wanted, view.isFirstResponder {
                view.resignFirstResponder()
            }
        }
    }
}
