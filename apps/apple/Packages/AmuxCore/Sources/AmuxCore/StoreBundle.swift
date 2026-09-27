import AmuxValues
import Foundation
import Observation

/// Everything one account's screens draw, over the runtime that serves it.
///
/// The runtime wakes this once per turn for however much changed; the
/// bundle takes the changes and reads the fleet and hosts again. Acts go
/// to the runtime and their answers land in the store the screen draws.
@MainActor
@Observable
public final class StoreBundle {
    public let account: AccountId
    public let fleet: FleetStore
    public let hosts: HostsStore
    public let pairing: PairingStore
    public let newAgent: NewAgentStore
    /// How many times the fleet was read, for a driver waiting on one.
    public private(set) var applied = 0

    /// The runtime serving this account, while it runs.
    @ObservationIgnored public private(set) var runtime: Runtime?
    /// Open chats by the id the runtime's wake names them with.
    @ObservationIgnored private var chats: [UInt64: (chat: Chat, woke: @MainActor () -> Void)] = [:]
    /// The chats pages hold, by agent.
    @ObservationIgnored private var models: [AgentKey: (chat: Chat, model: ChatModel)] = [:]
    /// The agents whose chat a page shows.
    @ObservationIgnored private var shown: Set<AgentKey> = []
    @ObservationIgnored public let now: @MainActor () -> Date
    /// Told how many machines this account has paired and how many of its
    /// agents need the person, each time the fleet is read.
    @ObservationIgnored public var saw: (@MainActor (_ hosts: Int, _ attention: Int) -> Void)?
    /// Whether a re-read of a relay link still coming up is scheduled.
    @ObservationIgnored private var settling = false

    public init(account: AccountId, clock: @escaping @MainActor () -> Date = { Date() }) {
        self.account = account
        self.now = clock
        self.fleet = FleetStore(now: clock())
        self.hosts = HostsStore(clock: clock)
        self.pairing = PairingStore(clock: clock)
        self.newAgent = NewAgentStore()
    }

    /// Starts drawing from a runtime, or stops when it is gone.
    public func attach(_ runtime: Runtime?) {
        self.runtime = runtime
        guard runtime != nil else { return }
        read(hostsMoved: true)
    }

    /// The runtime moved: the fleet when `chat` is 0, else that chat.
    public func woke(_ chat: UInt64) {
        if chat == 0 {
            guard let runtime else { return }
            let changes = runtime.takeFleetChanges()
            read(hostsMoved: changes.hosts)
        } else {
            chats[chat]?.woke()
        }
    }

    private func read(hostsMoved: Bool) {
        guard let runtime else { return }
        let views = runtime.hosts()
        fleet.show(runtime.fleetRows(expanding: Array(fleet.expanded)), hosts: views)
        hosts.show(views)
        newAgent.remember(fleet.rows)
        saw?(fleet.machines.filter(\.trusted).count, fleet.rows.filter(\.needsYou).count)
        applied += 1
        if hostsMoved {
            Task { await refreshAccount() }
            if hosts.readingDevices || hosts.roster == nil { Task { await refreshRoster() } }
        }
    }

