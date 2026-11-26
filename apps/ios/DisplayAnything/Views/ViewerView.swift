import SwiftUI

/// One computer's screen. In "View only" pinch zooms and drag pans the
/// picture on the phone. "Touch" and "Trackpad" send the pointer, clicks and
/// scrolling to the computer, and the keyboard button types on it.
struct ViewerView: View {
    let desktop: Desktop
    @EnvironmentObject private var store: DesktopStore
    @Environment(\.dismiss) private var dismiss
    @StateObject private var session = ViewerSession()

    @AppStorage("controlMode") private var mode: ControlMode = .touch
    @State private var keyboard = false
    @State private var scale: CGFloat = 1
    @State private var offset: CGSize = .zero
    @State private var controlPinch: CGFloat = 1
    @GestureState private var pinch: CGFloat = 1
    @GestureState private var drag: CGSize = .zero

    var body: some View {
        GeometryReader { geo in
            ZStack {
                Color.black
                StreamLayerView { session.attach(view: $0) }
                    .scaleEffect(liveScale)
                    .offset(x: offset.width + drag.width, y: offset.height + drag.height)
                    .gesture(
                        MagnificationGesture()
                            .updating($pinch) { value, state, _ in state = value }
                            .onEnded { value in scale = clamped(scale * value) }
                    )
                    .simultaneousGesture(
                        DragGesture()
                            .updating($drag) { value, state, _ in state = value.translation }
                            .onEnded { value in
                                offset.width += value.translation.width
                                offset.height += value.translation.height
                            }
                    )
                    .onTapGesture(count: 2) {
                        withAnimation { scale = 1; offset = .zero }
                    }

                if mode != .view, session.isLive {
                    ControlSurface(delegate: session)
                }

                if mode == .cursor, session.isLive, let p = session.cursorOnScreen {
                    Image(systemName: "cursorarrow")
                        .font(.system(size: 22))
                        .foregroundStyle(.white)
                        .shadow(color: .black, radius: 1)
                        .position(x: p.x + 6, y: p.y + 9)  // tip of the arrow at the point
                        .allowsHitTesting(false)
                }

                KeyboardInput(
                    isActive: $keyboard,
                    onText: { text, mods in session.type(text, modifiers: mods) },
                    onKey: { usage, mods in session.press(usage, modifiers: mods) }
                )
                .frame(width: 1, height: 1)
                .opacity(0.01)

                overlay
            }
            .onChange(of: layout(size: geo.size), initial: true) { _, l in session.layout = l }
        }
        .ignoresSafeArea()
        // Our own buttons instead of the navigation bar: they must sit above
        // the control surface, or a tap meant for them would click the computer.
        .overlay(alignment: .top) { controls }
        .toolbar(.hidden, for: .navigationBar)
        .statusBarHidden(true)
        .task {
            session.onPinch = { state, value in
                switch state {
                case .changed: controlPinch = value
                default:
                    scale = clamped(scale * controlPinch)
                    controlPinch = 1
                }
            }
            session.start(desktop: desktop, client: store.client)
        }
        .onDisappear {
            keyboard = false
            session.stop()
        }
    }

    private var liveScale: CGFloat { clamped(scale * pinch * controlPinch) }

    private func layout(size: CGSize) -> ViewLayout {
        ViewLayout(
            size: size, scale: liveScale,
            offset: CGSize(width: offset.width + drag.width, height: offset.height + drag.height),
            video: session.videoSize)
    }

    private var controls: some View {
        HStack(spacing: 10) {
            Button {
                dismiss()
            } label: {
                Image(systemName: "chevron.left").frame(width: 40, height: 40)
            }
            .accessibilityLabel("Computers")
            Text(desktop.name)
                .font(.subheadline.weight(.semibold))
                .lineLimit(1)
            Spacer()
            Menu {
                Picker("Control", selection: $mode) {
                    ForEach(ControlMode.allCases) { m in
                        Label(m.title, systemImage: m.symbol).tag(m)
                    }
                }
            } label: {
                Image(systemName: mode.symbol).frame(width: 40, height: 40)
            }
            .accessibilityLabel("Control: \(mode.title)")
            Button {
                keyboard.toggle()
            } label: {
                Image(systemName: keyboard ? "keyboard.chevron.compact.down" : "keyboard")
                    .frame(width: 40, height: 40)
            }
            .accessibilityLabel(keyboard ? "Hide keyboard" : "Keyboard")
            .disabled(!session.isLive)
        }
        .foregroundStyle(.white)
        .padding(.horizontal, 12)
        .padding(.vertical, 2)
        .background(.black.opacity(0.45), in: Capsule())
        .padding(.horizontal, 8)
        .padding(.top, 4)
    }

