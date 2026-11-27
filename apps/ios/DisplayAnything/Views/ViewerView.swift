import AVFoundation
import SwiftUI


/// One computer's screen, edge to edge. The controls stay hidden: swipe in
/// from the left edge for the control panel (Touch, Trackpad or View only,
/// monitor, keyboard, key bar, text box, settings, disconnect) and from the
/// right edge for the key bar (Esc, Tab, Ctrl, Alt, Win, arrows and so on).
/// Tap with three fingers to show or hide the keyboard.
struct ViewerView: View {
    let desktop: Desktop
    @EnvironmentObject private var store: DesktopStore
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @StateObject private var session = ViewerSession()

    @AppStorage("controlMode") private var mode: ControlMode = .cursor
    @AppStorage(StreamQuality.key) private var quality: StreamQuality = .auto
    @AppStorage(ViewerSetting.naturalScroll) private var naturalScroll = true
    @AppStorage(ViewerSetting.pointerSpeed) private var pointerSpeed = 1.0
    @AppStorage(ViewerSetting.haptics) private var haptics = true
    @AppStorage(ViewerSetting.fitAboveKeyboard) private var fitAboveKeyboard = true
    @AppStorage(ViewerSetting.showStats) private var showStats = false
    @AppStorage(ViewerSetting.keepAwake) private var keepAwake = true
    @State private var hostKeys: HostKeys = .windows
    @AppStorage(ViewerSession.soundKey) private var sound = true
    @State private var keyboard = false
    @State private var composing = false
    @State private var composeText = ""
    /// Whether the keyboard was up when the text box opened (it comes back after).
    @State private var keyboardBeforeCompose = false
    @FocusState private var composeFocused: Bool
    /// How much of the screen the on-screen keyboard (and its key row) covers.
    @State private var keyboardInset: CGFloat = 0
    @State private var panel = false
    @State private var keyBar = false
    @State private var scale: CGFloat = 1
    @State private var offset: CGSize = .zero
    @State private var controlPinch: CGFloat = 1
    @GestureState private var pinch: CGFloat = 1
    @GestureState private var drag: CGSize = .zero

