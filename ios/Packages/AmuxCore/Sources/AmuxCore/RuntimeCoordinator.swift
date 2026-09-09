import Foundation

/// The runtime boundary used by the application and by tests that script its replies.
@MainActor
public protocol AppRuntime: AnyObject {
    var events: AsyncStream<[Event]> { get }
    @discardableResult func dispatch(_ command: BridgeCommand) -> OpId?
    @discardableResult func attach(_ picked: PickedAttachment, bytes: Data) -> OpId?
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
    private var overrideRelay: URL?
    private var overrideTokens: [String: String] = [:]

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
        if let configured,
           configured.accounts.map(\.id) != registry.accounts.filter(\.signedIn).map({ $0.id.value }) {
            stopRuntime()
        }
        if replaced, let stores = registry.stores {
            stores.apply(Bridge.cachedFleet(in: cache, for: stores.account))
            storesChanged?(stores)
        }
        guard registry.selectedAccount?.signedIn == true else {
            stopRuntime()
            return
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
        return await reconnect()
    }

    /// Launch-time credentials supplied by a debug driver use the same installation,
    /// profiles, event pump and store wiring as credentials issued by the service.
    public func override(relay: URL, tokens: [String: String]) async -> Bool {
        overrideRelay = relay
        overrideTokens = tokens
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
        guard let account = registry.selected, registry.selectedAccount?.signedIn == true else {
            stopRuntime()
            return false
        }
        do {
            let relay: URL
            if let overrideRelay {
                relay = overrideRelay
            } else {
                let credential = try await cloud.connectToken(account)
                guard let address = credential.relay else { throw CloudError.refused("The relay has no address") }
                relay = address
            }
            guard expected == generation, registry.selected == account,
                  registry.selectedAccount?.signedIn == true else { return false }
            let configuration = try configuration(relay: relay, account: account)
            if let current = configured, let runtime,
               current.relay == configuration.relay, current.accounts == configuration.accounts {
                if current.active != account.value {
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
        guard let stores = registry.stores else { return }
        stores.apply(.connection(.init(state: .disconnected, reason: reason)))
        wired = stores
        stores.dispatch = { [weak self, weak stores] command in
            guard let self, let stores, self.registry.stores === stores,
                  command == .retryNow else { return nil }
            self.start()
            return OpId(UUID().uuidString)
        }
    }

    private func configuration(relay: URL, account: AccountId) throws -> BridgeConfiguration {
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
        return BridgeConfiguration(
            dataDirectory: support, cacheDirectory: cache, deviceName: deviceName,
            relay: .init(url: address.url!.absoluteString, tls: plain ? .plainLoopback : .system),
            accounts: registry.accounts.filter(\.signedIn).map {
                .init(id: $0.id.value, token: overrideTokens[$0.id.value].map { .fixed($0) } ?? .callback)
            }, active: account.value, logPath: support.appendingPathComponent("runtime.log"))
    }

    private func wireSelected(to client: any AppRuntime) {
        guard let stores = registry.stores else { return }
        unwire()
        wired = stores
        stores.watch = { [weak client] in client?.dispatch(.subscribe(agent: $0)) }
        stores.unwatch = { [weak client, weak self] agent in
            self?.unsubscribed?(agent)
            client?.dispatch(.unsubscribe(agent: agent))
        }
        stores.dispatch = { [weak client, weak self, weak stores] command in
            guard let self, let stores, self.registry.stores === stores else { return nil }
            return client?.dispatch(command)
        }
        stores.store = { [weak client, weak self, weak stores] picked, bytes in
            guard let self, let stores, self.registry.stores === stores else { return nil }
            return client?.attach(picked, bytes: bytes)
        }
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
        guard var answering = runtimeAccount else { return }
        var pending: [Event] = []
        var failed = false
        func deliver() {
            guard !pending.isEmpty else { return }
            lastBatch[answering] = pending
            registry.deliver(pending, for: answering)
            pending = []
        }
        for event in batch {
            switch event {
            case .invariant(let detail):
                invariant = detail
                if !initialized { failed = true }
            case .connection(let update):
                if update.state == .disconnected && update.reason == .stopped {
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

    public func setActive(_ active: Bool) {
        self.active = active
        runtime?.setActive(active)
        if active && runtime == nil { start() }
    }

    public func stop() {
        generation += 1
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
