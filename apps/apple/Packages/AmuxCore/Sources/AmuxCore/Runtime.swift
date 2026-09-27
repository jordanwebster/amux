import AmuxApp
import AmuxValues
import Foundation

/// Why the library did not do what it was asked, in its own words.
public struct RuntimeFailure: Error, Sendable, Equatable, CustomStringConvertible {
    public let description: String

    public init(_ description: String) {
        self.description = description
    }
}

/// The shared Rust runtime: this phone's profile, hosted in the app's own
/// process, and the fleet and chats over it.
///
/// Every value crosses as JSON whose Swift mirror is generated from the Rust
/// definition, so nothing here interprets a row, a card or a host; it only
/// hands them over. Reads return at once. Acts that wait on a machine are
/// `async` and resume on whatever thread the library answers on.
///
/// The wake is called on a library worker with the id of the chat that moved,
/// or 0 for the fleet, at most once until that one's changes are taken. The
/// owner schedules the take on the main actor and returns.
public final class Runtime: @unchecked Sendable {
    private let handle: OpaquePointer
    private let waker: Unmanaged<Waker>
    /// Held across every call into the library, so stopping waits for the
    /// calls in flight and none starts after it. Recursive, because a call
    /// may be made from inside another on the same thread.
    private let lock = NSRecursiveLock()
    private var running = true

    private init(handle: OpaquePointer, waker: Unmanaged<Waker>) {
        self.handle = handle
        self.waker = waker
    }

    /// Starts the runtime; blocks until its store is open and the fleet has
    /// caught up with it, so the first read draws what this phone holds.
    public static func start(
        _ config: StartConfig, wake: @escaping @Sendable (UInt64) -> Void
    ) throws(RuntimeFailure) -> Runtime {
        let waker = Unmanaged.passRetained(Waker(wake))
        var error: UnsafeMutablePointer<CChar>?
        let started = Bridge.json(config).withCString { config in
            amux_runtime_start(config, { context, chat in
                guard let context else { return }
                Unmanaged<Waker>.fromOpaque(context).takeUnretainedValue().wake(chat)
            }, waker.toOpaque(), &error)
        }
        guard let started else {
            waker.release()
            let reason = error.map { String(cString: $0) } ?? "the runtime did not start"
            if let error { amux_string_free(error) }
            throw RuntimeFailure(reason)
        }
        return Runtime(handle: started, waker: waker)
    }

    /// Stops the runtime and flushes its store. Close every chat first; any
    /// read after this answers nothing.
    public func stop() {
        let wasRunning = lock.withLock {
            defer { running = false }
            return running
        }
        guard wasRunning else { return }
        amux_runtime_stop(handle)
        waker.release()
    }

    /// Runs a call on the live runtime, or answers `stopped` once it is not.
    private func call<T>(_ stopped: T, _ body: (OpaquePointer) -> T) -> T {
        lock.withLock { running ? body(handle) : stopped }
    }

    deinit { stop() }

    // MARK: - The fleet

    /// Every family head, loudest first, with the members of the families
    /// whose root agent ids `expanding` names under them.
    public func fleetRows(expanding: [[UInt8]] = []) -> [FleetRow] {
        call([]) { live in
            Bridge.json(expanding).withCString {
                Bridge.read([FleetRow].self, amux_fleet_rows(live, $0))
            } ?? []
        }
    }

    public func fleetCard(_ agent: AgentKey) -> FleetCard? {
        call(nil) { live in
            Bridge.json(agent).withCString {
                Bridge.read(FleetCard?.self, amux_fleet_card(live, $0))
            } ?? nil
        }
    }

    /// The family around an agent's chat: its parent and its children.
    public func family(_ agent: AgentKey) -> FamilyHeader? {
        call(nil) { live in
            Bridge.json(agent).withCString {
                Bridge.read(FamilyHeader?.self, amux_fleet_family(live, $0))
            } ?? nil
        }
    }

    /// Every host in the fleet, this phone first.
    public func hosts() -> [HostView] {
        call([]) { live in
            Bridge.read([HostView].self, amux_fleet_hosts(live)) ?? []
        }
    }

    /// What moved in the fleet since the last take; the next change wakes
    /// the owner again.
    public func takeFleetChanges() -> FleetChanges {
        call(FleetChanges(agents: [], hosts: false)) { live in
            Bridge.read(FleetChanges.self, amux_fleet_take_changes(live))
                ?? FleetChanges(agents: [], hosts: false)
        }
    }

    // MARK: - The profile

    /// Listed in the foreground: every listed agent keeps a source open.
    /// Not listed when a push woke the app in the background: only the chats
    /// that open keep one.
    public func setSourcePolicy(listed: Bool) {
        call(()) { amux_runtime_set_source_policy($0, listed) }
    }

    /// Whether every listed agent keeps a source open now.
    public var listsSources: Bool {
        call(false) { amux_runtime_source_policy_listed($0) }
    }