    @ViewBuilder
    private var overlay: some View {
        switch session.state {
        case .connecting:
            ProgressView("Connecting…").tint(.white).foregroundStyle(.white)
        case .failed(let message):
            VStack(spacing: 12) {
                Text(message).multilineTextAlignment(.center)
                Button("Try again") { session.start(desktop: desktop, client: store.client) }
            }
            .padding()
            .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12))
            .foregroundStyle(.white)
        case .closed:
            Text("Stopped").foregroundStyle(.white)
        case .live:
            EmptyView()
        }
    }

    private func clamped(_ value: CGFloat) -> CGFloat { min(max(value, 1), 4) }
}

/// Where the picture sits on the phone: the view's size, the local zoom and
/// pan, and the size of the computer's picture.
struct ViewLayout: Equatable {
    var size: CGSize
    var scale: CGFloat
    var offset: CGSize
    var video: CGSize

    /// The picture's rectangle before zoom and pan (aspect fit).
    private var fitted: CGRect {
        guard video.width > 0, video.height > 0, size.width > 0, size.height > 0 else {
            return CGRect(origin: .zero, size: size)
        }
        let k = min(size.width / video.width, size.height / video.height)
        let w = video.width * k, h = video.height * k
        return CGRect(x: (size.width - w) / 2, y: (size.height - h) / 2, width: w, height: h)
    }

    /// The picture's size on screen, zoom included.
    var shownSize: CGSize { CGSize(width: fitted.width * scale, height: fitted.height * scale) }

    /// A point on the phone as fractions of the computer's screen.
    func fraction(at p: CGPoint) -> CGPoint {
        let cx = size.width / 2, cy = size.height / 2
        let x = (p.x - cx - offset.width) / scale + cx
        let y = (p.y - cy - offset.height) / scale + cy
        let r = fitted
        return CGPoint(x: (x - r.minX) / r.width, y: (y - r.minY) / r.height)
    }

    /// Fractions of the computer's screen as a point on the phone.
    func point(at f: CGPoint) -> CGPoint {
        let r = fitted
        let cx = size.width / 2, cy = size.height / 2
        let x = r.minX + f.x * r.width, y = r.minY + f.y * r.height
        return CGPoint(x: (x - cx) * scale + cx + offset.width, y: (y - cy) * scale + cy + offset.height)
    }
}

/// Runs one stream: opens it, shows its state, and stops it on leave.
@MainActor
final class ViewerSession: ObservableObject {
    @Published private(set) var state: StreamState = .connecting {
        didSet {
            if case .live(let size, _, _) = state { videoSize = size }
        }
    }
    @Published private(set) var videoSize: CGSize = .zero
    /// Trackpad mode: where the computer's pointer is, as fractions of its screen.
    @Published private(set) var cursor = CGPoint(x: 0.5, y: 0.5)
    var layout = ViewLayout(size: .zero, scale: 1, offset: .zero, video: .zero)
    /// Pinching in Touch or Trackpad mode still zooms the picture.
    var onPinch: ((UIGestureRecognizer.State, CGFloat) -> Void)?
    private var scrollRemainder = CGPoint.zero
    private var dragging = false
    private var lastHold = CGPoint.zero

    var isLive: Bool {
        if case .live = state { return true }
        return false
    }

    var cursorOnScreen: CGPoint? {
        layout.video == .zero ? nil : layout.point(at: cursor)
    }
    private var screen: Screen?
    private var view: StreamView?
    private var sink: StreamSink?

    func attach(view: StreamView) {
        self.view = view
    }

    func start(desktop: Desktop, client: Client?) {
        guard let client, let view else { return }
        stop()
        state = .connecting
        let sink = StreamSink(view: view)
        sink.onState = { [weak self] in self?.state = $0 }
        view.needsKeyframe = { [weak self] in
            Task { @MainActor in self?.screen?.requestKeyframe() }
        }
        self.sink = sink
        let request = ScreenRequest(
            display: 0, fps: 60, bitrateKbps: 15_000,
            maxWidth: UInt32(UIScreen.main.nativeBounds.width),
            maxHeight: UInt32(UIScreen.main.nativeBounds.height))
        Task {
            do {
                let opened = try await client.openScreen(
                    fingerprint: desktop.fingerprint, request: request, sink: sink)
                self.screen = opened
            } catch {
                self.state = .failed((error as? DaError).map(message) ?? error.localizedDescription)
            }
        }
    }

    func stop() {
        screen?.stop()
        screen = nil
        view?.reset()
    }

    // MARK: Keyboard

