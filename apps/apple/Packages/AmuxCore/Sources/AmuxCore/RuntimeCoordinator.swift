import Foundation

/// The runtime boundary used by the application and by tests that script its replies.
@MainActor
public protocol AppRuntime: AnyObject {
    var events: AsyncStream<[Event]> { get }
    @discardableResult func dispatch(_ command: BridgeCommand) -> OpId?
    @discardableResult func attach(_ picked: PickedAttachment, bytes: Data) -> OpId?
    func discovered(_ hosts: [FoundHost])
    func setActive(_ active: Bool)
    func stop()
}

extension BridgeClient: AppRuntime {}

/// Owns the selected account's connection, independently of any screen's lifetime.
@MainActor
public final class RuntimeCoordinator {
    public typealias TokenProvider = @Sendable (UInt64, String) async -> ConnectToken?
    public typealias Factory = @MainActor (BridgeConfiguration, @escaping TokenProvider) throws -> any AppRuntime

    public private(set) var runtime: (any AppRuntime)?
    public private(set) var runtimeAccount: AccountId?
    public private(set) var lastBatch: [AccountId: [Event]] = [:]
    /// Diagnostic detail for the driving door and reports, never screen copy.
    public private(set) var failure: String?
    public let deviceName: String
    /// The browser whose findings this hands on, where the app gave it one.
    /// It runs only while somebody is looking at the phone, so it is started
    /// and stopped with the scene rather than with the connection.
    public var discovery: LocalDiscovery? {
        didSet {
            discovery?.permissionChanged = { [weak self] permission in
                self?.localNetwork(permission)
            }
        }
    }
    /// The stores a phone with nobody signed in draws from.
    ///
    /// A phone without an account still finds the machines on its own network,
    /// pairs with them and reaches them, so it still has a connection and
    /// still has somewhere to put what that connection says. Held apart from
    /// the registry's, which belong to accounts.
    public var signedOutStores: StoreBundle? {
        didSet { signedOutStores?.hosts.sawLocalNetwork(permission) }
    }
    public var storesChanged: (@MainActor (StoreBundle) -> Void)?
    public var unsubscribed: (@MainActor (AgentId) -> Void)?

    private let registry: AccountRegistry
    private let cloud: any CloudService
    private let support: URL
    private let cache: URL
    private let allowPlainLoopback: Bool
    private let factory: Factory
    private var configured: BridgeConfiguration?
    private var initialized = false
    private var invariant: String?
    private var wired: StoreBundle?
    private var pump: Task<Void, Never>?
    private var starting: Task<Void, Never>?
    private var generation = 0
    private var active = true
    /// What the browser last saw, kept so a runtime started afterwards — a
    /// sign-in, a switch, a retry — is told without waiting for the browser to
    /// notice the same machines a second time.
    private var found: [FoundHost] = []
    /// What the system last said about letting this app look at the network.
    private var permission: LocalNetworkPermission = .unknown
    private var overrideRelay: URL?
    private var overrideTokens: [String: String] = [:]
    /// What a driver said those credentials buy. The account service says
    /// it in the same reply that issues a real one, and a link that
    /// reported nothing would report free.
    private var overrideTier: Tier?

    public init(
        registry: AccountRegistry, cloud: any CloudService, support: URL, cache: URL,
        deviceName: String, allowPlainLoopback: Bool = false,
        factory: @escaping Factory = { try BridgeClient(configuration: $0, tokenProvider: $1) }
    ) {
        self.registry = registry
        self.cloud = cloud
        self.support = support
        self.cache = cache
        self.deviceName = deviceName
        self.allowPlainLoopback = allowPlainLoopback
        self.factory = factory
        registry.changed = { [weak self] in self?.accountsChanged() }
    }

    deinit {
        starting?.cancel()
        pump?.cancel()
    }

    /// Runs after the synchronous cache read, so the first frame needs no network.
    public func start() {
        accountsChanged()
    }

    private func accountsChanged() {
        generation += 1
        starting?.cancel()
        let replaced = wired !== registry.stores
        unwire()
        if let configured, configured.accounts.map(\.id) != listed.map(\.value) {
            stopRuntime()
        }
        if replaced, let stores = registry.stores {
            stores.apply(Bridge.cachedFleet(in: cache, for: stores.account))
            storesChanged?(stores)
        }
        let expected = generation
        starting = Task { [weak self] in
            _ = await self?.connect(generation: expected)
        }
    }

    /// Imports a refresh session through the service, which remains the authority
    /// for the account facts, entitlement and relay address.
    public func restoreSession(account: AccountId, refresh: String) async throws -> Bool {
        guard let service = cloud as? AmuxCloudService else { throw CloudError.unauthenticated }
        try await service.restore(account, refresh: refresh)
        let facts = try await service.account(account)
        guard facts.id == account else { throw CloudError.unauthenticated }
        registry.add(SignedInAccount(id: facts.id, email: facts.email, displayName: facts.displayName),
                     entitlement: facts.entitlement)
        registry.select(account)
        overrideRelay = nil
        overrideTokens = [:]
        overrideTier = nil
        return await reconnect()
    }

