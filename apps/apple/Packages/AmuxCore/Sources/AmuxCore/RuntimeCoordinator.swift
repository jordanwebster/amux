import AmuxValues
import Foundation
import Observation

/// Runs this phone's installation and keeps the stores of the account on
/// screen fed from its profile.
///
/// One installation per process, with one profile per account, as a
/// desktop has. The profile on screen is the only one whose fleet is open,
/// the only one whose every listed agent keeps a source while the app is in
/// front of somebody, and the only bound one whose relay link runs: every
/// other bound profile is paused. Switching account never stops the
/// installation. Starting reads this phone's own store before any network is
/// dialled, so the first frame after a launch is what the phone last held.
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
        /// A served test relay's plaintext carrier, as `ip:port`. Only a
        /// driving build's library reads it.
        public var relayTCP: String?

        public init(discoveryScope: String = "", lanBind: String? = nil, relayTCP: String? = nil) {
            self.discoveryScope = discoveryScope
            self.lanBind = lanBind
            self.relayTCP = relayTCP
        }
    }

    public typealias Starter = @Sendable (StartConfig, @escaping @Sendable (UInt64) -> Void)
        throws(RuntimeFailure) -> Runtime

    /// The installation, once started.
    public private(set) var runtime: Runtime?
    /// The fleet of the profile on screen.
    public private(set) var profile: Profile?
    /// The installation's profiles as last read.
    public private(set) var profiles: [ProfileView] = []
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
    @ObservationIgnored private var active = true
    @ObservationIgnored private var found: [FoundHost] = []
    @ObservationIgnored private var permission: LocalNetworkPermission = .unknown
    @ObservationIgnored private var fed: StoreBundle?
    /// Which opening of a profile the wakes belong to.
    @ObservationIgnored private var opening = 0
    @ObservationIgnored private var waiting: [CheckedContinuation<Runtime?, Never>] = []
    @ObservationIgnored private var starting: Task<Void, Never>?
    /// Which start a started runtime answers; one a stop overtook is stopped.
    @ObservationIgnored private var launch = 0
    /// Profiles with a pause or resume in flight, so a second is not sent.
    @ObservationIgnored private var steering: Set<String> = []

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
        registry.switching = { [weak self] _ in self?.place() }
    }

    /// The stores on screen.
    public var stores: StoreBundle { registry.stores ?? signedOutStores }

    /// Where the installation lives: every profile's key, trust and store.
    public var directory: URL {
        support.appendingPathComponent("installation", isDirectory: true)
    }

    /// Starts the installation, if it is not running or starting.
    public func start() {
        guard runtime == nil, starting == nil else { return }
        // Before one installation held every account, each had a directory
        // of its own here; nothing reads them now.
        try? FileManager.default.removeItem(
            at: support.appendingPathComponent("installations", isDirectory: true))
        let directory = directory
        let config = StartConfig(
            dataDir: directory.path, deviceName: deviceName,
            discoveryScope: options.discoveryScope, lan: true, lanBind: options.lanBind,
            logPath: directory.appendingPathComponent("runtime.log").path,
            relayTcp: options.relayTCP, tail: nil)
        let starter = starter
        launch += 1
        let expected = launch
        starting = Task.detached {
            let started: Result<Runtime, RuntimeFailure>
            do {
                try FileManager.default.createDirectory(
                    at: directory, withIntermediateDirectories: true)
                started = .success(try starter(config) { _ in
                    Task { @MainActor [weak self] in self?.profilesMoved() }
                })
            } catch let failure as RuntimeFailure {
                started = .failure(failure)
            } catch {
                started = .failure(RuntimeFailure(error.localizedDescription))
            }
            await self.started(started, launch: expected)
        }
    }

    private func started(_ result: Result<Runtime, RuntimeFailure>, launch expected: Int) {
        guard expected == launch else {
            if case .success(let runtime) = result { runtime.stop() }
            return
        }
        starting = nil
        switch result {
        case .success(let runtime):
            self.runtime = runtime
            failure = nil
            storeFailure = nil
            if !found.isEmpty { runtime.discovered(found.map(\.found)) }
            profilesMoved()
            Signposts.emit(.reconciled)
            answer(runtime)
        case .failure(let reason):
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

    /// The installation once it has started, or nil when it failed.
    public func started() async -> Runtime? {
        if let runtime { return runtime }
        guard starting != nil else { return nil }
        return await withCheckedContinuation { waiting.append($0) }
    }

    /// Reads the profile list again, and puts the one on screen in front.
    public func profilesMoved() {
        guard let runtime else { return }
        let listed = runtime.profiles()
        let added = Set(listed.map(\.id)).subtracting(profiles.map(\.id))
        profiles = listed
        // A profile made since the browser last reported has not been told
        // what is on the network.
        if !added.isEmpty, !found.isEmpty { runtime.discovered(found.map(\.found)) }
        registry.show(listed)
        place()
    }

    /// Opens the fleet of the profile on screen when it is not the one open,
    /// then leaves every other profile on demand and paused.
    private func place() {
        guard let runtime, let id = registry.profile else { return }
        let stores = stores
        if profile?.id != id || fed !== stores {
            close()
            opening += 1
            let expected = opening
            do {
                let opened = try runtime.open(id) { chat in
                    Task { @MainActor [weak self] in
                        guard self?.opening == expected else { return }
                        stores.woke(chat)
                    }
                }
                profile = opened
                fed = stores
                stores.hosts.sawLocalNetwork(permission)
                stores.attach(opened)
            } catch {
                failure = error.description
            }
        }
        steer()
    }

    /// Only the profile on screen lists every agent's source, and only in
    /// front of somebody; only it, of the bound profiles, keeps its relay
    /// link, so one link is live.
    private func steer() {
        guard let runtime else { return }
        let onScreen = registry.profile
        for view in profiles {
            let shown = view.id == onScreen
            runtime.setSourcePolicy(view.id, listed: shown && active)
            let binding = view.account.binding
            if shown, binding == .paused {
                steer(view.id) { await runtime.resume($0) }
            } else if !shown, binding == .signedIn {
                steer(view.id) { await runtime.pause($0) }
            }
        }
    }

    private func steer(
        _ id: String, _ act: @escaping @Sendable (String) async -> Result<ProfileView, RuntimeFailure>
    ) {
        guard steering.insert(id).inserted else { return }
        Task {
            _ = await act(id)
            steering.remove(id)
            profilesMoved()
            await stores.refreshAccount()
        }
    }

    /// Closes the fleet on screen, and every chat on it first.
    private func close() {
        opening += 1
        fed?.closeChats()
        fed?.attach(nil)
        fed = nil
        profile?.close()
        profile = nil
    }

    /// Binds an account that signed in with its refresh token, and puts it
    /// on screen; its profile spends the token from then on and brings the
    /// relay link up. An account this phone knows binds its own profile; the
    /// first account takes over the profile nobody signed in on, keeping
    /// what it paired; any other gets a profile of its own.
    public func bind(
        _ account: AccountId, cloud: URL, client: String, refreshToken: String
    ) async -> Result<Nothing, RuntimeFailure> {
        guard let runtime = await started() else {
            return .failure(RuntimeFailure("this phone’s runtime did not start"))
        }
        let known = registry.accounts.first { $0.id == account }?.profile
        let target = known ?? (registry.selected == nil ? registry.unbound : nil)
        let bound = await runtime.bind(
            target, cloud: cloud, client: client, refreshToken: refreshToken)
        guard case .success(let view) = bound else {
            return bound.map { _ in Nothing() }
        }
        profilesMoved()
        registry.select(AccountId(view.subject))
        await stores.refreshAccount()
        return .success(Nothing())
    }

    /// Signs an account out; its profile stays, tied to the account and
    /// still reaching the machines on this network.
    public func signOut(_ account: AccountId) async {
        guard let runtime = await started(),
              let entry = registry.accounts.first(where: { $0.id == account }) else { return }
        let paused = profiles.first { $0.id == entry.profile }?.account.binding == .paused
        _ = await runtime.signOut(entry.profile)
        // A paused profile reads as paused whether or not it is signed in.
        if paused { _ = await runtime.resume(entry.profile) }
        profilesMoved()
        await stores.refreshAccount()
    }

    /// Takes an account off this phone: its profile — its key, the machines
    /// it paired, what it held — is deleted. The installation keeps one
    /// profile at least, so removing the last account leaves a fresh one
    /// nobody has signed in on.
    public func remove(_ account: AccountId) async {
        guard let runtime = await started(),
              let entry = registry.accounts.first(where: { $0.id == account }) else { return }
        if profiles.count == 1 {
            _ = await runtime.createProfile()
            profilesMoved()
        }
        registry.leave(account)
        _ = await runtime.deleteProfile(entry.profile)
        profilesMoved()
    }

    /// Stops the installation, closing the fleet and every chat on it first.
    public func stop() {
        close()
        launch += 1
        starting = nil
        answer(nil)
        runtime?.stop()
        runtime = nil
        profiles = []
        steering = []
    }

    /// Starts again after the store failed to open.
    public func relaunch() {
        storeFailure = nil
        stop()
        start()
    }

    /// In front of somebody, every listed agent of the profile on screen
    /// keeps a source open and the browser runs. Put away, only the chats a
    /// push opens keep one, and the browser stops and forgets what it found.
    public func setActive(_ active: Bool) {
        guard self.active != active else { return }
        self.active = active
        steer()
        if active {
            discovery?.start()
            start()
        } else {
            discovery?.stop()
        }
    }

    /// Brings one agent's chat current for a push. In the background the
    /// profile is put under the on-demand policy first, so the chat's own
    /// open is the only source that runs; coming to the foreground lists
    /// every agent again.
    public func warm(
        _ agent: AgentKey, inBackground: Bool, within limit: Duration = .seconds(25)
    ) async -> Bool {
        if inBackground { setActive(false) }
        start()
        guard await started() != nil, profile != nil else { return false }
        return await stores.warm(agent, within: limit)
    }

    /// What the browser found, handed over whole to every profile, and to
    /// any runtime started later.
    public func discovered(_ hosts: [FoundHost]) {
        found = hosts
        runtime?.discovered(hosts.map(\.found))
    }

    /// A bearer for an account, borrowed from its profile, which alone holds
    /// the account's refresh token.
    public func bearer(for account: AccountId) async -> String? {
        guard let runtime,
              let entry = registry.accounts.first(where: { $0.id == account }),
              case .success(let bearer) = await runtime.accessToken(entry.profile)
        else { return nil }
        return bearer.bearer
    }

    /// Asks the account service what an account buys now, over its
    /// profile's relay link.
    @discardableResult
    public func refreshEntitlement(for account: AccountId) async -> Bool {
        guard let runtime,
              let entry = registry.accounts.first(where: { $0.id == account }),
              case .success = await runtime.refreshEntitlement(entry.profile)
        else { return false }
        return true
    }

    /// The tail of the installation's log, for a report.
    public func logTail(bytes: Int = 64 * 1024) -> String? {
        guard let handle = try? FileHandle(
            forReadingFrom: directory.appendingPathComponent("runtime.log"))
        else { return nil }
        defer { try? handle.close() }
        let end = (try? handle.seekToEnd()) ?? 0
        try? handle.seek(toOffset: end > UInt64(bytes) ? end - UInt64(bytes) : 0)
        return (try? handle.readToEnd()).map { String(decoding: $0, as: UTF8.self) }
    }
}