    var body: some View {
        GeometryReader { geo in
            let area = pictureArea(geo.size)
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
                    latched: session.latched,
                    locked: session.locked,
                    hostKeys: hostKeys,
                    onText: { text in session.type(text) },
                    onKey: { usage in session.press(usage) },
                    onToggleModifier: { usage in session.toggleLatch(usage) },
                    onLockModifier: { usage in session.toggleLock(usage) },
                    onShortcut: { shortcut in session.press(shortcut) },
                    onCompose: { openCompose() }
                )
                .frame(width: 1, height: 1)
                .opacity(0.01)

                overlay
            }
            // With "Fit above keyboard" on, the picture shrinks to the part of
            // the screen the keyboard leaves free.
            .frame(width: area.width, height: area.height)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .background(Color.black)
            .clipped()
            .onChange(of: layout(size: area), initial: true) { _, l in session.layout = l }
        }
        .ignoresSafeArea()
        .overlay(alignment: .top) { statsBadge }
        .overlay(alignment: .top) { composeBox }
        // Everything below sits above the control surface, so touches on it
        // never reach the computer.
        .overlay { edges }
        .toolbar(.hidden, for: .navigationBar)
        // Also turns off the system's swipe-from-the-left-edge "back", which
        // would otherwise fight the swipe that opens the control panel.
        .navigationBarBackButtonHidden(true)
        .statusBarHidden(true)
        .persistentSystemOverlays(.hidden)
        .task {
            session.onPinch = { state, value in
                switch state {
                case .changed: controlPinch = value
                default:
                    scale = clamped(scale * controlPinch)
                    controlPinch = 1
                    DispatchQueue.main.async { offset = scale <= 1 ? .zero : clampedOffset(offset) }
                }
            }
            session.onPan = { delta in
                let next = clampedOffset(CGSize(
                    width: offset.width + delta.x, height: offset.height + delta.y))
                offset = next
                // Hand the session the offset actually used (after clamping)
                // right away, so the cursor it draws matches the picture.
                session.layout.offset = next
            }
            session.onResetZoom = {
                withAnimation(.snappy) { scale = 1; offset = .zero }
            }
            session.onToggleKeyboard = {
                if composing { closeCompose() } else { keyboard.toggle() }
            }
            hostKeys = HostKeys.saved(for: desktop.fingerprint)
            session.start(desktop: desktop, client: store.client)
        }
        .onChange(of: mode) { _, _ in
            session.releaseAll()
            session.syncPointer()
        }
        .onChange(of: quality) { _, q in session.apply(q) }
        .onChange(of: hostKeys) { _, k in
            UserDefaults.standard.set(k.rawValue, forKey: HostKeys.key(for: desktop.fingerprint))
        }
        .onChange(of: session.isLive && keepAwake, initial: true) { _, awake in
            UIApplication.shared.isIdleTimerDisabled = awake
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillChangeFrameNotification)) { note in
            keyboardInset = Self.overlap(note)
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillHideNotification)) { _ in
            keyboardInset = 0
        }
        .onChange(of: sound) { _, on in session.setSound(on) }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { session.releaseAll() }
        }
        .onDisappear {
            keyboard = false
            UIApplication.shared.isIdleTimerDisabled = false
            session.stop()
        }
    }

    // MARK: Edge panels

    private var edges: some View {
        ZStack {
            // Tapping the picture while a panel is open closes it.
            if panel || keyBar {
                Color.black.opacity(0.001)
                    .contentShape(Rectangle())
                    .onTapGesture { closePanels() }
            }
            HStack(spacing: 0) {
                if panel {
                    controlPanel
                        .transition(.move(edge: .leading).combined(with: .opacity))
                } else {
                    edgeStrip(opensFrom: .leading)
                }
                Spacer(minLength: 0)
                if keyBar {
                    KeyBar(session: session, hostKeys: hostKeys)
                        .transition(.move(edge: .trailing).combined(with: .opacity))
                } else {
                    edgeStrip(opensFrom: .trailing)
                }
            }
        }
        .overlay(alignment: .topLeading) {
            if !panel {
                Button {
                    session.releaseAll()
                    dismiss()
                } label: {
                    Image(systemName: "chevron.left")
                        .font(.body.weight(.semibold))
                        .foregroundStyle(.white)
                        .frame(width: 38, height: 38)
                        .background(.black.opacity(0.4), in: Circle())
                }
                .accessibilityLabel("Computers")
                .padding(.leading, 14)
                .padding(.top, 6)
            }
        }
        .animation(.snappy(duration: 0.25), value: panel)
        .animation(.snappy(duration: 0.25), value: keyBar)
    }

    /// A thin strip along a screen edge: swipe in from it to open that
    /// edge's panel. A faint handle shows where it is.
    private func edgeStrip(opensFrom edge: HorizontalEdge) -> some View {
        Color.clear
            .frame(width: 22)
            .frame(maxHeight: .infinity)
            .contentShape(Rectangle())
            .overlay {
                Capsule()
                    .fill(.white.opacity(0.28))
                    .frame(width: 4, height: 44)
                    .padding(edge == .leading ? .leading : .trailing, 3)
                    .frame(maxWidth: .infinity, alignment: edge == .leading ? .leading : .trailing)
            }
            .gesture(
                DragGesture(minimumDistance: 8)
                    .onEnded { value in
                        let inward = edge == .leading ? value.translation.width : -value.translation.width
                        guard inward > 30 else { return }
                        if edge == .leading { panel = true } else { keyBar = true }
                    }
            )
            .onTapGesture {
                if edge == .leading { panel = true } else { keyBar = true }
            }
            .accessibilityLabel(edge == .leading ? "Show controls" : "Show keys")
            .accessibilityAddTraits(.isButton)
    }

    private var controlPanel: some View {
        ScrollView(.vertical, showsIndicators: false) {
            panelContent.padding(16)
        }
        .frame(width: 280)
        .frame(maxHeight: .infinity, alignment: .top)
        .background(.regularMaterial)
        .environment(\.colorScheme, .dark)
        .gesture(
            DragGesture(minimumDistance: 20).onEnded { value in
                if value.translation.width < -40 { panel = false }
            }
        )
    }

    private var panelContent: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(desktop.name)
                .font(.headline)
                .lineLimit(1)

            VStack(alignment: .leading, spacing: 6) {
                Text("Control").font(.caption).foregroundStyle(.secondary)
                Picker("Control", selection: $mode) {
                    ForEach([ControlMode.cursor, .touch, .view]) { m in
                        Label(m.title, systemImage: m.symbol).tag(m)
                    }
                }
                .pickerStyle(.segmented)
                Text(modeHelp)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Quality").font(.caption).foregroundStyle(.secondary)
                Picker("Quality", selection: $quality) {
                    ForEach(StreamQuality.allCases) { q in
                        Text(q.title).tag(q)
                    }
                }
                .pickerStyle(.segmented)
                Text(quality.help)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if case .live(_, let displays, let display) = session.state, displays > 1 {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Monitor").font(.caption).foregroundStyle(.secondary)
                    Picker("Monitor", selection: Binding(
                        get: { display },
                        set: { session.selectDisplay($0) }
                    )) {
                        ForEach(0..<displays, id: \.self) { i in
                            Text("\(i + 1)").tag(i)
                        }
                    }
                    .pickerStyle(.segmented)
                }
            }

            Toggle(isOn: $keyboard) {
                Label("Keyboard", systemImage: "keyboard")
            }
            .disabled(!session.isLive)

            Toggle(isOn: $keyBar) {
                Label("Key bar", systemImage: "command")
            }

            Button {
                closePanels()
                openCompose()
            } label: {
                Label("Text box", systemImage: "pencil")
            }
            .disabled(!session.isLive)

            VStack(alignment: .leading, spacing: 6) {
                Text("Keys for").font(.caption).foregroundStyle(.secondary)
                Picker("Keys for", selection: $hostKeys) {
                    ForEach(HostKeys.allCases) { k in
                        Text(k.title).tag(k)
                    }
                }
                .pickerStyle(.segmented)
                Text("Labels the modifier keys (\(hostKeys.altLabel), \(hostKeys.metaLabel)) and picks the shortcuts on the extended keys panel.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            DisclosureGroup {
                VStack(alignment: .leading, spacing: 12) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Trackpad speed").font(.caption).foregroundStyle(.secondary)
                        Slider(value: $pointerSpeed, in: 0.5...2.5, step: 0.1) {
                            Text("Trackpad speed")
                        } minimumValueLabel: {
                            Image(systemName: "tortoise")
                        } maximumValueLabel: {
                            Image(systemName: "hare")
                        }
                    }
                    Toggle("Natural scrolling", isOn: $naturalScroll)
                    Toggle("Haptic feedback", isOn: $haptics)
                    Toggle("Fit above keyboard", isOn: $fitAboveKeyboard)
                    Toggle("Show stats", isOn: $showStats)
                    Toggle("Keep screen on", isOn: $keepAwake)
                }
                .padding(.top, 8)
            } label: {
                Label("Settings", systemImage: "gearshape")
            }

            Toggle(isOn: $sound) {
                Label("Computer's sound", systemImage: sound ? "speaker.wave.2" : "speaker.slash")
            }

            Button(role: .destructive) {
                session.releaseAll()
                dismiss()
            } label: {
                Label("Disconnect", systemImage: "xmark.circle")
            }
            .padding(.top, 8)
        }
    }

    private var modeHelp: String {
        switch mode {
        case .touch:
            return "Tap to click. Drag to drag. Press and hold for a right click, or hold then move to drag. Two fingers scroll, or move the picture when zoomed in. Pinch to zoom; double-tap with two fingers to reset. Tap with three fingers for the keyboard."
        case .cursor:
            return "Drag to move the pointer, tap to click, two-finger tap to right click. Press and hold, then move, to drag. Two fingers scroll, or move the picture when zoomed in. Pinch to zoom; double-tap with two fingers to reset. Tap with three fingers for the keyboard."
        case .view:
            return "Pinch and drag move the picture on this phone. Nothing is sent to the computer."
        }
    }

    // MARK: Text box, stats, keyboard room

    /// Jump-style text box: type (with autocorrect and dictation) or dictate
    /// a whole piece of text, check it, then send it to the computer.
    @ViewBuilder
    private var composeBox: some View {
        if composing {
            VStack(spacing: 8) {
                TextField("Type or dictate, then send", text: $composeText)
                    .focused($composeFocused)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { sendCompose(thenReturn: false) }
                HStack {
                    Button("Cancel", role: .cancel) { closeCompose() }
                    Spacer()
                    Button("Send") { sendCompose(thenReturn: false) }
                        .disabled(composeText.isEmpty)
                    Button("Send + Return") { sendCompose(thenReturn: true) }
                        .buttonStyle(.borderedProminent)
                }
            }
            .padding(12)
            .frame(maxWidth: 560)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14))
            .environment(\.colorScheme, .dark)
            .padding(.horizontal, 12)
            .padding(.top, 8)
            .transition(.move(edge: .top).combined(with: .opacity))
        }
    }

    private func openCompose() {
        // The hidden key catcher hands the keyboard over to the text box.
        keyboardBeforeCompose = keyboard
        keyboard = false
        composeText = ""
        withAnimation(.snappy) { composing = true }
        DispatchQueue.main.async { composeFocused = true }
    }

    private func sendCompose(thenReturn: Bool) {
        session.send(text: composeText, thenReturn: thenReturn)
        closeCompose()
    }

    private func closeCompose() {
        composeFocused = false
        withAnimation(.snappy) { composing = false }
        // Back to the key catcher, so typing keeps going to the computer.
        if keyboardBeforeCompose {
            DispatchQueue.main.async { keyboard = true }
        }
    }

    @ViewBuilder
    private var statsBadge: some View {
        if showStats, session.isLive {
            let size = session.videoSize
            let s = session.stats
            Text(String(format: "%.0f fps · %.1f Mbit/s · %d×%d",
                        s?.fps ?? 0, s?.mbps ?? 0, Int(size.width), Int(size.height)))
                .font(.caption2.monospacedDigit())
                .foregroundStyle(.white)
                .padding(.horizontal, 8)
                .padding(.vertical, 3)
                .background(.black.opacity(0.5), in: Capsule())
                .padding(.top, 6)
                .allowsHitTesting(false)
        }
    }

    /// The part of the screen the picture uses.
    private func pictureArea(_ size: CGSize) -> CGSize {
        guard fitAboveKeyboard, keyboardInset > 0, !composing else { return size }
        return CGSize(width: size.width, height: max(size.height - keyboardInset, size.height * 0.3))
    }

    /// How far up the screen the keyboard reaches, from a keyboard frame
    /// notification.
    @MainActor private static func overlap(_ note: Notification) -> CGFloat {
        guard let frame = note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? CGRect,
              let screen = (UIApplication.shared.connectedScenes.first as? UIWindowScene)?.screen
        else { return 0 }
        return max(0, screen.bounds.maxY - frame.minY)
    }

    private func closePanels() {
        panel = false
        keyBar = false
    }

    private var liveScale: CGFloat { clamped(scale * pinch * controlPinch) }

    /// Keep a zoomed picture covering the screen: it can be panned only as
    /// far as its zoomed size goes past the screen's edges.
    private func clampedOffset(_ proposed: CGSize) -> CGSize {
        let l = session.layout
        let shown = l.shownSize
        let maxX = max(0, (shown.width - l.size.width) / 2)
        let maxY = max(0, (shown.height - l.size.height) / 2)
        return CGSize(
            width: min(max(proposed.width, -maxX), maxX),
            height: min(max(proposed.height, -maxY), maxY))
    }

    private func layout(size: CGSize) -> ViewLayout {
        ViewLayout(
            size: size, scale: liveScale,
            offset: CGSize(width: offset.width + drag.width, height: offset.height + drag.height),
            video: session.videoSize)
    }

    @ViewBuilder
    private var overlay: some View {
        switch session.state {
        case .connecting:
            ProgressView(session.waking ? "Waking \(desktop.name)…" : "Connecting…")
                .tint(.white).foregroundStyle(.white)
        case .failed(let message):
            VStack(spacing: 12) {
                Text(message).multilineTextAlignment(.center)
                HStack {
                    Button("Back") { dismiss() }
                    Button("Try again") { session.start(desktop: desktop, client: store.client) }
                    // Offered once the computer has told this phone how to
                    // wake it (it does whenever the phone connects).
                    if store.canWake(desktop) {
                        Button("Wake and connect") {
                            session.start(desktop: desktop, client: store.client, wakeFirst: true)
                        }
                    }
                }
            }
            .padding()
            .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12))
            .foregroundStyle(.white)
        case .closed:
            VStack(spacing: 12) {
                Text("Stopped")
                Button("Back") { dismiss() }
            }
            .foregroundStyle(.white)
        case .live:
            EmptyView()
        }
    }

    private func clamped(_ value: CGFloat) -> CGFloat { min(max(value, 1), 4) }
}

