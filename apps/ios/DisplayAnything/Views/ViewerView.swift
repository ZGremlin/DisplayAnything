import SwiftUI

/// One computer's screen. Pinch to zoom, drag to pan (both local, no
/// round trip to the computer).
struct ViewerView: View {
    let desktop: Desktop
    @EnvironmentObject private var store: DesktopStore
    @StateObject private var session = ViewerSession()

    @State private var scale: CGFloat = 1
    @State private var offset: CGSize = .zero
    @GestureState private var pinch: CGFloat = 1
    @GestureState private var drag: CGSize = .zero

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            StreamLayerView { session.attach(view: $0) }
                .scaleEffect(clamped(scale * pinch))
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
                .ignoresSafeArea()

            overlay
        }
        .navigationTitle(desktop.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(.hidden, for: .navigationBar)
        .statusBarHidden(true)
        .task { session.start(desktop: desktop, client: store.client) }
        .onDisappear { session.stop() }
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

/// Runs one stream: opens it, shows its state, and stops it on leave.
@MainActor
final class ViewerSession: ObservableObject {
    @Published private(set) var state: StreamState = .connecting
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

    private func message(_ error: DaError) -> String {
        switch error {
        case .NotAllowed(let m): return m
        case .NotPaired(let m): return m
        case .Unreachable(let m): return m + "\n\nIs the computer on, and on the same network?"
        case .PairingFailed(let m), .Other(let m): return m
        }
    }
}
