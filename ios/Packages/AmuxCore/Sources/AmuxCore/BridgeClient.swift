import AmuxMobile
import Foundation

/// How to start the shared runtime. The field names are the bridge's own, so
/// this is the configuration it reads rather than a translation of it.
public struct BridgeConfiguration: Codable, Sendable, Equatable {
    public var data_dir: String
    public var cache_dir: String
    public var device_name: String
    public var relay: Relay
    public var log_path: String
    /// One callback batch per display frame by default.
    public var frame_interval_ns: UInt64

    public struct Relay: Codable, Sendable, Equatable {
        public var url: String
        public var tls: Tls
        public var token: Token

        public init(url: String, tls: Tls, token: Token) {
            self.url = url
            self.tls = tls
            self.token = token
        }
    }

    public enum Tls: String, Codable, Sendable, Equatable {
        case system = "System"
        /// A test relay on this machine, which has no certificate to trust.
        case plainLoopback = "PlainLoopback"
    }

    /// Where the relay credential comes from. `callback` means the bridge asks
    /// this app each time, which is what a rotating cloud token needs.
    public enum Token: Codable, Sendable, Equatable {
        case fixed(String)
        case callback

        private enum Key: String, CodingKey { case Static }

        public init(from decoder: any Decoder) throws {
            if let text = try? decoder.singleValueContainer().decode(String.self), text == "Callback" {
                self = .callback
                return
            }
            let container = try decoder.container(keyedBy: Key.self)
            self = .fixed(try container.decode(String.self, forKey: .Static))
        }

        public func encode(to encoder: any Encoder) throws {
            switch self {
            case .callback:
                var container = encoder.singleValueContainer()
                try container.encode("Callback")
            case .fixed(let bearer):
                var container = encoder.container(keyedBy: Key.self)
                try container.encode(bearer, forKey: .Static)
            }
        }
    }

    public init(
        dataDirectory: URL, cacheDirectory: URL, deviceName: String, relay: Relay,
        logPath: URL, frameIntervalNanoseconds: UInt64 = 16_666_667
    ) {
        self.data_dir = dataDirectory.path
        self.cache_dir = cacheDirectory.path
        self.device_name = deviceName
        self.relay = relay
        self.log_path = logPath.path
        self.frame_interval_ns = frameIntervalNanoseconds
    }
}

public enum BridgeError: Error, Sendable, Equatable {
    /// The bridge refused the configuration or could not create its worker.
    case didNotStart
}

/// The shared Rust runtime, as one object.
///
/// Rust owns delivery: callbacks arrive serially on its worker, coalesced to
/// one batch per frame, and must return promptly. So the bytes are copied and
/// handed to `events` immediately, and every store write happens later on the
/// main actor, in the order the batches arrived. Nothing here interprets an
/// event; interpreting is what the stores do.
public final class BridgeClient: Sendable {
    private struct State {
        var handle: OpaquePointer?
        var stopped = false
    }

    private final class Delivery: @unchecked Sendable {
        let batches: AsyncStream<[Event]>.Continuation
        let decoder = AmuxJSON.decoder
        private let lock = NSLock()
        /// Weak on purpose: the client owns this and the callback context
        /// holds it, so a strong link back would keep both alive forever and
        /// the runtime would never be stopped.
        private weak var owner: BridgeClient?
        private var unreadable: [String] = []

        init(batches: AsyncStream<[Event]>.Continuation) {
            self.batches = batches
        }

        var client: BridgeClient? {
            get { lock.withLock { owner } }
            set { lock.withLock { owner = newValue } }
        }

        /// Callback bytes that were not the pinned schema. Kept, because a
        /// batch this build cannot read is a fact worth reporting.
        var malformed: [String] { lock.withLock { unreadable } }

        func receive(_ json: UnsafePointer<CChar>) {
            let data = Data(String(cString: json).utf8)
            guard let batch = try? decoder.decode([Event].self, from: data) else {
                lock.withLock { unreadable.append(String(decoding: data, as: UTF8.self)) }
                return
            }
            batches.yield(batch)
            for event in batch {
                guard case .tokenRequest(let request) = event else { continue }
                client?.answerToken(request)
            }
        }
    }

    /// A value only one thread touches at a time.
    private final class Locked<Value>: @unchecked Sendable {
        private let lock = NSLock()
        private var value: Value

        init(_ value: Value) { self.value = value }

        func withLock<Result>(_ body: (inout Value) -> Result) -> Result {
            lock.lock()
            defer { lock.unlock() }
            return body(&value)
        }
    }

    /// One batch of projected events per callback, in arrival order.
    public let events: AsyncStream<[Event]>

    private let delivery: Delivery
    private let state: Locked<State>
    private let tokenProvider: @Sendable (UInt64) async -> ConnectToken?

    public init(
        configuration: BridgeConfiguration,
        tokenProvider: @escaping @Sendable (UInt64) async -> ConnectToken? = { _ in nil }
    ) throws {
        var continuation: AsyncStream<[Event]>.Continuation!
        self.events = AsyncStream { continuation = $0 }
        self.delivery = Delivery(batches: continuation)
        self.tokenProvider = tokenProvider
        self.state = Locked(State())

        let json = try AmuxJSON.encoder.encode(configuration)
        let context = Unmanaged.passRetained(delivery).toOpaque()
        let handle: OpaquePointer? = String(decoding: json, as: UTF8.self).withCString { config in
            amux_mobile_start(config, { bytes, context in
                guard let bytes, let context else { return }
                Unmanaged<Delivery>.fromOpaque(context).takeUnretainedValue().receive(bytes)
            }, context)
        }
        guard let handle else {
            Unmanaged<Delivery>.fromOpaque(context).release()
            continuation.finish()
            throw BridgeError.didNotStart
        }
        state.withLock { $0.handle = handle }
        delivery.client = self
    }