/// The keys a phone keyboard lacks, down the right edge. Modifiers latch
/// for the next key, click or typed character, then let go.
struct KeyBar: View {
    @ObservedObject var session: ViewerSession
    let hostKeys: HostKeys

    var body: some View {
        ScrollView(.vertical, showsIndicators: false) {
            VStack(spacing: 6) {
                key("esc", HIDKey.escape)
                key("tab", HIDKey.tab)
                modifier("ctrl", HIDKey.control)
                modifier(hostKeys.altLabel.lowercased(), HIDKey.alt)
                modifier(hostKeys.metaLabel.lowercased(), HIDKey.meta)
                modifier("shift", HIDKey.shift)
                key("arrow.up", HIDKey.up, symbol: true)
                HStack(spacing: 6) {
                    key("arrow.left", HIDKey.left, symbol: true)
                    key("arrow.right", HIDKey.right, symbol: true)
                }
                key("arrow.down", HIDKey.down, symbol: true)
                key("del", HIDKey.deleteForward)
                key("delete.left", HIDKey.backspace, symbol: true)
                key("return", HIDKey.enter, symbol: true)
                key("home", HIDKey.home)
                key("end", HIDKey.end)
                key("pg up", HIDKey.pageUp)
                key("pg dn", HIDKey.pageDown)
            }
            .padding(8)
        }
        .frame(width: 108)
        .frame(maxHeight: .infinity)
        .background(.regularMaterial)
        .environment(\.colorScheme, .dark)
    }