    /// Hands over the whole set this phone's browser found on the network.
    public func discovered(_ found: [Found]) {
        call(()) { live in Bridge.json(found).withCString { amux_runtime_discovered(live, $0) } }
    }

    /// Reaches and authenticates a machine; nothing is trusted until the
    /// attempt is confirmed.
    public func beginPair(_ request: PairRequest) async -> Result<PendingPair, RuntimeFailure> {
        await act(PendingPair.self) { live, callback, context in
            Bridge.json(request).withCString {
                amux_runtime_begin_pair(live, $0, callback, context)
            }
        }
    }

    public func confirmPair(_ pending: PendingPair) async -> Result<Paired, RuntimeFailure> {
        await act(Paired.self) { live, callback, context in
            Bridge.json(pending.token).withCString {
                amux_runtime_confirm_pair(live, $0, callback, context)
            }
        }
    }

    public func abandonPair(_ pending: PendingPair) async -> Result<Nothing, RuntimeFailure> {
        await act(Nothing.self) { live, callback, context in
            Bridge.json(pending.token).withCString {
                amux_runtime_abandon_pair(live, $0, callback, context)
            }
        }
    }

    /// Stops trusting a machine, telling it so where it can be reached.
    public func unpair(_ host: HostId) async -> Result<Nothing, RuntimeFailure> {
        await act(Nothing.self) { live, callback, context in
            Bridge.json(host.bytes).withCString {
                amux_runtime_unpair(live, $0, callback, context)
            }
        }
    }

    /// This phone and the machines it trusts.
    public func roster() async -> Result<Roster, RuntimeFailure> {
        await act(Roster.self) { live, callback, context in
            amux_runtime_roster(live, callback, context)
        }
    }

    public func account() async -> Result<AccountView, RuntimeFailure> {
        await act(AccountView.self) { live, callback, context in
            amux_runtime_account(live, callback, context)
        }
    }

    /// A bearer for the account service. The profile alone holds the refresh
    /// token, so this is the only way this app gets a fresh one.
    public func accessToken() async -> Result<Bearer, RuntimeFailure> {
        await act(Bearer.self) { live, callback, context in
            amux_runtime_access_token(live, callback, context)
        }
    }

    /// Asks the account service what the account buys now, over the relay
    /// link, so the relay's tier follows a purchase at once.
    public func refreshEntitlement() async -> Result<Nothing, RuntimeFailure> {
        await act(Nothing.self) { live, callback, context in
            amux_runtime_refresh_entitlement(live, callback, context)
        }
    }

    /// Binds this profile to an account with the refresh token a sign-in
    /// obtained as `client`. The profile spends the token from then on.
    public func signIn(
        cloud: URL, client: String, refreshToken: String
    ) async -> Result<Nothing, RuntimeFailure> {
        await act(Nothing.self) { live, callback, context in
            cloud.absoluteString.withCString { cloud in
                client.withCString { client in
                    refreshToken.withCString { token in
                        amux_runtime_sign_in(live, cloud, client, token, callback, context)
                    }
                }
            }
        }
    }

    public func signOut() async -> Result<Nothing, RuntimeFailure> {
        await act(Nothing.self) { live, callback, context in
            amux_runtime_sign_out(live, callback, context)
        }
    }

    /// Writes a dump of this profile with the fleet's and every open chat's
    /// part, and answers the bundle's directory.
    public func dump(reason: String) async -> Result<String, RuntimeFailure> {
        await act(String.self) { live, callback, context in
            reason.withCString { amux_runtime_dump(live, $0, callback, context) }
        }
    }

    // MARK: - Agents

    public func createAgent(_ agent: NewAgent) async -> Result<AgentKey, RuntimeFailure> {
        await act(AgentKey.self) { live, callback, context in
            Bridge.json(agent).withCString {
                amux_runtime_create_agent(live, $0, callback, context)
            }
        }
    }

    /// Where a host offers to start an agent, matching `query`.
    public func directories(
        on host: HostId, matching query: String, limit: UInt32
    ) async -> Result<Directories, RuntimeFailure> {
        await act(Directories.self) { live, callback, context in
            Bridge.json(host.bytes).withCString { host in
                query.withCString { query in
                    amux_runtime_directories(live, host, query, limit, callback, context)
                }
            }
        }
    }

    public func act(_ act: AgentAct, on agent: AgentKey) async -> Result<Nothing, RuntimeFailure> {
        await self.act(Nothing.self) { live, callback, context in
            Bridge.json(agent).withCString { agent in
                Bridge.json(act).withCString { act in
                    amux_runtime_agent_act(live, agent, act, callback, context)
                }
            }
        }
    }

    // MARK: - Chats

    /// Opens an agent's chat with `tail` rows, or the configured number when
    /// zero. Blocks until the rows this phone holds are applied, so the
    /// first read is right even with the agent's host away.
    public func openChat(_ agent: AgentKey, tail: UInt32 = 0) throws(RuntimeFailure) -> Chat {
        var error: UnsafeMutablePointer<CChar>?
        let opened = call(nil) { live in
            Bridge.json(agent).withCString { amux_session_open(live, $0, tail, &error) }
        }
        guard let opened else {
            let reason = error.map { String(cString: $0) } ?? "the chat did not open"
            if let error { amux_string_free(error) }
            throw RuntimeFailure(reason)
        }
        return Chat(handle: opened)
    }

