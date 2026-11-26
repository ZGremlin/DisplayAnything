import SwiftUI
import UIKit

/// How touches on the picture are used.
enum ControlMode: String, CaseIterable, Identifiable {
    /// Pinch and drag move the picture on the phone; nothing is sent.
    case view
    /// The finger is the pointer: tap to click where you touch.
    case touch
    /// The phone is a trackpad: drag to move the pointer, tap to click.
    case cursor

    var id: String { rawValue }

    var title: String {
        switch self {
        case .view: return "View only"
        case .touch: return "Touch"
        case .cursor: return "Trackpad"
        }
    }

    var symbol: String {
        switch self {
        case .view: return "eye"
        case .touch: return "hand.tap"
        case .cursor: return "cursorarrow.rays"
        }
    }
}

/// What a gesture on the control surface asks for. Points are in the
/// surface's own coordinates; the viewer maps them onto the desktop.
@MainActor
protocol ControlSurfaceDelegate: AnyObject {
    func surfaceTap(at point: CGPoint, fingers: Int)
    func surfaceLongPress(_ state: UIGestureRecognizer.State, at point: CGPoint)
    func surfaceDrag(_ state: UIGestureRecognizer.State, at point: CGPoint, delta: CGPoint)
    func surfaceScroll(delta: CGPoint)
    func surfacePinch(_ state: UIGestureRecognizer.State, scale: CGFloat)
    /// Two fingers tapped twice: reset the local zoom and pan.
    func surfaceResetZoom()
}

/// A clear view over the picture that turns touches into pointer actions.
final class ControlSurfaceView: UIView, UIGestureRecognizerDelegate {
    weak var delegate: ControlSurfaceDelegate?
    private var lastDrag: CGPoint = .zero
    private var lastScroll: CGPoint = .zero
    private var lastLongPress: CGPoint = .zero

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .clear
        isMultipleTouchEnabled = true

        let tap = UITapGestureRecognizer(target: self, action: #selector(tap1(_:)))
        let twoTap = UITapGestureRecognizer(target: self, action: #selector(tap2(_:)))
        twoTap.numberOfTouchesRequired = 2
        let twoDoubleTap = UITapGestureRecognizer(target: self, action: #selector(tap2Double(_:)))
        twoDoubleTap.numberOfTouchesRequired = 2
        twoDoubleTap.numberOfTapsRequired = 2
        twoTap.require(toFail: twoDoubleTap)

        let long = UILongPressGestureRecognizer(target: self, action: #selector(longPress(_:)))
        long.minimumPressDuration = 0.25
        long.allowableMovement = 10

        let drag = UIPanGestureRecognizer(target: self, action: #selector(drag(_:)))
        drag.minimumNumberOfTouches = 1
        drag.maximumNumberOfTouches = 1

        let scroll = UIPanGestureRecognizer(target: self, action: #selector(scroll(_:)))
        scroll.minimumNumberOfTouches = 2
        scroll.maximumNumberOfTouches = 2

        let pinch = UIPinchGestureRecognizer(target: self, action: #selector(pinch(_:)))

        tap.require(toFail: twoTap)
        for g in [tap, twoTap, twoDoubleTap, long, drag, scroll, pinch] as [UIGestureRecognizer] {
            g.delegate = self
            addGestureRecognizer(g)
        }
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    func gestureRecognizer(
        _ g: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer
    ) -> Bool {
        // Two fingers can scroll and pinch at once.
        (g is UIPinchGestureRecognizer && other is UIPanGestureRecognizer)
            || (g is UIPanGestureRecognizer && other is UIPinchGestureRecognizer)
    }

    @objc private func tap1(_ g: UITapGestureRecognizer) {
        delegate?.surfaceTap(at: g.location(in: self), fingers: 1)
    }

    @objc private func tap2(_ g: UITapGestureRecognizer) {
        delegate?.surfaceTap(at: g.location(in: self), fingers: 2)
    }

    @objc private func tap2Double(_ g: UITapGestureRecognizer) {
        delegate?.surfaceResetZoom()
    }

    @objc private func longPress(_ g: UILongPressGestureRecognizer) {
        let p = g.location(in: self)
        delegate?.surfaceLongPress(g.state, at: p)
        lastLongPress = p
    }

    @objc private func drag(_ g: UIPanGestureRecognizer) {
        let t = g.translation(in: self)
        let p = g.location(in: self)
        switch g.state {
        case .began:
            // Where the finger first went down, before it moved far enough to count as a drag.
            let start = CGPoint(x: p.x - t.x, y: p.y - t.y)
            delegate?.surfaceDrag(.began, at: start, delta: t)
        default:
            delegate?.surfaceDrag(g.state, at: p, delta: CGPoint(x: t.x - lastDrag.x, y: t.y - lastDrag.y))
        }
        lastDrag = g.state == .ended || g.state == .cancelled ? .zero : t
    }

    @objc private func scroll(_ g: UIPanGestureRecognizer) {
        let t = g.translation(in: self)
        if g.state == .changed {
            delegate?.surfaceScroll(delta: CGPoint(x: t.x - lastScroll.x, y: t.y - lastScroll.y))
        }
        lastScroll = g.state == .ended || g.state == .cancelled ? .zero : t
    }

    @objc private func pinch(_ g: UIPinchGestureRecognizer) {
        delegate?.surfacePinch(g.state, scale: g.scale)
    }
}

struct ControlSurface: UIViewRepresentable {
    weak var delegate: ControlSurfaceDelegate?

    func makeUIView(context: Context) -> ControlSurfaceView {
        let view = ControlSurfaceView()
        view.delegate = delegate
        return view
    }

    func updateUIView(_ view: ControlSurfaceView, context: Context) {
        view.delegate = delegate
    }
}
