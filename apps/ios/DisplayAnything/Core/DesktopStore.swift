import Foundation
import UIKit

/// Owns the Rust client for the app's lifetime and publishes the saved desktops.
@MainActor
final class DesktopStore: ObservableObject {
    @Published private(set) var desktops: [Desktop] = []
    @Published var startError: String?
    /// What this phone is called on the computers it pairs with.
    @Published private(set) var deviceName: String

    private static let nameKey = "deviceName"

    private(set) var client: Client?

    init() {
        // iOS reports a generic "iPhone" as the device name, so the person can
        // set their own; it is kept in UserDefaults.
        let saved = UserDefaults.standard.string(forKey: Self.nameKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let name = (saved?.isEmpty == false ? saved : nil) ?? UIDevice.current.name
        deviceName = name
        do {
            let identity = try PhoneIdentity.loadOrCreate(deviceName: name)
            let dir = try FileManager.default
                .url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
                .appendingPathComponent("DisplayAnything", isDirectory: true)
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
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

    /// Rename this phone. Computers see the new name the next time it connects.
    func rename(_ name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        UserDefaults.standard.set(trimmed, forKey: Self.nameKey)
        deviceName = trimmed
        client?.setDeviceName(name: trimmed)
    }

    /// Whether this desktop told the phone how to wake it. Looked up fresh,
    /// since the phone learns it whenever it connects.
    func canWake(_ desktop: Desktop) -> Bool {
        client?.savedDesktops().first { $0.fingerprint == desktop.fingerprint }?.canWake ?? false
    }

    /// Send a Wake-on-LAN packet. Returns a message for the person.
    func wake(_ desktop: Desktop) -> String {
        guard let client else { return "DisplayAnything didn't start." }
        do {
            try client.wake(fingerprint: desktop.fingerprint)
            return "Sent a wake signal to \(desktop.name). Give it a few seconds, then open it."
        } catch let error as DaError {
            switch error {
            case .NotAllowed(let m), .NotPaired(let m), .Unreachable(let m),
                 .PairingFailed(let m), .Other(let m):
                return m
            }
        } catch {
            return error.localizedDescription
        }
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
