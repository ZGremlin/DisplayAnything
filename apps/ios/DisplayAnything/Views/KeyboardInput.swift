import SwiftUI
import UIKit

/// Sends what is typed on the phone's keyboard to the desktop. A row of
/// keys sits on top of the keyboard, laid out like Jump Desktop's: Esc, Tab,
/// the extended keys panel, a text box for dictation and autocorrect, the
/// arrows, Del, latching Shift/Ctrl/Alt/Win (Opt/Cmd for a Mac), and a
/// button to put the keyboard away.
final class KeyboardInputView: UIView, UIKeyInput {
    /// Called with text the person typed.
    var onText: ((String) -> Void)?
    /// Called for a single key press (Return, Delete, Esc, arrows...).
    var onKey: ((UInt16) -> Void)?
    /// Called when a modifier on the row is tapped.
    var onToggleModifier: ((UInt16) -> Void)?
    /// Called when a modifier on the row is pressed and held (locks it on).
    var onLockModifier: ((UInt16) -> Void)?
    /// Called for a ready-made shortcut on the extended keys panel.
    var onShortcut: ((Shortcut) -> Void)?
    /// Called when the text box button (pencil) is tapped.
    var onCompose: (() -> Void)?
    /// Called whenever the keyboard goes away, however that happened.
    var onDismiss: (() -> Void)?

    /// Modifiers latched for the next key (shown highlighted on the row).
    var latched: [UInt16] = [] {
        didSet { if latched != oldValue { refreshModifiers() } }
    }
    /// Modifiers locked on until tapped again (shown filled, with a lock).
    var locked: [UInt16] = [] {
        didSet { if locked != oldValue { refreshModifiers() } }
    }

    /// Labels and shortcuts follow the kind of computer.
    var hostKeys: HostKeys = .windows {
        didSet {
            guard hostKeys != oldValue else { return }
            modifierButtons[HIDKey.alt]?.accessibilityLabel = hostKeys.altLabel
            modifierButtons[HIDKey.meta]?.accessibilityLabel = hostKeys.metaLabel
            refreshModifiers()
            fillExtended()  // rebuilds the shortcuts page
        }
    }

    private var modifierButtons: [UInt16: UIButton] = [:]
    private var extendedButton: UIButton?
    private lazy var bar: UIInputView = makeBar()
    private lazy var extended: UIInputView = makeExtended()
    private var extendedContent = UIView()
    private var showingExtended = false
    private var extendedTab = 0 {
        didSet { fillExtended() }
    }

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
    override var inputView: UIView? { showingExtended ? extended : nil }

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

    /// True while the Keyboard switch is on. SwiftUI takes first responder
    /// away now and then (closing the side panel, tapping its controls);
    /// while this is set the keyboard comes straight back.
    var wantsKeyboard = false
    private var hiding = false

    @discardableResult
    override func resignFirstResponder() -> Bool {
        let done = super.resignFirstResponder()
        guard done else { return done }
        if hiding {
            hiding = false
            onDismiss?()
        } else if wantsKeyboard {
            DispatchQueue.main.async { [weak self] in
                guard let self, self.wantsKeyboard, self.window != nil, !self.isFirstResponder else { return }
                self.becomeFirstResponder()
            }
        }
        return done
    }

    /// Put the keyboard away from its own row.
    @objc private func hide() {
        hiding = true
        wantsKeyboard = false
        if showingExtended { setExtended(false) }
        resignFirstResponder()
    }

    private func setExtended(_ on: Bool) {
        showingExtended = on
        var c = extendedButton?.configuration
        c?.image = UIImage(systemName: on ? "keyboard" : "keyboard.badge.ellipsis")
        extendedButton?.configuration = c
        extendedButton?.isSelected = on
        reloadInputViews()
    }

    private func refreshModifiers() {
        for (usage, b) in modifierButtons {
            b.isSelected = latched.contains(usage) || locked.contains(usage)
            b.setNeedsUpdateConfiguration()
        }
    }

    private func modifierTitle(_ usage: UInt16) -> String {
        switch usage {
        case HIDKey.shift: return "Shift"
        case HIDKey.control: return "Ctrl"
        case HIDKey.alt: return hostKeys.altLabel
        case HIDKey.meta: return hostKeys.metaLabel
        default: return ""
        }
    }

    // MARK: The row on top of the keyboard