    func type(_ text: String, modifiers: [UInt16]) {
        _ = screen?.typeText(text: text, modifiers: modifiers)
    }

    func press(_ usage: UInt16, modifiers: [UInt16]) {
        guard let screen else { return }
        for m in modifiers { screen.key(usage: m, down: true) }
        screen.key(usage: usage, down: true)
        screen.key(usage: usage, down: false)
        for m in modifiers.reversed() { screen.key(usage: m, down: false) }
    }

    // MARK: Pointer helpers

    private var mode: ControlMode {
        ControlMode(rawValue: UserDefaults.standard.string(forKey: "controlMode") ?? "") ?? .touch
    }

    private func clamp(_ p: CGPoint) -> CGPoint {
        CGPoint(x: min(max(p.x, 0), 1), y: min(max(p.y, 0), 1))
    }

    private func pointAt(_ f: CGPoint) {
        screen?.pointAt(fx: Double(f.x), fy: Double(f.y))
    }

    private func click(_ button: PointerButton) {
        screen?.click(button: button, down: true)
        screen?.click(button: button, down: false)
    }

    /// Trackpad: move the cursor by a finger movement on screen, a little
    /// faster the faster the finger moves.
    private func moveCursor(by delta: CGPoint) {
        let shown = layout.shownSize
        guard shown.width > 0, shown.height > 0 else { return }
        let speed = hypot(delta.x, delta.y)
        let gain = 1.2 + min(speed / 12, 2.0)
        cursor = clamp(CGPoint(
            x: cursor.x + delta.x * gain / shown.width,
            y: cursor.y + delta.y * gain / shown.height))
        pointAt(cursor)
    }
}

extension ViewerSession: ControlSurfaceDelegate {
    func surfaceTap(at point: CGPoint, fingers: Int) {
        let button: PointerButton = fingers == 2 ? .right : .left
        if mode == .touch {
            let f = layout.fraction(at: point)
            guard (0...1).contains(f.x), (0...1).contains(f.y) else { return }
            cursor = f
        }
        pointAt(cursor)
        click(button)
    }

    func surfaceLongPress(_ state: UIGestureRecognizer.State, at point: CGPoint) {
        switch mode {
        case .touch:
            // Press and hold: right click where the finger is.
            if state == .began {
                cursor = clamp(layout.fraction(at: point))
                pointAt(cursor)
                click(.right)
            }
        case .cursor:
            // Press and hold, then move: drag with the left button held.
            switch state {
            case .began:
                lastHold = point
                pointAt(cursor)
                screen?.click(button: .left, down: true)
                UIImpactFeedbackGenerator(style: .light).impactOccurred()
            case .changed:
                moveCursor(by: CGPoint(x: point.x - lastHold.x, y: point.y - lastHold.y))
                lastHold = point
            default:
                screen?.click(button: .left, down: false)
            }
        case .view:
            break
        }
    }

    func surfaceDrag(_ state: UIGestureRecognizer.State, at point: CGPoint, delta: CGPoint) {
        switch mode {
        case .touch:
            // Drag with the finger: the left button is held from where it went down.
            switch state {
            case .began:
                cursor = clamp(layout.fraction(at: point))
                pointAt(cursor)
                screen?.click(button: .left, down: true)
                dragging = true
                cursor = clamp(layout.fraction(at: CGPoint(x: point.x + delta.x, y: point.y + delta.y)))
                pointAt(cursor)
            case .changed:
                cursor = clamp(layout.fraction(at: point))
                pointAt(cursor)
            default:
                if dragging { screen?.click(button: .left, down: false) }
                dragging = false
            }
        case .cursor:
            if state == .began || state == .changed { moveCursor(by: delta) }
        case .view:
            break
        }
    }

    func surfaceScroll(delta: CGPoint) {
        // Natural scrolling: the content follows the fingers. 120 units is
        // one wheel notch; four units per point of finger movement.
        let x = -delta.x * 4 + scrollRemainder.x
        let y = delta.y * 4 + scrollRemainder.y
        let dx = Int(x), dy = Int(y)
        scrollRemainder = CGPoint(x: x - CGFloat(dx), y: y - CGFloat(dy))
        if dx != 0 || dy != 0 { screen?.scroll(dx: Int32(dx), dy: Int32(dy)) }
    }

    func surfacePinch(_ state: UIGestureRecognizer.State, scale: CGFloat) {
        onPinch?(state, scale)
    }

    private func message(_ error: DaError) -> String {
        switch error {
        case .NotAllowed(let m): return m
        case .NotPaired(let m): return m
        case .Unreachable(let m): return m + "\n\nIs the computer on, and on the same network?"
        case .PairingFailed(let m), .Other(let m): return m
        }
    }
}