    /// Launch-time credentials supplied by a debug driver use the same installation,
    /// profiles, event pump and store wiring as credentials issued by the service.
    ///
    /// - Parameter tier: what those credentials buy, which the relay will
    ///   enforce from the token's own claim and every screen reads off the
    ///   link. A driver says it because nothing here can read it out of an
    ///   opaque bearer.
    public func override(relay: URL, tokens: [String: String], tier: Tier? = nil) async -> Bool {
        overrideRelay = relay
        overrideTokens = tokens
        overrideTier = tier
        return await reconnect()
    }

    @discardableResult
    public func reconnect() async -> Bool {
        generation += 1
        starting?.cancel()
        return await connect(generation: generation)
    }

    private func connect(generation expected: Int) async -> Bool {
        guard expected == generation, !Task.isCancelled else { return false }
        // A phone nobody has signed in on still runs. It reaches no relay and
        // answers for no account, and everything it finds on its own network
        // it finds and dials itself.
        let account = registry.selectedAccount?.signedIn == true ? registry.selected : nil
        do {
            var relay: URL?
            if let account {
                if let overrideRelay {
                    relay = overrideRelay
                } else {
                    let credential = try await cloud.connectToken(account)
                    guard let address = credential.relay else {
                        throw CloudError.refused("The relay has no address")
                    }
                    relay = address
                }
            }
            guard expected == generation, registry.selected == account || account == nil,
                  (registry.selectedAccount?.signedIn == true) == (account != nil) else { return false }
            let configuration = try configuration(relay: relay, account: account)
            if let current = configured, let runtime,
               current.relay == configuration.relay, current.accounts == configuration.accounts {
                if let account, current.active != account.value {
                    runtime.dispatch(.selectAccount(account.value))
                    configured?.active = account.value
                }
                if runtimeAccount == account { wireSelected(to: runtime) }
                failure = nil
                return true
            }
            stopRuntime()
            try FileManager.default.createDirectory(at: support, withIntermediateDirectories: true)
            try FileManager.default.createDirectory(at: cache, withIntermediateDirectories: true)
            let service = cloud
            let client = try factory(configuration, { _, id in
                try? await service.connectToken(AccountId(id))
            })
            runtime = client
            configured = configuration
            runtimeAccount = account
            client.setActive(active)
            if !found.isEmpty { client.discovered(found) }
            wireSelected(to: client)
            pump = Task { [weak self] in
                for await batch in client.events {
                    guard !Task.isCancelled else { return }
                    self?.receive(batch)
                }
            }
            failure = nil
            return true
        } catch {
            guard expected == generation else { return false }
            fail(detail: String(describing: error), reason: .unreachable)
            return false
        }
    }

    private func fail(detail: String, reason: OfflineReason) {
        stopRuntime()
        failure = detail
        guard let stores = visible else { return }
        stores.apply(.connection(.init(state: .disconnected, reason: reason)))
        wired = stores
        stores.dispatch = { [weak self, weak stores] command in
            guard let self, let stores, self.visible === stores,
                  command == .retryNow else { return nil }
            self.start()
            return OpId(UUID().uuidString)
        }
    }

    /// The stores the app is drawing: the selected account's, or — with nobody
    /// signed in — the ones a phone without an account draws from.
    private var visible: StoreBundle? {
        registry.selectedAccount?.signedIn == true ? registry.stores : signedOutStores
    }

    /// The accounts a configuration may list.
    ///
    /// Every account this phone is signed in to while one of them is on
    /// screen, so the switcher can say that an account nobody is looking at
    /// has something waiting. None at all with nobody on screen, even when
    /// another account is still signed in: an account is reached through the
    /// relay, the relay address comes with the on-screen account's credential,
    /// and a connection that listed an account it has no route for is refused
    /// outright — which would leave a phone whose other account happens to be
    /// signed in with no connection at all, and so no way to find or reach the
    /// machines on its own network.
    private var listed: [AccountId] {
        guard registry.selectedAccount?.signedIn == true else { return [] }
        return registry.accounts.filter(\.signedIn).map(\.id)
    }

    private func configuration(relay: URL?, account: AccountId?) throws -> BridgeConfiguration {
        var endpoint: BridgeConfiguration.Relay?
        if let relay {
            guard let host = relay.host, relay.port != nil else { throw BridgeError.didNotStart }
            let octets = host.split(separator: ".", omittingEmptySubsequences: false)
            let loopback = host == "localhost" || host == "::1" || host == "[::1]"
                || octets.count == 4 && octets.first == "127" && octets.allSatisfy { UInt8($0) != nil }
            let plain = allowPlainLoopback && loopback
            guard plain || relay.scheme == "https" else { throw BridgeError.didNotStart }
            var address = URLComponents(url: relay, resolvingAgainstBaseURL: false)!
            if plain {
                address.scheme = "http"
                if host == "localhost" { address.host = "127.0.0.1" }
            }
            endpoint = .init(url: address.url!.absoluteString, tls: plain ? .plainLoopback : .system)
        }
        return BridgeConfiguration(
            dataDirectory: support, cacheDirectory: cache, deviceName: deviceName,
            relay: endpoint,
            accounts: listed.map { id in
                .init(id: id.value,
                      token: overrideTokens[id.value]
                          .map { .fixed($0, tier: overrideTier) } ?? .callback)
            },
            // With nobody signed in, the account last on screen: its profile
            // and the machines it paired with stay where they were rather than
            // the app emptying itself the moment somebody signs out.
            active: (account ?? registry.selected)?.value,
            logPath: support.appendingPathComponent("runtime.log"))
    }