    /// A flat key on the row: just its label, like the keyboard's own
    /// suggestion bar, with a rounded highlight when latched.
    private func barButton(
        _ title: String, symbol: String? = nil, action: @escaping (UIButton) -> Void
    ) -> UIButton {
        var config = UIButton.Configuration.plain()
        if let symbol {
            config.image = UIImage(systemName: symbol)
            config.preferredSymbolConfigurationForImage = UIImage.SymbolConfiguration(pointSize: 17, weight: .regular)
        } else {
            config.title = title
        }
        config.contentInsets = NSDirectionalEdgeInsets(top: 6, leading: 6, bottom: 6, trailing: 6)
        config.titleTextAttributesTransformer = UIConfigurationTextAttributesTransformer { a in
            var a = a
            a.font = .systemFont(ofSize: 17, weight: .medium)
            return a
        }
        let b = UIButton(configuration: config, primaryAction: UIAction { a in
            UIDevice.current.playInputClick()
            action(a.sender as! UIButton)
        })
        b.accessibilityLabel = title
        b.widthAnchor.constraint(greaterThanOrEqualToConstant: 40).isActive = true
        b.configurationUpdateHandler = { b in
            var c = b.configuration
            c?.background.cornerRadius = 8
            // Explicit colours: the row has no keyboard behind it when a
            // hardware keyboard is attached, and the defaults read as disabled.
            c?.background.backgroundColor = b.isSelected ? .tintColor : (b.isHighlighted ? .systemFill : .clear)
            c?.baseForegroundColor = b.isSelected ? .white : .label
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
        func modifier(_ usage: UInt16) -> UIButton {
            let b = barButton(modifierTitle(usage)) { [weak self] _ in self?.onToggleModifier?(usage) }
            b.configurationUpdateHandler = { [weak self] b in
                guard let self else { return }
                var c = b.configuration
                let isLocked = self.locked.contains(usage)
                c?.title = self.modifierTitle(usage)
                // A locked modifier shows a small lock next to its name.
                c?.image = isLocked ? UIImage(systemName: "lock.fill") : nil
                c?.imagePlacement = .trailing
                c?.imagePadding = 3
                c?.preferredSymbolConfigurationForImage = UIImage.SymbolConfiguration(pointSize: 10, weight: .bold)
                c?.background.cornerRadius = 8
                c?.background.backgroundColor = b.isSelected ? .tintColor : (b.isHighlighted ? .systemFill : .clear)
                c?.baseForegroundColor = b.isSelected ? .white : .label
                b.configuration = c
            }
            // Press and hold locks the modifier on (for several clicks or keys).
            let hold = UILongPressGestureRecognizer(target: self, action: #selector(modifierHeld(_:)))
            hold.minimumPressDuration = 0.45
            b.addGestureRecognizer(hold)
            b.tag = Int(usage)
            modifierButtons[usage] = b
            return b
        }

        let ext = barButton("Extended keys", symbol: "keyboard.badge.ellipsis") { [weak self] _ in
            guard let self else { return }
            self.setExtended(!self.showingExtended)
        }
        extendedButton = ext
        let compose = barButton("Text box", symbol: "pencil") { [weak self] _ in self?.onCompose?() }

        let stack = UIStackView(arrangedSubviews: [
            key("Esc", HIDKey.escape),
            key("Tab", HIDKey.tab),
            ext,
            compose,
            key("Left", HIDKey.left, symbol: "arrow.left"),
            key("Right", HIDKey.right, symbol: "arrow.right"),
            key("Up", HIDKey.up, symbol: "arrow.up"),
            key("Down", HIDKey.down, symbol: "arrow.down"),
            key("Del", HIDKey.deleteForward),
            modifier(HIDKey.shift),
            modifier(HIDKey.control),
            modifier(HIDKey.alt),
            modifier(HIDKey.meta),
        ])
        stack.axis = .horizontal
        stack.spacing = 2
        stack.distribution = .equalSpacing
        stack.translatesAutoresizingMaskIntoConstraints = false

        let scroll = UIScrollView()
        scroll.showsHorizontalScrollIndicator = false
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.addSubview(stack)

        let hide = barButton("Hide keyboard", symbol: "arrowtriangle.down.fill") { [weak self] _ in
            self?.hide()
        }
        hide.translatesAutoresizingMaskIntoConstraints = false

        bar.addSubview(scroll)
        bar.addSubview(hide)
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: bar.safeAreaLayoutGuide.leadingAnchor, constant: 6),
            scroll.trailingAnchor.constraint(equalTo: hide.leadingAnchor, constant: -4),
            scroll.topAnchor.constraint(equalTo: bar.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: bar.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor),
            stack.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
            // Spread the keys across the row when it is wide enough (landscape,
            // iPad); scroll sideways when it isn't.
            stack.widthAnchor.constraint(greaterThanOrEqualTo: scroll.frameLayoutGuide.widthAnchor),
            {
                // Otherwise as narrow as the keys allow.
                let c = stack.widthAnchor.constraint(equalTo: scroll.frameLayoutGuide.widthAnchor)
                c.priority = .defaultLow
                return c
            }(),
            hide.trailingAnchor.constraint(equalTo: bar.safeAreaLayoutGuide.trailingAnchor, constant: -6),
            hide.centerYAnchor.constraint(equalTo: bar.centerYAnchor),
        ])
        return bar
    }

    @objc private func modifierHeld(_ g: UILongPressGestureRecognizer) {
        guard g.state == .began, let usage = g.view.map({ UInt16($0.tag) }) else { return }
        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        onLockModifier?(usage)
    }

    // MARK: Extended keys panel

    private static let tabs = ["Fn", "Keys", "123", "Shortcuts"]

    private func makeExtended() -> UIInputView {
        let panel = UIInputView(frame: CGRect(x: 0, y: 0, width: 0, height: 270), inputViewStyle: .keyboard)
        panel.allowsSelfSizing = false

        let picker = UISegmentedControl(items: Self.tabs)
        picker.selectedSegmentIndex = extendedTab
        picker.addAction(UIAction { [weak self, weak picker] _ in
            guard let self, let picker else { return }
            self.extendedTab = picker.selectedSegmentIndex
        }, for: .valueChanged)
        picker.translatesAutoresizingMaskIntoConstraints = false

        extendedContent.translatesAutoresizingMaskIntoConstraints = false
        panel.addSubview(picker)
        panel.addSubview(extendedContent)
        let guide = panel.safeAreaLayoutGuide
        NSLayoutConstraint.activate([
            picker.topAnchor.constraint(equalTo: panel.topAnchor, constant: 8),
            picker.leadingAnchor.constraint(equalTo: guide.leadingAnchor, constant: 8),
            picker.trailingAnchor.constraint(equalTo: guide.trailingAnchor, constant: -8),
            extendedContent.topAnchor.constraint(equalTo: picker.bottomAnchor, constant: 8),
            extendedContent.leadingAnchor.constraint(equalTo: guide.leadingAnchor, constant: 6),
            extendedContent.trailingAnchor.constraint(equalTo: guide.trailingAnchor, constant: -6),
            extendedContent.bottomAnchor.constraint(equalTo: guide.bottomAnchor, constant: -8),
        ])
        fillExtended()
        return panel
    }

    private func padKey(
        _ label: String, symbol: String? = nil, size: CGFloat = 20, action: @escaping () -> Void
    ) -> UIButton {
        var config = UIButton.Configuration.filled()
        config.baseBackgroundColor = .secondarySystemBackground
        config.baseForegroundColor = .label
        if let symbol {
            config.image = UIImage(systemName: symbol)
        } else {
            var title = AttributedString(label)
            title.font = .systemFont(ofSize: size, weight: .regular)
            config.attributedTitle = title
        }
        config.titleLineBreakMode = .byTruncatingTail
        config.contentInsets = NSDirectionalEdgeInsets(top: 2, leading: 2, bottom: 2, trailing: 2)
        let b = UIButton(configuration: config, primaryAction: UIAction { _ in
            UIDevice.current.playInputClick()
            action()
        })
        b.titleLabel?.adjustsFontSizeToFitWidth = true
        b.accessibilityLabel = label
        return b
    }

    private func grid(_ rows: [[UIButton]]) -> UIStackView {
        let g = UIStackView(arrangedSubviews: rows.map { row in
            let r = UIStackView(arrangedSubviews: row)
            r.axis = .horizontal
            r.spacing = 6
            r.distribution = .fillEqually
            return r
        })
        g.axis = .vertical
        g.spacing = 6
        g.distribution = .fillEqually
        return g
    }

    private func fillExtended() {
        extendedContent.subviews.forEach { $0.removeFromSuperview() }
        func key(_ label: String, _ usage: UInt16, symbol: String? = nil) -> UIButton {
            padKey(label, symbol: symbol) { [weak self] in self?.onKey?(usage) }
        }
        func text(_ s: String) -> UIButton { padKey(s) { [weak self] in self?.onText?(s) } }

        let content: UIStackView
        switch extendedTab {
        case 0:
            // Function keys, plus the keys that sit around them.
            content = grid([
                (1...6).map { key("F\($0)", HIDKey.function($0)) },
                (7...12).map { key("F\($0)", HIDKey.function($0)) },
                [key("PrtSc", HIDKey.printScreen), key("Ins", HIDKey.insert),
                 key("Esc", HIDKey.escape), key("Tab", HIDKey.tab),
                 key("Space", HIDKey.space), key("Enter", HIDKey.enter, symbol: "return")],
            ])
        case 1:
            // Navigation keys, laid out like a full keyboard's.
            content = grid([
                [key("Home", HIDKey.home), key("Up", HIDKey.up, symbol: "arrow.up"),
                 key("End", HIDKey.end), key("Page Up", HIDKey.pageUp)],
                [key("Left", HIDKey.left, symbol: "arrow.left"), key("Down", HIDKey.down, symbol: "arrow.down"),
                 key("Right", HIDKey.right, symbol: "arrow.right"), key("Page Down", HIDKey.pageDown)],
                [key("Backspace", HIDKey.backspace, symbol: "delete.left"),
                 key("Delete", HIDKey.deleteForward, symbol: "delete.right"),
                 key("Insert", HIDKey.insert), key("Enter", HIDKey.enter, symbol: "return")],
            ])
        case 2:
            // Operators and Enter are the computer's keypad keys (they work with
            // Num Lock on or off). Digits and the point are the main-row keys, so
            // they type numbers even when Num Lock is off.
            content = grid([
                [text("7"), text("8"), text("9"), key("/", HIDKey.keypadDivide), text("(")],
                [text("4"), text("5"), text("6"), key("*", HIDKey.keypadMultiply), text(")")],
                [text("1"), text("2"), text("3"), key("-", HIDKey.keypadMinus),
                 key("Backspace", HIDKey.backspace, symbol: "delete.left")],
                [text("0"), text("."), text("="), key("+", HIDKey.keypadPlus),
                 key("Enter", HIDKey.keypadEnter, symbol: "return")],
            ])
        default:
            let buttons = hostKeys.shortcuts.map { s in
                padKey(s.title, size: 14) { [weak self] in self?.onShortcut?(s) }
            }
            let rows = stride(from: 0, to: buttons.count, by: 4).map { i in
                Array(buttons[i..<min(i + 4, buttons.count)])
            }
            content = grid(rows)
        }
        content.translatesAutoresizingMaskIntoConstraints = false
        extendedContent.addSubview(content)
        NSLayoutConstraint.activate([
            content.leadingAnchor.constraint(equalTo: extendedContent.leadingAnchor),
            content.trailingAnchor.constraint(equalTo: extendedContent.trailingAnchor),
            content.topAnchor.constraint(equalTo: extendedContent.topAnchor),
            content.bottomAnchor.constraint(equalTo: extendedContent.bottomAnchor),
        ])
    }
}

