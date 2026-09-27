import AmuxValues
import Foundation
import Observation

/// Runs the installation of the account on screen and keeps its stores fed.
///
/// One runtime per process. Switching account stops the one running and
/// starts the other's; removing an account deletes its installation once
/// nothing runs from it. Starting reads this phone's own store before any
/// network is dialled, so the first frame after a launch is what the phone
/// last held.
@MainActor
@Observable
public final class RuntimeCoordinator {
    /// How a test or a driving build changes where and how the runtime runs.
    public struct Options: Sendable {
        /// Advertisers with another scope are never listed; real machines
        /// advertise none.
        public var discoveryScope: String
        /// Where direct links listen instead of every interface, as
        /// `ip:port`. Only a driving build's library reads it.
        public var lanBind: String?

        public init(discoveryScope: String = "", lanBind: String? = nil) {
            self.discoveryScope = discoveryScope
            self.lanBind = lanBind
        }
    }

    public typealias Starter = @Sendable (StartConfig, @escaping @Sendable (UInt64) -> Void)
        throws(RuntimeFailure) -> Runtime

    public private(set) var runtime: Runtime?
    /// The installation the running runtime serves.
    public private(set) var running: String?
    /// Why the runtime is not running, when it failed to start.
    public private(set) var failure: String?
    /// A failure to open the store stops the whole app: nothing it holds is
    /// drawable, and a page drawn from nothing would lie.
    public private(set) var storeFailure: String? {
        didSet { if storeFailure != oldValue { storeFailureChanged?(storeFailure) } }
    }
    @ObservationIgnored public var storeFailureChanged: (@MainActor (String?) -> Void)?
    public let deviceName: String

    /// The phone's own browser; only the system may look at the network.
    @ObservationIgnored public var discovery: LocalDiscovery? {
        didSet {
            discovery?.permissionChanged = { [weak self] permission in
                self?.permission = permission
                self?.stores.hosts.sawLocalNetwork(permission)
            }
        }
    }

    /// What a phone nobody has signed in on draws.
    @ObservationIgnored public var signedOutStores: StoreBundle

    @ObservationIgnored private let registry: AccountRegistry
    @ObservationIgnored private let support: URL
    @ObservationIgnored private let options: Options
    @ObservationIgnored private let starter: Starter
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var active = true
    @ObservationIgnored private var found: [FoundHost] = []
    @ObservationIgnored private var permission: LocalNetworkPermission = .unknown
    @ObservationIgnored private var fed: StoreBundle?
    @ObservationIgnored private var waiting: [CheckedContinuation<Runtime?, Never>] = []
    /// The start under way, which the next one waits for: two runtimes may
    /// not hold one installation, and a start that lost its place stops the
    /// runtime it started before the next begins.
    @ObservationIgnored private var starting: Task<Void, Never>?
    @ObservationIgnored private var inFlight = false

    public init(
        registry: AccountRegistry, support: URL, deviceName: String,
        signedOut: StoreBundle, options: Options = Options(),
        starter: @escaping Starter = { config, wake throws(RuntimeFailure) in
            try Runtime.start(config, wake: wake)
        }
    ) {
        self.registry = registry
        self.support = support
        self.deviceName = deviceName
        self.signedOutStores = signedOut
        self.options = options
        self.starter = starter
        registry.switching = { [weak self] _ in self?.restart() }
    }

    /// The stores on screen.
    public var stores: StoreBundle { registry.stores ?? signedOutStores }

    /// Where an installation lives.
    public func directory(of installation: String) -> URL {
        support.appendingPathComponent("installations", isDirectory: true)
            .appendingPathComponent(installation, isDirectory: true)
    }

    public func start() {
        guard runtime == nil, !inFlight else { return }
        restart()
    }