    private func key(_ label: String, _ usage: UInt16, symbol: Bool = false) -> some View {
        Button {
            session.press(usage)
        } label: {
            Group {
                if symbol { Image(systemName: label) } else { Text(label) }
            }
            .font(.system(.callout, design: .rounded).weight(.medium))
            .frame(maxWidth: .infinity, minHeight: 34)
        }
        .buttonStyle(.bordered)
        .accessibilityLabel(label)
    }

    private func modifier(_ label: String, _ usage: UInt16) -> some View {
        let on = session.latched.contains(usage) || session.locked.contains(usage)
        return Button {
            session.toggleLatch(usage)
        } label: {
            Text(label)
                .font(.system(.callout, design: .rounded).weight(.medium))
                .frame(maxWidth: .infinity, minHeight: 34)
        }
        .buttonStyle(.bordered)
        .tint(on ? .accentColor : nil)
        .background(on ? Color.accentColor.opacity(0.5) : .clear, in: RoundedRectangle(cornerRadius: 8))
        .accessibilityLabel(label)
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

/// How sharp the picture is, traded against network use. On a Windows
/// computer the host also switches the monitor to the closest resolution
/// for the session (and puts it back afterwards).
enum StreamQuality: String, CaseIterable, Identifiable {
    case saver, auto, sharp, full

    static let key = "streamQuality"
    static var saved: StreamQuality {
        StreamQuality(rawValue: UserDefaults.standard.string(forKey: key) ?? "") ?? .auto
    }

    var id: String { rawValue }

    var title: String {
        switch self {
        case .saver: return "Saver"
        case .auto: return "Auto"
        case .sharp: return "Sharp"
        case .full: return "Full"
        }
    }

    var help: String {
        switch self {
        case .saver: return "720p at 4 Mbit/s. Uses the least Wi-Fi."
        case .auto: return "Sized to this phone's screen, 15 Mbit/s."
        case .sharp: return "Twice this phone's screen, 30 Mbit/s. Clearer text when zoomed in."
        case .full: return "The computer's own resolution, 40 Mbit/s. Its screen keeps its usual resolution."
        }
    }

    var bitrateKbps: UInt32 {
        switch self {
        case .saver: return 4_000
        case .auto: return 15_000
        case .sharp: return 30_000
        case .full: return 40_000
        }
    }

    /// Largest picture to ask for, landscape (long side first). Zero means
    /// the computer's own size.
    var maxSize: (width: UInt32, height: UInt32) {
        let native = UIScreen.main.nativeBounds.size
        let long = UInt32(max(native.width, native.height))
        let short = UInt32(min(native.width, native.height))
        switch self {
        case .saver: return (1280, 720)
        case .auto: return (long, short)
        case .sharp: return (long * 2, short * 2)
        case .full: return (0, 0)
        }
    }
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
    /// Two-finger drag while zoomed in pans the picture on the phone.
    var onPan: ((CGPoint) -> Void)?
    var onResetZoom: (() -> Void)?
    /// Three-finger tap shows or hides the keyboard.
    var onToggleKeyboard: (() -> Void)?
    private var scrollRemainder = CGPoint.zero
    private var dragging = false
    private var lastHold = CGPoint.zero
    private var holdStart = CGPoint.zero
    private var holdMoved = false
    private var holding = false

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

    /// True while waiting for a computer woken with Wake-on-LAN to answer.
    @Published private(set) var waking = false

    /// How long a woken computer gets to come back on the network.
    private static let wakeTimeout: TimeInterval = 45

    /// Open the computer's screen. With `wakeFirst`, send it a Wake-on-LAN
    /// packet first and keep trying while it wakes up.
    func start(desktop: Desktop, client: Client?, wakeFirst: Bool = false) {
        guard let client, let view else { return }
        stop()
        state = .connecting
        waking = wakeFirst
        let sink = StreamSink(view: view)
        sink.onState = { [weak self] in self?.state = $0 }
        sink.onStats = { [weak self] in self?.stats = $0 }
        view.needsKeyframe = { [weak self] in
            Task { @MainActor in self?.screen?.requestKeyframe() }
        }
        self.sink = sink
        let q = StreamQuality.saved
        let sound = Self.soundOn
        if sound { Self.prepareAudioSession() }
        let request = ScreenRequest(
            display: 0, fps: 60, bitrateKbps: q.bitrateKbps,
            maxWidth: q.maxSize.width, maxHeight: q.maxSize.height,
            audio: sound)
        Task {
            defer { self.waking = false }
            if wakeFirst {
                do {
                    try client.wake(fingerprint: desktop.fingerprint)
                } catch {
                    self.state = .failed((error as? DaError).map(message) ?? error.localizedDescription)
                    return
                }
            }
            // A Mac takes a few seconds to wake and rejoin the network, so
            // a woken computer that doesn't answer yet is tried again.
            let deadline = Date().addingTimeInterval(wakeFirst ? Self.wakeTimeout : 0)
            while true {
                do {
                    let opened = try await client.openScreen(
                        fingerprint: desktop.fingerprint, request: request, sink: sink)
                    self.screen = opened
                    self.syncPointer()
                    return
                } catch DaError.Unreachable where Date() < deadline {
                    try? await Task.sleep(for: .seconds(3))
                } catch {
                    var text = (error as? DaError).map(message) ?? error.localizedDescription
                    if wakeFirst, let e = error as? DaError, case .Unreachable = e {
                        text += "\n\nIt didn't wake. On the Mac, turn on \"Wake for network access\" "
                            + "in Energy settings and keep it plugged in."
                    }
                    self.state = .failed(text)
                    return
                }
            }
        }
    }

    func stop() {
        screen?.stop()
        screen = nil
        view?.reset()
    }

    /// Frame rate and bit rate over the last second.
    @Published private(set) var stats: StreamStats?

    /// Show another of the computer's monitors.
    func selectDisplay(_ index: Int) {
        screen?.selectDisplay(display: UInt32(index))
    }

    // MARK: Sound

    static let soundKey = "playSound"

    /// The "Computer's sound" setting (on unless turned off).
    static var soundOn: Bool {
        UserDefaults.standard.object(forKey: soundKey) as? Bool ?? true
    }

    /// Turn the computer's sound on or off while the stream runs. The
    /// computer stops recording while it's off.
    func setSound(_ on: Bool) {
        if on { Self.prepareAudioSession() }
        screen?.setAudio(on: on)
    }

    /// Play through the speaker even with the silent switch on, mixed with
    /// any other app's sound.
    static func prepareAudioSession() {
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playback, mode: .default, options: [.mixWithOthers])
        try? session.setActive(true)
    }

    /// Change the picture size and bit rate while the stream runs.
    func apply(_ quality: StreamQuality) {
        let size = quality.maxSize
        screen?.configure(maxWidth: size.width, maxHeight: size.height, bitrateKbps: quality.bitrateKbps)
    }

    // MARK: Keyboard

    /// Modifiers latched on the key bar: held for the next key, click or
    /// typed character, then let go.
    @Published private(set) var latched: [UInt16] = []
    /// Modifiers locked on (press and hold on the key row): held for every
    /// key, click and typed character until tapped again.
    @Published private(set) var locked: [UInt16] = []

    /// A modifier on the key row or key bar. The first tap latches it for
    /// the next key or click (Win, then Up sends Win+Up). Tapping it again
    /// while it is latched presses it on its own, with any other latched
    /// modifiers held: Win twice opens the Start menu.
    func toggleLatch(_ usage: UInt16) {
        if let i = locked.firstIndex(of: usage) {
            // A locked modifier is let go by tapping it.
            locked.remove(at: i)
        } else if let i = latched.firstIndex(of: usage) {
            latched.remove(at: i)
            press(usage)
        } else {
            latched.append(usage)
        }
    }

    /// Lock a modifier on (or let go of a locked one).
    func toggleLock(_ usage: UInt16) {
        latched.removeAll { $0 == usage }
        if let i = locked.firstIndex(of: usage) {
            locked.remove(at: i)
        } else {
            locked.append(usage)
        }
    }

    /// The modifiers to hold for the next key or click: the locked ones, and
    /// the latched ones (which are used up).
    private func takeLatched() -> [UInt16] {
        let mods = locked + latched.filter { !locked.contains($0) }
        if !latched.isEmpty { latched.removeAll() }
        return mods
    }

    /// A ready-made combination from the extended keys panel, with anything
    /// latched or locked held as well.
    func press(_ shortcut: Shortcut) {
        guard let screen else { return }
        var modifiers = takeLatched()
        for m in shortcut.modifiers where !modifiers.contains(m) { modifiers.append(m) }
        for m in modifiers { screen.key(usage: m, down: true) }
        screen.key(usage: shortcut.key, down: true)
        screen.key(usage: shortcut.key, down: false)
        for m in modifiers.reversed() { screen.key(usage: m, down: false) }
    }

    /// Type a whole piece of text from the text box, optionally with Return.
    func send(text: String, thenReturn: Bool) {
        if !text.isEmpty { _ = screen?.typeText(text: text, modifiers: []) }
        if thenReturn { press(HIDKey.enter) }
    }

    func type(_ text: String) {
        _ = screen?.typeText(text: text, modifiers: takeLatched())
    }

    func press(_ usage: UInt16) {
        guard let screen else { return }
        let modifiers = takeLatched()
        for m in modifiers { screen.key(usage: m, down: true) }
        screen.key(usage: usage, down: true)
        screen.key(usage: usage, down: false)
        for m in modifiers.reversed() { screen.key(usage: m, down: false) }
    }

    /// Let go of everything held on the computer: after a drag that was cut
    /// short, on a mode change, or when the app leaves the screen.
    func releaseAll() {
        if dragging || holding {
            dragging = false
            holding = false
        }
        latched.removeAll()
        locked.removeAll()
        screen?.releaseAll()
    }

    // MARK: Pointer helpers

    private var mode: ControlMode {
        ControlMode(rawValue: UserDefaults.standard.string(forKey: "controlMode") ?? "") ?? .cursor
    }

    private func clamp(_ p: CGPoint) -> CGPoint {
        CGPoint(x: min(max(p.x, 0), 1), y: min(max(p.y, 0), 1))
    }

    private func pointAt(_ f: CGPoint) {
        screen?.pointAt(fx: Double(f.x), fy: Double(f.y))
    }

    /// Click, holding any latched modifiers (Ctrl-click, Shift-click).
    private func click(_ button: PointerButton) {
        guard let screen else { return }
        let modifiers = takeLatched()
        for m in modifiers { screen.key(usage: m, down: true) }
        screen.click(button: button, down: true)
        screen.click(button: button, down: false)
        for m in modifiers.reversed() { screen.key(usage: m, down: false) }
    }

    /// Press the left button and keep it held (start of a drag).
    private func pressLeft() {
        screen?.click(button: .left, down: true)
    }

    private func releaseLeft() {
        screen?.click(button: .left, down: false)
    }

    /// Trackpad: move the cursor by a finger movement on screen, a little
    /// faster the faster the finger moves.
    private func moveCursor(by delta: CGPoint) {
        let shown = layout.shownSize
        guard shown.width > 0, shown.height > 0 else { return }
        let speed = hypot(delta.x, delta.y)
        let saved = UserDefaults.standard.double(forKey: ViewerSetting.pointerSpeed)
        let factor = saved == 0 ? 1 : saved
        let gain = (1.2 + min(speed / 12, 2.0)) * factor
        cursor = clamp(CGPoint(
            x: cursor.x + delta.x * gain / shown.width,
            y: cursor.y + delta.y * gain / shown.height))
        pointAt(cursor)
        followCursor()
    }

    /// Zoomed in: when the cursor comes near an edge of the phone's screen,
    /// move the picture so the cursor stays in view.
    private func followCursor() {
        guard layout.scale > 1.01 else { return }
        let p = layout.point(at: cursor)
        let margin: CGFloat = 60
        let size = layout.size
        var pan = CGPoint.zero
        if p.x < margin { pan.x = margin - p.x }
        if p.x > size.width - margin { pan.x = (size.width - margin) - p.x }
        if p.y < margin { pan.y = margin - p.y }
        if p.y > size.height - margin { pan.y = (size.height - margin) - p.y }
        guard pan != .zero else { return }
        onPan?(pan)
    }

    /// Put the computer's pointer where the phone's cursor is, so the two
    /// start out together (on connect and when switching to Trackpad).
    func syncPointer() {
        guard mode == .cursor else { return }
        pointAt(cursor)
    }
}

extension ViewerSession: ControlSurfaceDelegate {
    func surfaceTap(at point: CGPoint, fingers: Int) {
        let button: PointerButton = fingers == 2 ? .right : .left
        if button == .right { haptic() }
        if mode == .touch {
            let f = layout.fraction(at: point)
            guard (0...1).contains(f.x), (0...1).contains(f.y) else { return }
            cursor = f
        }
        pointAt(cursor)
        click(button)
    }

