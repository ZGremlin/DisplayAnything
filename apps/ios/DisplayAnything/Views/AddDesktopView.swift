import SwiftUI

/// Pairs with a computer: type its IP, then the 6-digit code it shows.
struct AddDesktopView: View {
    @EnvironmentObject private var store: DesktopStore
    @Environment(\.dismiss) private var dismiss

    @State private var address = ""
    @State private var code = ""
    @State private var phase: Phase = .enterAddress
    @State private var message: String?
    @StateObject private var finder = NetworkFinder()

    enum Phase: Equatable { case enterAddress, pairing, enterCode }

    var body: some View {
        Form {
            Section {
                TextField("192.168.1.20", text: $address)
                    .keyboardType(.decimalPad)
                    .textContentType(.none)
                    .autocorrectionDisabled()
                    .disabled(phase != .enterAddress)
            } header: {
                Text("IP address")
            } footer: {
                Text("Use the address shown in the computer's DisplayAnything window. Port 47800 is assumed unless you add one.")
            }

            if phase == .enterAddress {
                findSection
            }

            if phase == .enterCode {
                Section {
                    TextField("6-digit code", text: $code)
                        .keyboardType(.numberPad)
                        .font(.system(.title2, design: .monospaced))
                } header: {
                    Text("Code")
                } footer: {
                    Text("Type the code shown on the computer.")
                }
            }

            if phase == .pairing {
                Section { ProgressView("Connecting…") }
            }

            if let message {
                Section { Text(message).foregroundStyle(.red) }
            }
        }
        .onDisappear { finder.stop() }
        .navigationTitle("Add computer")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") {
                    store.client?.cancelPairing()
                    dismiss()
                }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button(phase == .enterCode ? "Pair" : "Next", action: primaryAction)
                    .disabled(!canContinue)
            }
        }
    }

    @ViewBuilder
    private var findSection: some View {
        Section {
            if finder.state == .idle && finder.computers.isEmpty {
                Button {
                    finder.start()
                } label: {
                    Label("Find on network", systemImage: "antenna.radiowaves.left.and.right")
                }
            } else {
                ForEach(finder.computers) { computer in
                    Button {
                        if let a = computer.address { address = a }
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(computer.name).foregroundStyle(.primary)
                                Text(subtitle(for: computer))
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            if computer.address != nil, address == computer.address {
                                Image(systemName: "checkmark").foregroundStyle(.tint)
                            } else if computer.address == nil, !computer.unreachable {
                                ProgressView()
                            }
                        }
                    }
                    .disabled(computer.address == nil)
                }
                if case .failed(let text) = finder.state {
                    Text(text).foregroundStyle(.red)
                    Button("Try again") { finder.start() }
                } else if finder.computers.isEmpty {
                    HStack(spacing: 10) {
                        ProgressView()
                        Text("Looking for computers running DisplayAnything…")
                            .foregroundStyle(.secondary)
                    }
                }
            }
        } header: {
            Text("On this network")
        } footer: {
            if finder.state == .searching {
                Text("Tap a computer to use its address. You will still type the code it shows.")
            }
        }
    }

    private func subtitle(for computer: NetworkFinder.Computer) -> String {
        let paired = computer.fingerprint.map { fp in
            store.desktops.contains { $0.fingerprint.caseInsensitiveCompare(fp) == .orderedSame }
        } ?? false
        let where_ = computer.address
            ?? (computer.unreachable ? "Couldn't reach it from this phone" : "Finding its address…")
        return paired ? "\(where_) · already paired" : where_
    }

    private var canContinue: Bool {
        switch phase {
        case .enterAddress: return !address.trimmingCharacters(in: .whitespaces).isEmpty
        case .enterCode: return code.filter(\.isNumber).count == 6
        case .pairing: return false
        }
    }

    private func primaryAction() {
        guard let client = store.client else { return }
        switch phase {
        case .enterAddress:
            phase = .pairing
            message = nil
            let target = address.trimmingCharacters(in: .whitespaces)
            Task {
                do {
                    _ = try await client.pair(address: target, listener: CodeWaiter { phase = .enterCode })
                    store.refresh()
                    dismiss()
                } catch {
                    phase = .enterAddress
                    message = (error as? DaError).map(describe) ?? error.localizedDescription
                }
            }
        case .enterCode:
            if !client.submitPairingCode(code: code.filter(\.isNumber)) {
                message = "The computer is no longer waiting for a code. Start again."
                phase = .enterAddress
            }
        case .pairing:
            break
        }
    }

    private func describe(_ error: DaError) -> String {
        switch error {
        case .PairingFailed(let m), .Unreachable(let m), .Other(let m),
             .NotPaired(let m), .NotAllowed(let m):
            return m
        }
    }
}

/// Called from the core when the computer is showing its code. The core
/// calls from a background thread, so the UI update hops to the main actor.
private final class CodeWaiter: PairingListener, @unchecked Sendable {
    private let onNeeded: @MainActor () -> Void
    init(onNeeded: @escaping @MainActor () -> Void) { self.onNeeded = onNeeded }
    func codeNeeded() {
        Task { @MainActor in self.onNeeded() }
    }
}