extension KeyboardInputView: UIInputViewAudioFeedback {
    var enableInputClicksWhenVisible: Bool { true }
}

/// SwiftUI wrapper: shows the keyboard while `isActive` is true.
struct KeyboardInput: UIViewRepresentable {
    @Binding var isActive: Bool
    let latched: [UInt16]
    let locked: [UInt16]
    let hostKeys: HostKeys
    let onText: (String) -> Void
    let onKey: (UInt16) -> Void
    let onToggleModifier: (UInt16) -> Void
    let onLockModifier: (UInt16) -> Void
    let onShortcut: (Shortcut) -> Void
    let onCompose: () -> Void

    func makeUIView(context: Context) -> KeyboardInputView {
        KeyboardInputView()
    }

    func updateUIView(_ view: KeyboardInputView, context: Context) {
        view.onText = onText
        view.onKey = onKey
        view.onToggleModifier = onToggleModifier
        view.onLockModifier = onLockModifier
        view.onShortcut = onShortcut
        view.onCompose = onCompose
        view.hostKeys = hostKeys
        view.latched = latched
        view.locked = locked
        let binding = $isActive
        view.onDismiss = {
            if binding.wrappedValue { binding.wrappedValue = false }
        }
        let wanted = isActive
        view.wantsKeyboard = wanted
        DispatchQueue.main.async {
            if wanted, !view.isFirstResponder, view.window != nil {
                view.becomeFirstResponder()
            } else if !wanted, view.isFirstResponder {
                view.resignFirstResponder()
            }
        }
    }
}
