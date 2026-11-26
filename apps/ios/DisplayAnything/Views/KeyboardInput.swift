import SwiftUI
import UIKit

/// Sends what is typed on the phone's keyboard to the desktop. Keys the
/// phone keyboard lacks (Esc, Tab, Ctrl...) are on the key bar at the right
/// edge of the viewer.
final class KeyboardInputView: UIView, UIKeyInput {
    /// Called with text the person typed.
    var onText: ((String) -> Void)?
    /// Called for a single key press (Return, Delete).
    var onKey: ((UInt16) -> Void)?
    /// Called when the keyboard is put away from its own bar.
    var onDismiss: (() -> Void)?

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
        if text == "\n" {
            onKey?(HIDKey.enter)
        } else {
            onText?(text)
        }
    }

    func deleteBackward() {
        onKey?(HIDKey.backspace)
    }

    /// A slim bar with a button to put the keyboard away.
    private func makeBar() -> UIInputView {
        let bar = UIInputView(frame: CGRect(x: 0, y: 0, width: 0, height: 40), inputViewStyle: .keyboard)
        var config = UIButton.Configuration.plain()
        config.image = UIImage(systemName: "keyboard.chevron.compact.down")
        let hide = UIButton(configuration: config, primaryAction: UIAction { [weak self] _ in
            self?.onDismiss?()
        })
        hide.accessibilityLabel = "Hide keyboard"
        hide.translatesAutoresizingMaskIntoConstraints = false
        bar.addSubview(hide)
        NSLayoutConstraint.activate([
            hide.trailingAnchor.constraint(equalTo: bar.trailingAnchor, constant: -8),
            hide.centerYAnchor.constraint(equalTo: bar.centerYAnchor),
        ])
        return bar
    }
}

/// SwiftUI wrapper: shows the keyboard while `isActive` is true.
struct KeyboardInput: UIViewRepresentable {
    @Binding var isActive: Bool
    let onText: (String) -> Void
    let onKey: (UInt16) -> Void

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