    private func wireSelected(to client: any AppRuntime) {
        guard let stores = visible else { return }
        unwire()
        wired = stores
        stores.watch = { [weak client] in client?.dispatch(.subscribe(agent: $0)) }
        stores.unwatch = { [weak client, weak self] agent in
            self?.unsubscribed?(agent)
            client?.dispatch(.unsubscribe(agent: agent))
        }
        stores.dispatch = { [weak client, weak self, weak stores] command in
            guard let self, let stores, self.visible === stores else { return nil }
            return client?.dispatch(command)
        }
        stores.store = { [weak client, weak self, weak stores] picked, bytes in
            guard let self, let stores, self.visible === stores else { return nil }
            return client?.attach(picked, bytes: bytes)
        }
        stores.hosts.sawLocalNetwork(permission)
        for agent in stores.streamingAgents { client.dispatch(.subscribe(agent: agent)) }
        storesChanged?(stores)
    }

    private func unwire() {
        wired?.watch = nil
        wired?.unwatch = nil
        wired?.dispatch = nil
        wired?.store = nil
        wired = nil
    }

    private func receive(_ batch: [Event]) {
        // A runtime with nobody signed in answers for no account. What it says
        // goes to the stores a signed-out phone draws from, and the registry —
        // whose whole job is keeping one account's answers off another
        // account's screen — has nothing to decide about it.
        var answering = runtimeAccount
        var pending: [Event] = []
        var failed = false
        func deliver() {
            guard !pending.isEmpty else { return }
            if let answering {
                lastBatch[answering] = pending
                registry.deliver(pending, for: answering)
            } else {
                signedOutStores?.apply(pending)
            }
            pending = []
        }
        for event in batch {
            switch event {
            case .invariant(let detail):
                invariant = detail
                if !initialized { failed = true }
            case .connection(let update):
                // A runtime with no relay says it is stopped because it is not
                // on one, which is the truth about a phone nobody has signed
                // in on rather than a worker that died. Only a runtime that
                // was given a relay can report having stopped reaching it.
                if update.state == .disconnected && update.reason == .stopped,
                   configured?.relay != nil {
                    failed = true
                } else {
                    initialized = true
                }
            default: break
            }
            if case .opResult(let result) = event,
               case .selected(let account) = result.outcome {
                // A coalesced batch can straddle a switch. Its prefix still belongs
                // to the previous account; only the suffix belongs to the new one.
                deliver()
                answering = AccountId(account)
                runtimeAccount = answering
                if answering == registry.selected, let runtime { wireSelected(to: runtime) }
            }
            pending.append(event)
        }
        deliver()
        if failed {
            // The C handle exists before installation startup finishes. A dead
            // worker cannot service Retry Now; release it and retry from credentials.
            fail(detail: invariant ?? "The mobile runtime stopped", reason: .stopped)
        }
    }

    /// Every machine the phone's browser can currently see.
    ///
    /// Remembered as well as passed on, because the connection and the browser
    /// have separate lives: the runtime is replaced on a sign-in or a switch,
    /// and the machines on this network did not go anywhere when it was.
    public func discovered(_ hosts: [FoundHost]) {
        found = hosts
        runtime?.discovered(hosts)
    }

    /// What the system has said about letting this app look at the network
    /// this phone is on.
    ///
    /// Kept here rather than on the browser, because a screen that explains a
    /// refusal outlives every browser this phone starts: browsing stops when
    /// the app is put away and the refusal does not go away with it.
    public func localNetwork(_ permission: LocalNetworkPermission) {
        // Stopping the browser forgets what it was told; a refusal already
        // heard is not withdrawn by that, and re-announcing it as unknown
        // would take the explanation off a screen nobody had fixed anything
        // on.
        guard permission != .unknown else { return }
        self.permission = permission
        visible?.hosts.sawLocalNetwork(permission)
    }

    public func setActive(_ active: Bool) {
        self.active = active
        runtime?.setActive(active)
        // Browsing belongs to the foreground. Put away, the browser stops; the
        // machines it found are kept, so coming back dials them at once
        // instead of showing an empty network until the browser catches up.
        if active { discovery?.start() } else { discovery?.stop() }
        if active && runtime == nil { start() }
    }

    public func stop() {
        generation += 1
        discovery?.stop()
        starting?.cancel()
        starting = nil
        stopRuntime()
    }

    private func stopRuntime() {
        unwire()
        pump?.cancel()
        pump = nil
        runtime?.stop()
        runtime = nil
        runtimeAccount = nil
        configured = nil
        initialized = false
        invariant = nil
    }
}
