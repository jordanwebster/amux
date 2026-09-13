import Foundation
import Network
import dnssd

/// One machine this phone's browser has resolved on the local network.
///
/// The field names are the bridge's own, because this is handed straight to it
/// rather than translated on the way.
public struct FoundHost: Equatable, Sendable, Codable {
    public let host: HostId
    public let name: String
    /// The protocol version the advertisement's own record claims.
    public let version: UInt32
    /// Where this machine can be dialled, as `address:port`.
    public let addrs: [String]

    public init(host: HostId, name: String, version: UInt32, addrs: [String]) {
        self.host = host
        self.name = name
        self.version = version
        self.addrs = addrs
    }
}

/// Whether the system is letting this app look at the network it is on.
///
/// A person who said no is not a person with no machines nearby, and the two
/// have to be told apart: one is explained and can be undone in Settings, the
/// other is a network with nothing on it.
public enum LocalNetworkPermission: Equatable, Sendable {
    /// Nothing has been asked yet, or the browser has not answered.
    case unknown
    case granted
    case denied
}

/// The one place in this app that looks at the local network.
///
/// On iOS only the system may browse, so the shared Rust library does not: it
/// is handed what this saw and treats it as the only machines this device can
/// see. The whole set goes over each time — a machine that has gone is a
/// machine missing from the set, not an event of its own.
///
/// Browsing runs only while somebody is looking at the phone. A browser left
/// running behind a locked screen keeps the radio awake to learn about
/// machines nobody is about to talk to.
@MainActor
public final class LocalDiscovery {
    /// What amux advertises itself as. The transport is QUIC, hence `_udp`.
    public nonisolated static let service = "_amux._udp"

    public typealias HandOver = @MainActor ([FoundHost]) -> Void

    /// What the system has said about looking at this network.
    public private(set) var permission: LocalNetworkPermission = .unknown {
        didSet {
            guard permission != oldValue else { return }
            permissionChanged?(permission)
        }
    }

    /// Told when the answer changes, so a screen can explain a refusal.
    public var permissionChanged: (@MainActor (LocalNetworkPermission) -> Void)?

    private let handOver: HandOver
    private var browser: NWBrowser?
    /// One in-flight address resolution per advertisement.
    private var resolving: [NWEndpoint: NWConnection] = [:]
    /// What each advertisement claimed about itself, before its address is known.
    private var claims: [NWEndpoint: (host: HostId, name: String, version: UInt32)] = [:]
    /// The advertisements whose addresses are known, which is what is handed over.
    private var found: [NWEndpoint: FoundHost] = [:]

    public init(handOver: @escaping HandOver) {
        self.handOver = handOver
    }

    /// Begins browsing. Doing this twice is doing it once.
    public func start() {
        guard browser == nil else { return }
        let parameters = NWParameters()
        parameters.includePeerToPeer = false
        let browser = NWBrowser(
            for: .bonjourWithTXTRecord(type: Self.service, domain: nil), using: parameters)
        browser.stateUpdateHandler = { state in
            MainActor.assumeIsolated { self.permission = Self.permission(for: state) }
        }
        browser.browseResultsChangedHandler = { results, _ in
            MainActor.assumeIsolated { self.browsed(results) }
        }
        self.browser = browser
        browser.start(queue: .main)
    }

    /// Stops browsing, forgetting what was seen without withdrawing it.
    ///
    /// Nothing is handed over here. The library is told separately that the
    /// app went away and closes its direct links itself; the machines it was
    /// told about are the ones it dials again the moment the app comes back,
    /// rather than an empty network until the browser has found them twice.
    /// A machine that really did leave meanwhile is missing from the first set
    /// the restarted browser hands over.
    public func stop() {
        guard browser != nil else { return }
        browser?.cancel()
        browser = nil
        for connection in resolving.values { connection.cancel() }
        resolving = [:]
        claims = [:]
        found = [:]
        permission = .unknown
    }

