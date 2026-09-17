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
    func keepSession(_ id: AccountId) async throws(CloudError) {}
    func forgetSession(_ id: AccountId) async throws {}
    func signIn(_ intent: SignInIntent, presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount {
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
    var handedOver: [[FoundHost]] = []
    var stopped = false
    init() { (events, replies) = AsyncStream.makeStream() }
    func dispatch(_ command: BridgeCommand) -> OpId? {
        commands.append(command)
        return OpId(UUID().uuidString)
    }
    func attach(_ picked: PickedAttachment, bytes: Data) -> OpId? { OpId(UUID().uuidString) }
    func discovered(_ hosts: [FoundHost]) { handedOver.append(hosts) }
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
            XCTAssertEqual(configurations.last?.relay?.url, "https://retry.example:443")
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
        XCTAssertEqual(configurations.last?.relay?.url, "https://other.example:443")
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

    func testARuntimeStartedLaterIsToldWhatTheBrowserAlreadySaw() async {
        // Signing in builds a new runtime. The machines on this network did
        // not go anywhere when it did, and waiting for the browser to notice
        // them a second time would show an empty network meanwhile.
        let directory = root
        defer { try? FileManager.default.removeItem(at: directory) }
        let registry = AccountRegistry()
        registry.add(ada)
        var clients: [ScriptedRuntime] = []
        let coordinator = RuntimeCoordinator(
            registry: registry, cloud: RelayCloud(), support: directory, cache: directory,
            deviceName: "Phone",
            factory: { _, _ in
                let client = ScriptedRuntime()
                clients.append(client)
                return client
            })
        defer { coordinator.stop() }

        let kitchen = FoundHost(
            host: HostId(UUID(uuidString: "6D7A4B1E-3C2F-4A58-9B0D-1E2F3A4B5C6D")!),
            name: "kitchen", version: 7, addrs: ["192.168.1.24:41234"])
        coordinator.discovered([kitchen])
        _ = await coordinator.reconnect()
        XCTAssertEqual(clients.last?.handedOver, [[kitchen]])

        // And the live one hears every later set, the empty one included.
        coordinator.discovered([])
        XCTAssertEqual(clients.last?.handedOver, [[kitchen], []])
    }

    func testAPhoneWithNobodySignedInStillRunsAndIsNotKilledBySayingItHasNoRelay() async throws {
        // A phone without an account reaches no relay and answers for no
        // account, and still finds and reaches the machines on its own
        // network. A runtime with no relay reports that it is not on one,
        // which reads on the wire exactly like a worker that stopped; only a
        // runtime that was given a relay can have stopped reaching it.
        let directory = root
        defer { try? FileManager.default.removeItem(at: directory) }
        let registry = AccountRegistry()
        let signedOut = StoreBundle(account: AccountId("signed-out"))
        var configurations: [BridgeConfiguration] = []
        let client = ScriptedRuntime()
        let coordinator = RuntimeCoordinator(
            registry: registry, cloud: RelayCloud(), support: directory, cache: directory,
            deviceName: "Phone", factory: { config, _ in
                configurations.append(config)
                return client
            })
        coordinator.signedOutStores = signedOut
        defer { coordinator.stop() }
        _ = await coordinator.reconnect()

        XCTAssertTrue(coordinator.runtime === client, "a phone with no account started no runtime")
        XCTAssertNil(coordinator.runtimeAccount)
        XCTAssertNil(configurations.last?.relay)
        XCTAssertEqual(configurations.last?.accounts, [])
        XCTAssertNil(configurations.last?.active)

        client.replies.yield([.connection(.init(state: .disconnected, reason: .stopped))])
        let kitchen = HostEntry(id: HostId(UUID()), name: "kitchen", online: false,
                                trustStatus: .untrustedButOnline)
        client.replies.yield([.discovered([kitchen])])
        for _ in 0..<1000 where signedOut.hosts.discovered.isEmpty { await Task.yield() }
        XCTAssertTrue(coordinator.runtime === client,
                      "a runtime that never had a relay was taken for a dead one")
        XCTAssertNil(coordinator.failure)
        XCTAssertEqual(signedOut.hosts.discovered.map(\.name), ["kitchen"],
                       "what a signed-out runtime says reached no screen")

        // And what the system says about the network reaches the same screen.
        coordinator.localNetwork(.denied)
        XCTAssertEqual(signedOut.hosts.localNetwork, .denied)
    }

    /// A removed account is named to the next runtime, which is the only
    /// thing that can delete its profile — even when removing it changed
    /// nothing about which accounts are signed in — and it keeps being named
    /// until a runtime says that account is really gone from the device. A
    /// runtime that merely started is not that word: a profile whose key or
    /// caches would not delete is still here.
    func testARemovedAccountIsNamedUntilARuntimeSaysItIsGone() async throws {
        let directory = root
        defer { try? FileManager.default.removeItem(at: directory) }
        let registry = AccountRegistry()
        registry.add(ada)
        registry.add(bo)
        registry.signOut(bo.id)
        var clients: [ScriptedRuntime] = []
        var configurations: [BridgeConfiguration] = []
        let coordinator = RuntimeCoordinator(
            registry: registry, cloud: RelayCloud(), support: directory, cache: directory,
            deviceName: "Phone", factory: { config, _ in
                configurations.append(config)
                let client = ScriptedRuntime()
                clients.append(client)
                return client
            })
        defer { coordinator.stop() }
        _ = await coordinator.reconnect()
        XCTAssertEqual(configurations.last?.forget, [])

        // Signed out and not on screen: the signed-in accounts are the same
        // before and after, and a runtime starts anyway.
        registry.forget(bo.id)
        for _ in 0..<1000 where clients.count < 2 { await Task.yield() }
        XCTAssertEqual(clients.count, 2, "removing an account did not restart the runtime")
        XCTAssertEqual(configurations.last?.forget, ["bo"])
        XCTAssertEqual(configurations.last?.accounts.map(\.id), ["ada"])
        XCTAssertTrue(clients[0].stopped)

        // A runtime that has opened and connected has said nothing about what
        // it managed to delete, so the removal is still pending.
        clients.last?.replies.yield([.connection(.init(state: .connected))])
        for _ in 0..<200 { await Task.yield() }
        XCTAssertEqual(
            registry.forgotten, [bo.id],
            "a runtime that only connected was read as having deleted the profile")
        XCTAssertEqual(coordinator.deletedProfiles, [])

        // Nor does a report about some other account.
        clients.last?.replies.yield([.forgotten(accounts: ["ada"])])
        for _ in 0..<200 { await Task.yield() }
        XCTAssertEqual(registry.forgotten, [bo.id])
        XCTAssertEqual(coordinator.deletedProfiles, [])

        clients.last?.replies.yield([.forgotten(accounts: ["bo"])])
        for _ in 0..<1000 where !registry.forgotten.isEmpty { await Task.yield() }
        XCTAssertEqual(registry.forgotten, [])
        XCTAssertEqual(coordinator.deletedProfiles, ["bo"])
        // Having deleted it, nothing restarts over it.
        for _ in 0..<50 { await Task.yield() }
        XCTAssertEqual(clients.count, 2)
    }

    func testSigningOutWithASecondAccountSignedInStillLeavesAPhoneThatBrowsesAndDials() async throws {
        // An account is reached through the relay, and the relay address comes
        // with the credential of the account on screen. With nobody on screen
        // there is no such address, so a connection that still listed the other
        // signed-in account would name an account it has no route for and be
        // refused — leaving a phone that cannot even see its own network
        // because somebody else happens to be signed in on it.
        let directory = root
        defer { try? FileManager.default.removeItem(at: directory) }
        let registry = AccountRegistry()
        registry.add(ada)
        registry.add(bo)
        registry.select(bo.id)
        let signedOut = StoreBundle(account: AccountId("signed-out"))
        var clients: [ScriptedRuntime] = []
        var configurations: [BridgeConfiguration] = []
        let coordinator = RuntimeCoordinator(
            registry: registry, cloud: RelayCloud(), support: directory, cache: directory,
            deviceName: "Phone", factory: { config, _ in
                configurations.append(config)
                let client = ScriptedRuntime()
                clients.append(client)
                return client
            })
        coordinator.signedOutStores = signedOut
        defer { coordinator.stop() }
        let kitchen = FoundHost(
            host: HostId(UUID(uuidString: "6D7A4B1E-3C2F-4A58-9B0D-1E2F3A4B5C6D")!),
            name: "kitchen", version: 7, addrs: ["192.168.1.24:41234"])
        coordinator.discovered([kitchen])
        _ = await coordinator.reconnect()
        XCTAssertEqual(configurations.last?.accounts.map(\.id), ["ada", "bo"])
        XCTAssertNotNil(configurations.last?.relay)

        registry.signOut(bo.id)
        for _ in 0..<1000 where clients.count < 2 { await Task.yield() }
        XCTAssertEqual(clients.count, 2, "signing out left the phone without a runtime")
        XCTAssertTrue(coordinator.runtime === clients.last)
        XCTAssertNil(coordinator.runtimeAccount)
        XCTAssertNil(coordinator.failure)
        XCTAssertNil(configurations.last?.relay)
        XCTAssertEqual(configurations.last?.accounts, [],
                       "a phone with nobody on screen listed an account it has no relay for")
        XCTAssertEqual(configurations.last?.active, "bo",
                       "signing out must leave the last account's machines on screen")

        // It is a phone that works: it hears the network the browser already
        // found, and what it finds there reaches the signed-out screen.
        XCTAssertEqual(clients.last?.handedOver, [[kitchen]])
        let host = HostEntry(id: HostId(UUID()), name: "kitchen", online: false,
                             trustStatus: .untrustedButOnline)
        clients.last?.replies.yield([.connection(.init(state: .disconnected, reason: .stopped)),
                                     .discovered([host])])
        for _ in 0..<1000 where signedOut.hosts.discovered.isEmpty { await Task.yield() }
        XCTAssertEqual(signedOut.hosts.discovered.map(\.name), ["kitchen"])
        XCTAssertNil(coordinator.failure)

        // The same holds for picking an account out of the switcher that is
        // already signed out, with the other one still signed in behind it.
        registry.select(ada.id)
        for _ in 0..<1000 where clients.count < 3 { await Task.yield() }
        XCTAssertEqual(configurations.last?.accounts.map(\.id), ["ada"])
        XCTAssertNotNil(configurations.last?.relay)
        registry.select(bo.id)
        for _ in 0..<1000 where clients.count < 4 { await Task.yield() }
        XCTAssertEqual(clients.count, 4)
        XCTAssertTrue(coordinator.runtime === clients.last)
        XCTAssertNil(coordinator.failure)
        XCTAssertNil(configurations.last?.relay)
        XCTAssertEqual(configurations.last?.accounts, [])
        XCTAssertEqual(configurations.last?.active, "bo")
    }
}
