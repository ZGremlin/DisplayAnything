import Foundation
import Network

/// Finds computers running DisplayAnything on the local network. They
/// advertise `_displayanything._tcp` over Bonjour (see the core's
/// discovery module). Finding one only fills in its address: trust still
/// comes from pairing with the code it shows.
@MainActor
final class NetworkFinder: ObservableObject {
    struct Computer: Identifiable, Equatable {
        /// The Bonjour service name, which is the computer's name.
        let name: String
        /// Certificate fingerprint from the advertisement, if any.
        let fingerprint: String?
        /// "192.168.1.20:47800" once resolved.
        var address: String?
        /// Its address couldn't be reached from this phone.
        var unreachable = false
        var id: String { name }
    }

    enum State: Equatable {
        case idle
        case searching
        /// The browser failed, with a message for the person.
        case failed(String)
    }

    @Published private(set) var computers: [Computer] = []
    @Published private(set) var state: State = .idle

    private var browser: NWBrowser?
    private var resolving: [String: NWConnection] = [:]

    func start() {
        stop()
        computers = []
        state = .searching
        let browser = NWBrowser(
            for: .bonjourWithTXTRecord(type: "_displayanything._tcp", domain: "local."),
            using: .tcp)
        browser.stateUpdateHandler = { [weak self] state in
            Task { @MainActor in self?.browserState(state) }
        }
        browser.browseResultsChangedHandler = { [weak self] results, _ in
            Task { @MainActor in self?.update(results) }
        }
        browser.start(queue: .main)
        self.browser = browser
    }

    func stop() {
        browser?.cancel()
        browser = nil
        for (_, c) in resolving { c.cancel() }
        resolving.removeAll()
        if state == .searching { state = .idle }
    }

    private func browserState(_ s: NWBrowser.State) {
        switch s {
        case .failed(let error):
            state = .failed(message(for: error))
            browser?.cancel()
            browser = nil
        case .waiting(let error):
            // Usually Local Network access was turned off for the app.
            state = .failed(message(for: error))
        case .ready:
            if case .failed = state { state = .searching }
        default:
            break
        }
    }

    private func message(for error: NWError) -> String {
        if case .dns(let code) = error, code == -65570 /* kDNSServiceErr_PolicyDenied */ {
            return "DisplayAnything isn't allowed to look on the local network. Turn on Local Network for it in Settings > Privacy & Security > Local Network."
        }
        return "Couldn't search the network: \(error.localizedDescription)"
    }

    private func update(_ results: Set<NWBrowser.Result>) {
        var next: [Computer] = []
        for result in results {
            guard case .service(let name, _, _, _) = result.endpoint else { continue }
            var fingerprint: String?
            if case .bonjour(let txt) = result.metadata {
                fingerprint = txt["fp"]
            }
            let known = computers.first { $0.name == name }
            next.append(Computer(
                name: name, fingerprint: fingerprint, address: known?.address,
                unreachable: known?.unreachable ?? false))
            if known?.address == nil, known?.unreachable != true, resolving[name] == nil {
                resolve(name: name, endpoint: result.endpoint)
            }
        }
        computers = next.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    /// Bonjour gives a service name, not an address. Open a short TCP
    /// connection to it (IPv4 only, as the pairing field expects), read the
    /// address it reached, and close it again.
    private func resolve(name: String, endpoint: NWEndpoint) {
        let params = NWParameters.tcp
        if let ip = params.defaultProtocolStack.internetProtocol as? NWProtocolIP.Options {
            ip.version = .v4
        }
        let connection = NWConnection(to: endpoint, using: params)
        resolving[name] = connection
        connection.stateUpdateHandler = { [weak self, weak connection] state in
            guard let connection else { return }
            switch state {
            case .ready:
                var address: String?
                if case .hostPort(let host, let port) = connection.currentPath?.remoteEndpoint {
                    address = "\(Self.text(of: host)):\(port.rawValue)"
                }
                connection.cancel()
                Task { @MainActor in self?.resolved(name: name, address: address) }
            case .failed, .waiting:
                connection.cancel()
                Task { @MainActor in self?.resolved(name: name, address: nil) }
            default:
                break
            }
        }
        connection.start(queue: .main)
        // Give up if it hasn't answered in a few seconds.
        DispatchQueue.main.asyncAfter(deadline: .now() + 5) { [weak self, weak connection] in
            guard let self, let connection, self.resolving[name] === connection else { return }
            connection.cancel()
            self.resolved(name: name, address: nil)
        }
    }

    private func resolved(name: String, address: String?) {
        resolving[name] = nil
        guard let i = computers.firstIndex(where: { $0.name == name }) else { return }
        if let address {
            computers[i].address = address
        } else {
            computers[i].unreachable = true
        }
    }

    nonisolated private static func text(of host: NWEndpoint.Host) -> String {
        switch host {
        case .ipv4(let a):
            return "\(a)".components(separatedBy: "%").first ?? "\(a)"
        case .ipv6(let a):
            return "[\(a)]"
        case .name(let n, _):
            return n
        @unknown default:
            return "\(host)"
        }
    }
}