    /// The set the browser can currently see, resolving the ones that are new.
    private func browsed(_ results: Set<NWBrowser.Result>) {
        let endpoints = Set(results.map(\.endpoint))
        for endpoint in Array(claims.keys) where !endpoints.contains(endpoint) {
            resolving.removeValue(forKey: endpoint)?.cancel()
            claims.removeValue(forKey: endpoint)
            found.removeValue(forKey: endpoint)
        }
        for result in results {
            guard claims[result.endpoint] == nil,
                  case .bonjour(let record) = result.metadata,
                  let claimed = Self.claim(from: record),
                  let name = Self.serviceName(of: result.endpoint)
            else { continue }
            claims[result.endpoint] = (claimed.host, name, claimed.version)
            resolve(result.endpoint)
        }
        handOver(Array(found.values))
    }

    /// Turns an advertisement into an address by opening the connection the
    /// system resolves it for, reading the address it settled on and dropping
    /// it again. Bonjour gives a name and a port; only a connection gives the
    /// address behind them, and nothing here wants the connection itself.
    private func resolve(_ endpoint: NWEndpoint) {
        let connection = NWConnection(to: endpoint, using: .udp)
        resolving[endpoint] = connection
        connection.stateUpdateHandler = { state in
            MainActor.assumeIsolated {
                switch state {
                case .ready:
                    self.resolved(endpoint, at: connection.currentPath?.remoteEndpoint)
                case .failed, .cancelled:
                    self.resolving.removeValue(forKey: endpoint)
                default: break
                }
            }
        }
        connection.start(queue: .main)
    }

    private func resolved(_ endpoint: NWEndpoint, at remote: NWEndpoint?) {
        resolving.removeValue(forKey: endpoint)?.cancel()
        guard let claim = claims[endpoint], let remote,
              let address = Self.address(of: remote)
        else { return }
        found[endpoint] = FoundHost(
            host: claim.host, name: claim.name, version: claim.version, addrs: [address])
        handOver(Array(found.values))
    }

    /// What an advertisement's own record claims it is.
    ///
    /// A record missing either field is not an amux host advertising itself —
    /// some other service on the same name, or a version that predates them —
    /// and is passed over rather than guessed at.
    nonisolated static func claim(from record: NWTXTRecord) -> (host: HostId, version: UInt32)? {
        guard let hid = record["hid"], let host = HostId(hid),
              let claimed = record["v"], let version = UInt32(claimed)
        else { return nil }
        return (host, version)
    }

    /// The instance name a Bonjour endpoint carries, which is the machine's own.
    nonisolated static func serviceName(of endpoint: NWEndpoint) -> String? {
        guard case .service(let name, _, _, _) = endpoint else { return nil }
        return name
    }

    /// A resolved address as the shared library spells one, or nothing where
    /// it could not dial what was resolved.
    ///
    /// A link-local IPv6 address is dropped: it is only meaningful with the
    /// interface it was learned on, and the address the library parses has
    /// nowhere to carry one. A machine reachable at all is also reachable at
    /// an address that travels.
    nonisolated static func address(of endpoint: NWEndpoint) -> String? {
        guard case .hostPort(let host, let port) = endpoint else { return nil }
        switch host {
        case .ipv4(let address):
            return "\(unscoped(address)):\(port.rawValue)"
        case .ipv6(let address):
            guard !address.isLinkLocal else { return nil }
            return "[\(unscoped(address))]:\(port.rawValue)"
        case .name:
            return nil
        @unknown default:
            return nil
        }
    }

    /// An address without the `%en0` the system appends when it knows which
    /// interface the address was learned on.
    private nonisolated static func unscoped(_ address: any IPAddress) -> String {
        String("\(address)".prefix { $0 != "%" })
    }

    /// What a browser's state says about being allowed to look at this network.
    ///
    /// A refusal is not an error the browser reports once and recovers from:
    /// it waits, holding the one DNS code that means the person said no. Every
    /// other wait is an ordinary network that is not ready yet.
    nonisolated static func permission(for state: NWBrowser.State) -> LocalNetworkPermission {
        switch state {
        case .ready: return .granted
        case .waiting(.dns(DNSServiceErrorType(kDNSServiceErr_PolicyDenied))): return .denied
        case .failed(.dns(DNSServiceErrorType(kDNSServiceErr_PolicyDenied))): return .denied
        default: return .unknown
        }
    }
}