    func surfaceLongPress(_ state: UIGestureRecognizer.State, at point: CGPoint) {
        guard mode != .view else { return }
        // Press and hold presses the left button and keeps it down, so moving
        // afterwards drags (a window by its title bar, a selection...). In
        // Touch mode, letting go without moving is a right click instead.
        switch state {
        case .began:
            holdStart = point
            lastHold = point
            holdMoved = false
            if mode == .touch {
                cursor = clamp(layout.fraction(at: point))
            }
            pointAt(cursor)
            pressLeft()
            holding = true
            haptic()
        case .changed:
            guard holding else { return }
            if hypot(point.x - holdStart.x, point.y - holdStart.y) > 8 { holdMoved = true }
            if mode == .touch {
                cursor = clamp(layout.fraction(at: point))
                pointAt(cursor)
            } else {
                moveCursor(by: CGPoint(x: point.x - lastHold.x, y: point.y - lastHold.y))
            }
            lastHold = point
        default:
            // Ended, cancelled or failed: the button always comes back up.
            guard holding else { return }
            holding = false
            releaseLeft()
            if mode == .touch, !holdMoved {
                haptic()
                click(.right)
            }
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
                pressLeft()
                dragging = true
                cursor = clamp(layout.fraction(at: CGPoint(x: point.x + delta.x, y: point.y + delta.y)))
                pointAt(cursor)
            case .changed:
                cursor = clamp(layout.fraction(at: point))
                pointAt(cursor)
            default:
                if dragging { releaseLeft() }
                dragging = false
            }
        case .cursor:
            if state == .began || state == .changed { moveCursor(by: delta) }
        case .view:
            break
        }
    }

    func surfaceResetZoom() {
        onResetZoom?()
    }

    func surfaceThreeFingerTap() {
        onToggleKeyboard?()
    }

    private func haptic() {
        guard ViewerSetting.bool(ViewerSetting.haptics, default: true) else { return }
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
    }

    func surfaceScroll(delta: CGPoint) {
        // Zoomed in: two fingers move the picture here instead of scrolling
        // the computer.
        if layout.scale > 1.01 {
            onPan?(delta)
            return
        }
        // Natural scrolling: the content follows the fingers. 120 units is
        // one wheel notch; four units per point of finger movement. With
        // natural scrolling off, fingers move the scroll bar instead.
        let k: CGFloat = ViewerSetting.bool(ViewerSetting.naturalScroll, default: true) ? 4 : -4
        let x = -delta.x * k + scrollRemainder.x
        let y = delta.y * k + scrollRemainder.y
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