    /// Stops what runs and starts the installation on screen.
    public func restart() {
        stop()
        generation += 1
        let expected = generation
        let installation = registry.installation
        let stores = stores
        let directory = directory(of: installation)
        let config = StartConfig(
            dataDir: directory.path, deviceName: deviceName,
            discoveryScope: options.discoveryScope, lan: true, lanBind: options.lanBind,
            logPath: directory.appendingPathComponent("runtime.log").path, tail: nil)
        let starter = starter
        let previous = starting
        inFlight = true
        starting = Task.detached {
            await previous?.value
            let started: Result<Runtime, RuntimeFailure>
            do {
                try FileManager.default.createDirectory(
                    at: directory, withIntermediateDirectories: true)
                started = .success(try starter(config) { chat in
                    Task { @MainActor [weak self] in
                        guard self?.generation == expected else { return }
                        stores.woke(chat)
                    }
                })
            } catch let failure as RuntimeFailure {
                started = .failure(failure)
            } catch {
                started = .failure(RuntimeFailure(error.localizedDescription))
            }
            await self.started(started, generation: expected, installation: installation,
                               stores: stores)
        }
    }

    private func started(
        _ result: Result<Runtime, RuntimeFailure>, generation expected: Int,
        installation: String, stores: StoreBundle
    ) {
        if expected == generation { inFlight = false }
        switch result {
        case .success(let runtime):
            guard expected == generation else {
                runtime.stop()
                return
            }
            self.runtime = runtime
            running = installation
            failure = nil
            storeFailure = nil
            runtime.setSourcePolicy(listed: active)
            if !found.isEmpty { runtime.discovered(found.map(\.found)) }
            stores.hosts.sawLocalNetwork(permission)
            stores.attach(runtime)
            fed = stores
            Signposts.emit(.reconciled)
            answer(runtime)
        case .failure(let reason):
            guard expected == generation else { return }
            failure = reason.description
            storeFailure = reason.description
            answer(nil)
        }
    }

    private func answer(_ runtime: Runtime?) {
        let answered = waiting
        waiting = []
        for waiter in answered { waiter.resume(returning: runtime) }
    }

    /// The runtime serving `installation` once it has started, or nil when
    /// it failed or another took its place.
    public func started(_ installation: String) async -> Runtime? {
        if running == installation, let runtime { return runtime }
        let runtime = await withCheckedContinuation { waiting.append($0) }
        return running == installation ? runtime : nil
    }

    /// Binds the account on screen to a sign-in's refresh token; its profile
    /// spends the token from then on and brings the relay link up.
    public func bind(
        _ installation: String, cloud: URL, client: String, refreshToken: String
    ) async -> Result<Nothing, RuntimeFailure> {
        guard let runtime = await started(installation) else {
            return .failure(RuntimeFailure("this account's installation did not start"))
        }
        let bound = await runtime.signIn(cloud: cloud, client: client, refreshToken: refreshToken)
        await stores.refreshAccount()
        return bound
    }

    /// Stops the runtime, closing every chat on it first.
    public func stop() {
        generation += 1
        inFlight = false
        fed?.closeChats()
        fed?.attach(nil)
        fed = nil
        runtime?.stop()
        runtime = nil
        running = nil
    }

    /// Starts again after the store failed to open.
    public func relaunch() {
        storeFailure = nil
        restart()
    }

    /// In front of somebody, every listed agent keeps a source open and the
    /// browser runs. Put away, only the chats a push opens keep one, and the
    /// browser stops and forgets what it found.
    public func setActive(_ active: Bool) {
        guard self.active != active else { return }
        self.active = active
        runtime?.setSourcePolicy(listed: active)
        if active {
            discovery?.start()
            start()
        } else {
            discovery?.stop()
        }
    }

    /// What the browser found, handed over whole, and to any runtime started
    /// later.
    public func discovered(_ hosts: [FoundHost]) {
        found = hosts
        runtime?.discovered(hosts.map(\.found))
    }

    /// Deletes an installation nothing runs from.
    public func delete(installation: String) {
        guard installation != running else { return }
        try? FileManager.default.removeItem(at: directory(of: installation))
    }
}