    /// Enqueues a command and answers with the identifier its result will
    /// carry. A command the bridge cannot read fails as an OpResult, never as
    /// a crash, so a refusal is always visible on the same path as a success.
    @discardableResult
    public func dispatch(_ command: BridgeCommand) -> OpId? {
        guard let json = try? AmuxJSON.encoder.encode(command) else { return nil }
        return state.withLock { state -> OpId? in
            guard let handle = state.handle else { return nil }
            guard let reply = String(decoding: json, as: UTF8.self).withCString({
                amux_mobile_dispatch(handle, $0)
            }) else { return nil }
            defer { amux_mobile_free(reply) }
            return OpId(String(cString: reply))
        }
    }

    /// Matches callback cadence to the display the app is actually drawing on.
    public func setFrameInterval(nanoseconds: UInt64) {
        state.withLock { state in
            guard let handle = state.handle else { return }
            amux_mobile_set_frame_interval(handle, nanoseconds)
        }
    }

    /// The shared reducer's model, frozen. Never call this from an event
    /// callback: it waits for the worker that delivers them.
    public func snapshot() -> String? {
        state.withLock { state in
            guard let handle = state.handle, let json = amux_mobile_snapshot(handle) else { return nil }
            defer { amux_mobile_free(json) }
            return String(cString: json)
        }
    }

    /// Stops the runtime and joins its worker; no callback can follow.
    public func stop() {
        let handle: OpaquePointer? = state.withLock { state in
            guard !state.stopped else { return nil }
            state.stopped = true
            defer { state.handle = nil }
            return state.handle
        }
        guard let handle else { return }
        amux_mobile_stop(handle)
        delivery.batches.finish()
        delivery.client = nil
        // Balances the reference handed to the callback context at start.
        Unmanaged.passUnretained(delivery).release()
    }

    /// Callback bytes this build could not read as the pinned schema.
    public var malformedBatches: [String] { delivery.malformed }

    /// Hands the runtime's own callback a batch of projected events.
    ///
    /// The performance harness generates its workloads as the JSON the
    /// runtime emits and pushes them through here, so a measured run decodes,
    /// orders and applies exactly what a relay-fed run would; only the relay
    /// is missing. Nothing but a measurement should call this — the runtime
    /// is the source of events in every other case.
    public func deliverAsRuntime(_ json: String) {
        json.withCString { delivery.receive($0) }
    }

    private func answerToken(_ request: UInt64) {
        Task { [tokenProvider] in
            let token = await tokenProvider(request)
            let reply: [String: JSONValue] = if let token {
                token.expiresAt.map {
                    ["token": .string(token.bearer), "expires_at": .int(Int($0.timeIntervalSince1970))]
                } ?? ["token": .string(token.bearer)]
            } else {
                ["error": .string("no credential for this account")]
            }
            guard let json = try? AmuxJSON.encoder.encode(reply) else { return }
            self.state.withLock { state in
                guard let handle = state.handle else { return }
                String(decoding: json, as: UTF8.self).withCString {
                    amux_mobile_token_reply(handle, request, $0)
                }
            }
        }
    }

    deinit {
        stop()
    }
}

/// Applies batches to the stores, in the order they arrived, on the main
/// actor. This is the only place bridge events become screen state.
@MainActor
public func applyBatches(
    from events: AsyncStream<[Event]>,
    to registry: AccountRegistry,
    for account: AccountId
) async {
    for await batch in events {
        registry.deliver(batch, for: account)
    }
}

/// A command for the bridge. The shared vocabulary is the core's, so a command
/// is carried as the JSON that vocabulary defines rather than restated here;
/// subscription is the client's own and is named.
public enum BridgeCommand: Sendable, Equatable, Codable {
    case subscribe(agent: AgentId)
    case unsubscribe(agent: AgentId)
    /// A shared UI command, exactly as the core spells it.
    case shared(JSONValue)

    private enum Key: String, CodingKey { case command, agent }

    public init(from decoder: any Decoder) throws {
        let body = try JSONValue(from: decoder)
        if let container = try? decoder.container(keyedBy: Key.self),
           let command = try? container.decode(String.self, forKey: .command),
           command == "subscribe" || command == "unsubscribe" {
            let agent = try container.decode(AgentId.self, forKey: .agent)
            self = command == "subscribe" ? .subscribe(agent: agent) : .unsubscribe(agent: agent)
            return
        }
        self = .shared(body)
    }

    public func encode(to encoder: any Encoder) throws {
        switch self {
        case .subscribe(let agent):
            var container = encoder.container(keyedBy: Key.self)
            try container.encode("subscribe", forKey: .command)
            try container.encode(agent, forKey: .agent)
        case .unsubscribe(let agent):
            var container = encoder.container(keyedBy: Key.self)
            try container.encode("unsubscribe", forKey: .command)
            try container.encode(agent, forKey: .agent)
        case .shared(let body):
            try body.encode(to: encoder)
        }
    }
}