    // MARK: - Acts

    /// Runs an act the library answers once with `{"Ok": T}` or
    /// `{"Err": reason}`.
    private func act<T: Decodable & Sendable>(
        _ type: T.Type,
        _ body: @escaping (OpaquePointer, AmuxCallback, UnsafeMutableRawPointer) -> Void
    ) async -> Result<T, RuntimeFailure> {
        await Bridge.answer(T.self) { callback, context in
            let ran = self.call(false) { live in
                body(live, callback, context)
                return true
            }
            if !ran { Bridge.stopped(callback, context) }
        }
    }
}

/// What a pairing trusted.
public struct Paired: Decodable, Sendable, Equatable {
    public var hostId: [UInt8]
    public var name: String

    private enum CodingKeys: String, CodingKey {
        case hostId = "host_id"
        case name
    }
}

/// An act that answers nothing but whether it happened.
public struct Nothing: Decodable, Sendable, Equatable {
    public init() {}
    public init(from decoder: any Decoder) throws {}
}

/// The owner's wake, held where the library can reach it from its worker.
private final class Waker: @unchecked Sendable {
    let wake: @Sendable (UInt64) -> Void

    init(_ wake: @escaping @Sendable (UInt64) -> Void) {
        self.wake = wake
    }
}

/// The JSON the library hands back and takes.
public enum Bridge {
    static func json<T: Encodable>(_ value: T) -> String {
        guard let data = try? JSONEncoder().encode(value) else { return "null" }
        return String(decoding: data, as: UTF8.self)
    }

    /// Decodes a string the library returned, and frees it.
    static func read<T: Decodable>(_ type: T.Type, _ text: UnsafeMutablePointer<CChar>?) -> T? {
        guard let text else { return nil }
        defer { amux_string_free(text) }
        let data = Data(bytesNoCopy: text, count: strlen(text), deallocator: .none)
        return try? JSONDecoder().decode(T.self, from: data)
    }

    /// Waits for a callback's one `{"Ok": T}` or `{"Err": reason}`.
    static func answer<T: Decodable & Sendable>(
        _ type: T.Type, _ call: (AmuxCallback, UnsafeMutableRawPointer) -> Void
    ) async -> Result<T, RuntimeFailure> {
        let text: String = await withCheckedContinuation { continuation in
            let reply = Unmanaged.passRetained(Reply(continuation)).toOpaque()
            call({ context, json in
                guard let context else { return }
                let reply = Unmanaged<Reply>.fromOpaque(context).takeRetainedValue()
                reply.continuation.resume(returning: json.map { String(cString: $0) } ?? "null")
            }, reply)
        }
        guard let answer = try? JSONDecoder().decode(Answer<T>.self, from: Data(text.utf8)) else {
            return .failure(RuntimeFailure("the runtime answered what this build cannot read"))
        }
        switch answer {
        case .ok(let value): return .success(value)
        case .err(let reason): return .failure(RuntimeFailure(reason))
        }
    }

    /// Answers a callback at once for a call that can no longer be made, so
    /// whoever waits on it is not left waiting.
    static func stopped(_ callback: AmuxCallback, _ context: UnsafeMutableRawPointer) {
        #"{"Err":"the runtime has stopped"}"#.withCString { callback(context, $0) }
    }

    /// Decodes a callback's JSON that is the value itself, not an answer.
    static func value<T: Decodable & Sendable>(
        _ type: T.Type, _ call: (AmuxCallback, UnsafeMutableRawPointer) -> Void
    ) async -> T? {
        let text: String = await withCheckedContinuation { continuation in
            let reply = Unmanaged.passRetained(Reply(continuation)).toOpaque()
            call({ context, json in
                guard let context else { return }
                let reply = Unmanaged<Reply>.fromOpaque(context).takeRetainedValue()
                reply.continuation.resume(returning: json.map { String(cString: $0) } ?? "null")
            }, reply)
        }
        return try? JSONDecoder().decode(T.self, from: Data(text.utf8))
    }

    private final class Reply: @unchecked Sendable {
        let continuation: CheckedContinuation<String, Never>

        init(_ continuation: CheckedContinuation<String, Never>) {
            self.continuation = continuation
        }
    }

    private enum Answer<T: Decodable>: Decodable {
        case ok(T)
        case err(String)

        private enum Key: String, CodingKey {
            case ok = "Ok"
            case err = "Err"
        }

        init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: Key.self)
            if let reason = try container.decodeIfPresent(String.self, forKey: .err) {
                self = .err(reason)
            } else if T.self == Nothing.self, try container.decodeNil(forKey: .ok) {
                self = .ok(Nothing() as! T)
            } else {
                self = .ok(try container.decode(T.self, forKey: .ok))
            }
        }
    }
}