    /// Reads the account and its relay link again.
    public func refreshAccount() async {
        guard let runtime, case .success(let account) = await runtime.account() else { return }
        hosts.show(account)
        fleet.relay(account.relay)
        // The relay link settles without the host list moving, so while it
        // is still coming up it is read again until it says where it landed.
        guard account.relay == .connecting || account.relay == .retrying,
              !settling else { return }
        settling = true
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(1))
            guard let self, self.runtime === runtime else { return }
            self.settling = false
            await self.refreshAccount()
        }
    }

    /// Reads this phone's identity and the machines it trusts again.
    public func refreshRoster() async {
        guard let runtime, case .success(let roster) = await runtime.roster() else { return }
        hosts.show(roster)
    }

    /// Lists or folds a family's members.
    public func toggleFamily(_ head: AgentKey) {
        fleet.toggle(head)
        read(hostsMoved: false)
    }

    // MARK: - Chats

    /// Opens an agent's chat and routes its wakes to `woke`.
    public func openChat(
        _ agent: AgentKey, woke: @escaping @MainActor () -> Void
    ) throws(RuntimeFailure) -> Chat {
        guard let runtime else { throw RuntimeFailure("nothing is running for this account") }
        let chat = try runtime.openChat(agent)
        chats[chat.id] = (chat, woke)
        fleet.opened(agent, at: now())
        return chat
    }

    public func closeChat(_ chat: Chat) {
        chats.removeValue(forKey: chat.id)
        chat.close()
    }

    /// The chat a page shows, opened the first time it is asked for and kept
    /// until ``leave(_:)``: a child's chat pushed on top keeps the parent's
    /// draft and place.
    public func chat(_ agent: AgentKey) throws(RuntimeFailure) -> ChatModel {
        shown.insert(agent)
        return try model(agent)
    }

    private func model(_ agent: AgentKey) throws(RuntimeFailure) -> ChatModel {
        if let open = models[agent] { return open.model }
        let chat = try openChat(agent) { [weak self] in self?.models[agent]?.model.woke() }
        let model = ChatModel(source: chat)
        models[agent] = (chat, model)
        return model
    }

    /// The page showing this chat is gone: close it.
    public func leave(_ agent: AgentKey) {
        shown.remove(agent)
        guard let open = models.removeValue(forKey: agent) else { return }
        closeChat(open.chat)
    }

    /// Brings one agent's chat current, for a push that woke the app in the
    /// background: the ordinary open, which asks the agent's host for what
    /// this phone missed, then a wait for it to catch up. Nothing else is
    /// brought current. A chat no page shows is closed again; its rows stay
    /// in the store, so tapping the notification opens it at once.
    @discardableResult
    public func warm(_ agent: AgentKey, within limit: Duration = .seconds(25)) async -> Bool {
        guard let model = try? model(agent) else { return false }
        let deadline = ContinuousClock.now + limit
        while model.frame?.caughtUp != true, ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(100))
        }
        let current = model.frame?.caughtUp == true
        if !shown.contains(agent), let open = models.removeValue(forKey: agent) {
            closeChat(open.chat)
        }
        return current
    }

    /// Closes every chat, before the runtime under them stops.
    public func closeChats() {
        for open in chats.values { open.chat.close() }
        chats.removeAll()
        models.removeAll()
        shown.removeAll()
    }

    // MARK: - Pairing

    /// Reaches a machine with the six digits typed so far, once there are six.
    @discardableResult
    public func pair(digits typed: String) -> Bool {
        pairing.enter(typed)
        guard let request = pairing.request else { return false }
        begin(request, relayOnly: pairing.machine.map { $0.addrs.isEmpty } ?? true)
        return true
    }

    /// Reaches the machine a pairing link names.
    @discardableResult
    public func pair(link: String) -> Bool {
        guard runtime != nil else { return false }
        pairing.open()
        begin(.link(link), relayOnly: false)
        return true
    }

    private func begin(_ request: PairRequest, relayOnly: Bool) {
        guard let runtime else { return }
        let attempt = pairing.checking()
        // A machine only the relay can reach is refused on an account that
        // does not buy the relay; said as such rather than as a wrong code.
        let unsubscribed = relayOnly && hosts.account?.pro == false
        Task {
            let reached = await runtime.beginPair(request)
            pairing.reached(reached, attempt: attempt, relayOnly: unsubscribed)
        }
    }

    public func confirmPairing(_ pending: PendingPair) {
        guard let runtime else { return }
        let attempt = pairing.checking()
        Task {
            let paired = await runtime.confirmPair(pending)
            pairing.paired(paired, attempt: attempt)
            await refreshRoster()
        }
    }

    public func abandonPairing(_ pending: PendingPair) {
        pairing.abandoned()
        guard let runtime else { return }
        Task { _ = await runtime.abandonPair(pending) }
    }

    /// Stops trusting a machine at once; the row leaves when the runtime
    /// confirms.
    public func revoke(_ host: HostId) {
        guard let runtime else { return }
        Task {
            _ = await runtime.unpair(host)
            await refreshRoster()
        }
    }

    // MARK: - Starting an agent

    public func startNewAgent(on host: HostId?) {
        newAgent.open(on: host)
        newAgent.remember(fleet.rows)
        guard let host else { return }
        listDirectories(on: host, matching: "")
    }

    public func point(at host: HostId) {
        guard newAgent.machine != host else { return }
        newAgent.point(at: host)
        listDirectories(on: host, matching: "")
    }

    public func searchDirectories() {
        guard newAgent.searchesTheMachine, let host = newAgent.machine else { return }
        listDirectories(on: host, matching: newAgent.query)
    }

    private func listDirectories(on host: HostId, matching query: String) {
        let asked = newAgent.asking()
        guard let runtime else {
            newAgent.listed(.failure(RuntimeFailure("nothing is running")), asked: asked)
            return
        }
        Task {
            let listed = await runtime.directories(
                on: host, matching: query, limit: NewAgentStore.limit)
            newAgent.listed(listed, asked: asked)
        }
    }

    @discardableResult
    public func startAgent() -> Bool {
        guard let request = newAgent.request, let runtime else { return false }
        newAgent.starts()
        Task { newAgent.started(await runtime.createAgent(request)) }
        return true
    }

    // MARK: - Agents

    public func rename(_ name: String, of agent: AgentKey) async -> Result<Nothing, RuntimeFailure> {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let runtime else {
            return .failure(RuntimeFailure("a name cannot be empty"))
        }
        return await runtime.act(.rename(trimmed), on: agent)
    }

    public func stop(_ agent: AgentKey) async -> Result<Nothing, RuntimeFailure> {
        guard let runtime else { return .failure(RuntimeFailure("nothing is running")) }
        return await runtime.act(.stop, on: agent)
    }

    public func delete(_ agent: AgentKey) async -> Result<Nothing, RuntimeFailure> {
        guard let runtime else { return .failure(RuntimeFailure("nothing is running")) }
        return await runtime.act(.delete, on: agent)
    }

    // MARK: - Diagnostics

    /// Writes a dump of this account's profile and answers its directory.
    public func dump(reason: String) async -> Result<URL, RuntimeFailure> {
        guard let runtime else { return .failure(RuntimeFailure("nothing is running")) }
        return await runtime.dump(reason: reason).map { URL(fileURLWithPath: $0) }
    }
}
