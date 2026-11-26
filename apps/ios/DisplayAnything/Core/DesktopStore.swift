import Foundation
import UIKit

/// Owns the Rust client for the app's lifetime and publishes the saved desktops.
@MainActor
final class DesktopStore: ObservableObject {
    @Published private(set) var desktops: [Desktop] = []
    @Published var startError: String?

    private(set) var client: Client?

    init() {
        do {
            let name = UIDevice.current.name
            let identity = try PhoneIdentity.loadOrCreate(deviceName: name)
            let dir = try FileManager.default
                .url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
                .appendingPathComponent("DisplayAnything", isDirectory: true)
            let client = try Client(dataDir: dir.path, deviceName: name, identity: identity)
            self.client = client
            refresh()
        } catch {
            startError = error.localizedDescription
        }
    }

    func refresh() {
        desktops = client?.savedDesktops() ?? []
    }

    func forget(_ desktop: Desktop) {
        do {
            try client?.forget(fingerprint: desktop.fingerprint)
        } catch {
            startError = error.localizedDescription
        }
        refresh()
    }
}
