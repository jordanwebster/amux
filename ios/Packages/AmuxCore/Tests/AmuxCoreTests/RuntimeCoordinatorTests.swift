import Foundation
import XCTest
@testable import AmuxCore

private actor RelayCloud: CloudService {
    var calls: [AccountId] = []
    var host = "relay.example"
    var permitted = true
    func permit(_ permitted: Bool) { self.permitted = permitted }
    func use(_ host: String) { self.host = host }
    func connectToken(_ id: AccountId) async throws(CloudError) -> ConnectToken {
        calls.append(id)
        guard permitted else { throw .refused("A subscription is needed") }
        return ConnectToken(bearer: "token-\(id)", host: host, port: 443)
    }
    func signIn(presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount {
        throw .unauthenticated
    }
    func account(_ id: AccountId) async throws(CloudError) -> AccountFacts { throw .unauthenticated }
    func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement { throw .unauthenticated }
    func recordPurchase(_ id: AccountId, signedTransaction: String) async throws(CloudError) { throw .unauthenticated }
    func requestDeletion(_ id: AccountId, confirmedEmail: String) async throws(CloudError) -> DeletionOutcome {
        throw .unauthenticated
    }
    func uploadReport(_ id: AccountId, bundle: ReportBundle) async throws(CloudError) -> ReportReceipt {
        throw .unauthenticated
    }
}

@MainActor
final class ScriptedRuntime: AppRuntime {
    let events: AsyncStream<[Event]>
    let replies: AsyncStream<[Event]>.Continuation
    var commands: [BridgeCommand] = []
    var activity: [Bool] = []
    var stopped = false
    init() { (events, replies) = AsyncStream.makeStream() }
    func dispatch(_ command: BridgeCommand) -> OpId? {
        commands.append(command)
        return OpId(UUID().uuidString)
    }
    func attach(_ picked: PickedAttachment, bytes: Data) -> OpId? { OpId(UUID().uuidString) }
    func setActive(_ active: Bool) { activity.append(active) }
    func stop() { stopped = true; replies.finish() }
}

@MainActor
final class RuntimeCoordinatorTests: XCTestCase {
    private let ada = SignedInAccount(id: AccountId("ada"), email: "ada@example.com")
    private let bo = SignedInAccount(id: AccountId("bo"), email: "bo@example.com")
    private var root: URL { FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString) }

    func testStartupWorkerFailureReleasesTheHandleAndRetryStartsFromCredentials() async throws {
        let diagnostic = "installation profile path disagrees with its namespace"
        for terminal in [
            [Event.invariant(detail: diagnostic), .connection(.init(state: .disconnected, reason: .stopped))],
            [.invariant(detail: diagnostic)],
            [.connection(.init(state: .disconnected, reason: .stopped))],
        ] {
            let directory = root
            defer { try? FileManager.default.removeItem(at: directory) }
            let registry = AccountRegistry()
            registry.add(ada)
            let stores = try XCTUnwrap(registry.stores)
            let cloud = RelayCloud()
            var clients: [ScriptedRuntime] = []
            var configurations: [BridgeConfiguration] = []
            let coordinator = RuntimeCoordinator(
                registry: registry, cloud: cloud, support: directory, cache: directory, deviceName: "Phone",
                factory: { config, _ in
                    configurations.append(config)
                    let client = ScriptedRuntime()
                    clients.append(client)
                    if clients.count == 1 {
                        client.replies.yield([Made.fleet([
                            Made.card(1, name: "Remembered agent", minutesAgo: 1, now: Date()),
                        ], reconciled: false)])
                        client.replies.yield(terminal)
                    }
                    return client
                })
            defer { coordinator.stop() }
            _ = await coordinator.reconnect()
            for _ in 0..<1000 where coordinator.failure == nil { await Task.yield() }
            XCTAssertNil(coordinator.runtime, "a dead worker must report started=false")
            XCTAssertNil(coordinator.runtimeAccount)
            XCTAssertTrue(try XCTUnwrap(clients.first).stopped)
            XCTAssertEqual(coordinator.failure, terminal.count == 1 && terminal.first == .connection(
                .init(state: .disconnected, reason: .stopped)) ? "The mobile runtime stopped" : diagnostic)
            XCTAssertEqual(stores.fleet.rows.map(\.name), ["Remembered agent"])
            XCTAssertFalse(stores.fleet.reconciled)
            XCTAssertEqual(stores.fleet.exceptions, "Offline · amux could not start")
            XCTAssertEqual(stores.hosts.connection.reason, .stopped)
            XCTAssertNil(stores.watch)
            XCTAssertNil(stores.store)

            await cloud.use("retry.example")
            let callsBefore = await cloud.calls.count
            XCTAssertNotNil(stores.dispatch?(.retryNow))
            for _ in 0..<1000 where clients.count < 2 { await Task.yield() }
            XCTAssertEqual(clients.count, 2)
            XCTAssertTrue(coordinator.runtime === clients.last)
            XCTAssertEqual(configurations.last?.relay.url, "https://retry.example:443")
            let callsAfter = await cloud.calls.count
            XCTAssertGreaterThan(callsAfter, callsBefore)
            XCTAssertFalse(clients[0].commands.contains(.retryNow))
            XCTAssertNil(coordinator.failure)
            clients.last?.replies.yield([.connection(.init(state: .connected))])
            for _ in 0..<1000 where stores.fleet.connection.state != .connected { await Task.yield() }
            XCTAssertNil(stores.fleet.exceptions)
        }
    }

    func testAnInitializedWorkerKeepsTransportRetryButStoppedRemovesIt() async throws {
        let directory = root
        defer { try? FileManager.default.removeItem(at: directory) }
        let registry = AccountRegistry()
        registry.add(ada)
        let client = ScriptedRuntime()
        let coordinator = RuntimeCoordinator(
            registry: registry, cloud: RelayCloud(), support: directory, cache: directory,
            deviceName: "Phone", factory: { _, _ in client })
        defer { coordinator.stop() }
        _ = await coordinator.reconnect()
        client.replies.yield([
            .connection(.init(state: .connected)),
            .invariant(detail: "fleet cache write failed"),
            .connection(.init(state: .disconnected, reason: .unreachable)),
        ])
        for _ in 0..<1000 where registry.stores?.fleet.connection.reason != .unreachable { await Task.yield() }
        XCTAssertTrue(coordinator.runtime === client)
        XCTAssertNil(coordinator.failure)
        XCTAssertNotNil(registry.stores?.dispatch?(.retryNow))
        XCTAssertEqual(client.commands.last, .retryNow)
        client.replies.yield([.connection(.init(state: .disconnected, reason: .stopped))])
        for _ in 0..<1000 where coordinator.runtime != nil { await Task.yield() }
        XCTAssertNil(coordinator.runtime)
        XCTAssertEqual(coordinator.failure, "fleet cache write failed")
    }

    func testServiceChoosesRelayCallbacksAnswerItAndSwitchCreditsEachSideOfTheBatch() async throws {
        let directory = root
        defer { try? FileManager.default.removeItem(at: directory) }
        let registry = AccountRegistry()
        registry.add(ada)
        registry.add(bo)
        let cloud = RelayCloud()
        let client = ScriptedRuntime()
        var configurations: [BridgeConfiguration] = []
        var answer: RuntimeCoordinator.TokenProvider?
        let coordinator = RuntimeCoordinator(
            registry: registry, cloud: cloud, support: directory, cache: directory,
            deviceName: "Ada's iPhone", factory: { config, provider in
                configurations.append(config)
                answer = provider
                return client
            })
        defer { coordinator.stop() }
        let connected = await coordinator.reconnect()
        XCTAssertTrue(connected)
        let configuration = try XCTUnwrap(configurations.first)
        XCTAssertEqual(configuration.relay, .init(url: "https://relay.example:443", tls: .system))
        XCTAssertEqual(configuration.accounts.map(\.token), [.callback, .callback])
        XCTAssertEqual(configuration.data_dir, directory.path)
        XCTAssertEqual(configuration.device_name, "Ada's iPhone")
        let renewed = await answer?(1, "bo")
        XCTAssertEqual(renewed?.bearer, "token-bo")

        let oldStores = try XCTUnwrap(registry.stores)
        let agent = Made.card(1, name: "Ada's agent", minutesAgo: 1, now: Date()).agent.id
        oldStores.openConversation(agent)
        oldStores.releaseStream(agent)
        XCTAssertEqual(client.commands, [.subscribe(agent: agent), .unsubscribe(agent: agent)])
        _ = await coordinator.reconnect()
        XCTAssertEqual(client.commands, [.subscribe(agent: agent), .unsubscribe(agent: agent)],
                       "reconnecting must not resume a conversation somebody left")
        registry.select(bo.id)
        let switched = await coordinator.reconnect()
        XCTAssertTrue(switched)
        XCTAssertEqual(configurations.count, 1)
        XCTAssertEqual(client.commands.last, .selectAccount("bo"))
        XCTAssertNil(oldStores.dispatch)
        XCTAssertNil(registry.stores?.dispatch, "a switch must be acknowledged before the new account sends")
        client.replies.yield([
            Made.fleet([Made.card(1, name: "Ada's late row", minutesAgo: 1, now: Date())], reconciled: true),
            .opResult(.init(op: OpId(UUID().uuidString)!, outcome: .selected(account: "bo"))),
            Made.fleet([Made.card(2, name: "Bo's row", minutesAgo: 1, now: Date())], reconciled: true),
        ])
        for _ in 0..<100 where registry.stores?.fleet.rows.isEmpty == true { await Task.yield() }
        XCTAssertEqual(registry.stores?.fleet.rows.map(\.name), ["Bo's row"])
        XCTAssertGreaterThan(registry.dropped, 0)
        XCTAssertNotNil(registry.stores?.dispatch)
        coordinator.setActive(false)
        coordinator.setActive(true)
        XCTAssertEqual(client.activity, [true, false, true])
        registry.signOut(bo.id)
        XCTAssertTrue(client.stopped)
        XCTAssertNil(coordinator.runtime)
    }

    func testANewEntitlementStartsThePreviouslyRefusedConnection() async {
        let directory = root
        defer { try? FileManager.default.removeItem(at: directory) }
        let registry = AccountRegistry()
        registry.add(ada)
        let cloud = RelayCloud()
        await cloud.permit(false)
        let coordinator = RuntimeCoordinator(
            registry: registry, cloud: cloud, support: directory, cache: directory, deviceName: "Phone",
            factory: { _, _ in ScriptedRuntime() })
        defer { coordinator.stop() }
        let refused = await coordinator.reconnect()
        XCTAssertFalse(refused)
        XCTAssertNil(coordinator.runtime)
        await cloud.permit(true)
        registry.entitlement(.active(grant: .granted, renews: nil), for: ada.id)
        for _ in 0..<1000 where coordinator.runtime == nil { await Task.yield() }
        XCTAssertNotNil(coordinator.runtime)
        XCTAssertEqual(registry.gate, .ready)
    }

    func testDifferentRelayRestartsAndPlaintextRequiresExplicitLoopbackPermission() async throws {
        let directory = root
        defer { try? FileManager.default.removeItem(at: directory) }
        let registry = AccountRegistry()
        registry.add(ada)
        registry.add(bo)
        let cloud = RelayCloud()
        var clients: [ScriptedRuntime] = []
        var configurations: [BridgeConfiguration] = []
        let coordinator = RuntimeCoordinator(
            registry: registry, cloud: cloud, support: directory, cache: directory, deviceName: "Phone",
            factory: { config, _ in
                configurations.append(config)
                let client = ScriptedRuntime()
                clients.append(client)
                return client
            })
        defer { coordinator.stop() }
        _ = await coordinator.reconnect()
        await cloud.use("other.example")
        registry.select(bo.id)
        _ = await coordinator.reconnect()
        XCTAssertEqual(clients.count, 2)
        XCTAssertTrue(clients[0].stopped)
        XCTAssertEqual(configurations.last?.relay.url, "https://other.example:443")
        let refused = await coordinator.override(relay: URL(string: "http://127.0.0.1:8080")!, tokens: [:])
        XCTAssertFalse(refused)
        XCTAssertNil(coordinator.runtime)
        XCTAssertEqual(registry.stores?.hosts.connection.state, .disconnected)
    }

    func testDebugLoopbackUsesTheCredentialsAddressWithTheSameInstallation() async {
        let directory = root
        defer { try? FileManager.default.removeItem(at: directory) }
        let registry = AccountRegistry()
        registry.add(ada)
        let cloud = RelayCloud()
        await cloud.use("127.0.0.1")
        var configuration: BridgeConfiguration?
        let coordinator = RuntimeCoordinator(
            registry: registry, cloud: cloud, support: directory, cache: directory, deviceName: "Phone",
            allowPlainLoopback: true, factory: { config, _ in
                configuration = config
                return ScriptedRuntime()
            })
        defer { coordinator.stop() }
        _ = await coordinator.reconnect()
        XCTAssertEqual(configuration?.relay, .init(url: "http://127.0.0.1:443", tls: .plainLoopback))
        let refused = await coordinator.override(relay: URL(string: "http://remote.example:8080")!, tokens: [:])
        XCTAssertFalse(refused)
    }
}
