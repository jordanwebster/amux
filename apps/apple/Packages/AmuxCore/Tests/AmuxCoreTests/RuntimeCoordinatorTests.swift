import Foundation
import Network
import XCTest

@testable import AmuxCore

/// The shared runtime itself, started in this process on a scratch
/// directory: what the app does at every launch, with no machine to reach.
/// Signing in binds against an account service served on loopback, which
/// knows every account and signs its tokens in plain text.
@MainActor
final class RuntimeCoordinatorTests: XCTestCase {
    private var support: URL!

    override func setUp() async throws {
        support = FileManager.default.temporaryDirectory
            .appendingPathComponent("runtime-\(UUID().uuidString)", isDirectory: true)
    }

    override func tearDown() async throws {
        try? FileManager.default.removeItem(at: support)
    }

    private func coordinator(_ registry: AccountRegistry) -> RuntimeCoordinator {
        RuntimeCoordinator(
            registry: registry, support: support, deviceName: "Test Phone",
            signedOut: StoreBundle(account: AccountId("signed-out")),
            options: .init(discoveryScope: "unit-\(UUID().uuidString)", lanBind: "127.0.0.1:0"))
    }

    private func eventually(
        _ what: String, _ check: @MainActor () -> Bool, file: StaticString = #filePath,
        line: UInt = #line
    ) async {
        let deadline = Date().addingTimeInterval(30)
        while !check() {
            if Date() > deadline {
                XCTFail("never saw \(what)", file: file, line: line)
                return
            }
            try? await Task.sleep(for: .milliseconds(20))
        }
    }

    private func sign(
        _ coordinator: RuntimeCoordinator, in account: String, at service: FakeAccountService,
        file: StaticString = #filePath, line: UInt = #line
    ) async {
        let bound = await coordinator.bind(
            AccountId(account), cloud: service.url, client: "mobile",
            refreshToken: "refresh-\(account)")
        if case .failure(let why) = bound {
            XCTFail("\(account) did not sign in: \(why)", file: file, line: line)
        }
    }

    private func binding(_ coordinator: RuntimeCoordinator, _ profile: String?) -> AccountBinding? {
        coordinator.profiles.first { $0.id == profile }?.account.binding
    }

    func testTheSignedOutPhoneStartsItsOwnProfileAndListsItself() async {
        let registry = AccountRegistry()
        let coordinator = coordinator(registry)
        defer { coordinator.stop() }
        coordinator.start()
        let runtime = await coordinator.started()
        XCTAssertNotNil(runtime)
        XCTAssertNil(coordinator.storeFailure)
        XCTAssertEqual(coordinator.profiles.count, 1)
        XCTAssertEqual(coordinator.profile?.id, registry.unbound)
        let stores = coordinator.stores
        XCTAssertTrue(stores === coordinator.signedOutStores)
        await eventually("this phone listed") { stores.hosts.local != nil }
        XCTAssertEqual(stores.hosts.local?.name, "Test Phone")
        XCTAssertTrue(stores.fleet.rows.isEmpty)
        await eventually("the account read") { stores.hosts.account != nil }
        XCTAssertEqual(stores.hosts.account?.binding, .unbound)
        await eventually("the roster read") { stores.hosts.roster != nil }
        XCTAssertEqual(stores.hosts.roster?.identity.name, "Test Phone")
        XCTAssertEqual(stores.hosts.roster?.identity.fingerprint.count, 64)
        XCTAssertTrue(stores.hosts.devices.isEmpty)
        XCTAssertTrue(FileManager.default.fileExists(atPath: coordinator.directory.path))
    }

    func testALeftoverDirectoryOfPerAccountInstallationsIsDeletedAtStart() async throws {
        let leftover = support.appendingPathComponent("installations/old", isDirectory: true)
        try FileManager.default.createDirectory(at: leftover, withIntermediateDirectories: true)
        let coordinator = coordinator(AccountRegistry())
        defer { coordinator.stop() }
        coordinator.start()
        let started = await coordinator.started()
        XCTAssertNotNil(started)
        XCTAssertFalse(FileManager.default.fileExists(
            atPath: support.appendingPathComponent("installations").path))
    }

    func testTheRuntimeLendsNoBearerBeforeAnySignIn() async {
        let registry = AccountRegistry()
        let coordinator = coordinator(registry)
        defer { coordinator.stop() }
        coordinator.start()
        guard let runtime = await coordinator.started(), let profile = coordinator.profile else {
            return XCTFail("the runtime did not start")
        }
        guard case .failure = await runtime.accessToken(profile.id) else {
            return XCTFail("a bearer was lent for no account")
        }
        runtime.discovered([FoundHost(
            host: HostId(UUID()), name: "elsewhere", version: 1, addrs: ["127.0.0.1:9"],
            scope: "another-scope").found])
        guard case .success(nil) = await runtime.trusting(HostId(UUID())) else {
            return XCTFail("a profile trusts a machine nobody paired")
        }
    }

    /// Put away, only the chats a push opens keep a source; in front of
    /// somebody, every listed agent does.
    func testBackgroundAndForegroundSwitchTheSourcePolicy() async {
        let registry = AccountRegistry()
        let coordinator = coordinator(registry)
        defer { coordinator.stop() }
        coordinator.start()
        guard await coordinator.started() != nil, let profile = coordinator.profile else {
            return XCTFail("the runtime did not start")
        }
        XCTAssertTrue(profile.listsSources)
        coordinator.setActive(false)
        XCTAssertFalse(profile.listsSources)
        coordinator.setActive(true)
        XCTAssertTrue(profile.listsSources)
    }

    func testAPushInTheBackgroundWarmsItsChatUnderTheOnDemandPolicy() async {
        let registry = AccountRegistry()
        let coordinator = coordinator(registry)
        defer { coordinator.stop() }
        coordinator.start()
        guard await coordinator.started() != nil, let profile = coordinator.profile else {
            return XCTFail("the runtime did not start")
        }
        XCTAssertTrue(profile.listsSources)
        // No machine is paired, so the named chat never catches up; what
        // matters is the policy it was opened under and that it is closed.
        let agent = AgentKey(host: HostId(UUID()), agent: UUID())
        let current = await coordinator.warm(agent, inBackground: true, within: .milliseconds(300))
        XCTAssertFalse(current)
        XCTAssertFalse(profile.listsSources, "a background wake opens only the chat it names")
        coordinator.setActive(true)
        XCTAssertTrue(profile.listsSources, "the foreground lists every agent again")
    }

    func testTheFirstSignInTakesOverTheProfileNobodySignedInOn() async throws {
        let service = try await FakeAccountService.start()
        defer { service.stop() }
        let registry = AccountRegistry()
        let coordinator = coordinator(registry)
        defer { coordinator.stop() }
        coordinator.start()
        guard let runtime = await coordinator.started() else {
            return XCTFail("the runtime did not start")
        }
        let before = coordinator.profile?.id
        await sign(coordinator, in: "ada", at: service)
        XCTAssertEqual(registry.selected, AccountId("ada"))
        XCTAssertEqual(registry.profile, before, "ada keeps what the phone paired before")
        XCTAssertEqual(coordinator.profiles.count, 1)
        XCTAssertNil(registry.unbound)
        XCTAssertEqual(coordinator.profile?.id, before)
        XCTAssertTrue(coordinator.stores === registry.stores)
        let stores = coordinator.stores
        await eventually("ada's stores fed") { stores.hosts.local != nil }
        await eventually("ada's account read") { stores.hosts.account?.binding == .signedIn }
        XCTAssertEqual(stores.hosts.account?.email, "ada@example.com")
        guard case .success(let bearer) = await runtime.accessToken(before ?? "") else {
            return XCTFail("ada's profile lends no bearer")
        }
        XCTAssertEqual(bearer.bearer, "access-ada")
        let lent = await coordinator.bearer(for: AccountId("ada"))
        XCTAssertEqual(lent, "access-ada")
    }

    /// A second account is a profile of its own and goes on screen; the
    /// first is paused, so one relay link is live. Switching back pauses
    /// and resumes without the runtime stopping, and each account's fleet is
    /// its own.
    func testASecondAccountGetsItsOwnProfileAndSwitchingPausesTheOther() async throws {
        let service = try await FakeAccountService.start()
        defer { service.stop() }
        let registry = AccountRegistry()
        let coordinator = coordinator(registry)
        defer { coordinator.stop() }
        coordinator.start()
        guard let runtime = await coordinator.started() else {
            return XCTFail("the runtime did not start")
        }
        await sign(coordinator, in: "ada", at: service)
        let ada = registry.profile
        let adaStores = coordinator.stores
        await eventually("ada's phone listed") { adaStores.hosts.local != nil }
        let adaHost = adaStores.hosts.local?.hostId

        await sign(coordinator, in: "bob", at: service)
        XCTAssertEqual(registry.selected, AccountId("bob"))
        let bob = registry.profile
        XCTAssertNotNil(bob)
        XCTAssertNotEqual(bob, ada)
        XCTAssertEqual(coordinator.profile?.id, bob)
        XCTAssertEqual(registry.accounts.map(\.profile).sorted(), [ada, bob].compactMap { $0 }.sorted())
        await eventually("ada paused") { binding(coordinator, ada) == .paused }
        XCTAssertEqual(binding(coordinator, bob), .signedIn)
        XCTAssertTrue(runtime.listsSources(bob ?? ""))
        XCTAssertFalse(runtime.listsSources(ada ?? ""), "only the profile on screen lists")
        let bobStores = coordinator.stores
        await eventually("bob's phone listed") { bobStores.hosts.local != nil }
        XCTAssertNotEqual(bobStores.hosts.local?.hostId, adaHost, "each account's fleet is its own")

        registry.select(AccountId("ada"))
        XCTAssertTrue(coordinator.runtime === runtime, "switching never restarts the runtime")
        XCTAssertEqual(coordinator.profile?.id, ada)
        await eventually("ada resumed") { binding(coordinator, ada) == .signedIn }
        await eventually("bob paused") { binding(coordinator, bob) == .paused }
        let shown = coordinator.stores
        await eventually("ada's fleet shown again") { shown.hosts.local?.hostId == adaHost }
    }

    func testSigningOutKeepsTheProfileTiedToItsAccount() async throws {
        let service = try await FakeAccountService.start()
        defer { service.stop() }
        let registry = AccountRegistry()
        let coordinator = coordinator(registry)
        defer { coordinator.stop() }
        coordinator.start()
        _ = await coordinator.started()
        await sign(coordinator, in: "ada", at: service)
        let ada = registry.profile
        await coordinator.signOut(AccountId("ada"))
        XCTAssertEqual(registry.selected, AccountId("ada"), "it stays on screen")
        XCTAssertEqual(registry.selectedAccount?.signedIn, false)
        XCTAssertEqual(registry.selectedAccount?.account.email, "ada@example.com")
        XCTAssertEqual(binding(coordinator, ada), .signedOut)
        await sign(coordinator, in: "ada", at: service)
        XCTAssertEqual(registry.profile, ada, "signing back in finds the same profile")
        XCTAssertEqual(registry.selectedAccount?.signedIn, true)
    }

    func testRemovingTheOnlyAccountLeavesAFreshProfileNobodySignedInOn() async throws {
        let service = try await FakeAccountService.start()
        defer { service.stop() }
        let registry = AccountRegistry()
        let coordinator = coordinator(registry)
        defer { coordinator.stop() }
        coordinator.start()
        _ = await coordinator.started()
        await sign(coordinator, in: "ada", at: service)
        let ada = registry.profile
        await coordinator.remove(AccountId("ada"))
        XCTAssertNil(registry.selected)
        XCTAssertTrue(registry.accounts.isEmpty)
        XCTAssertEqual(coordinator.profiles.count, 1)
        XCTAssertNotNil(registry.unbound)
        XCTAssertNotEqual(registry.unbound, ada)
        XCTAssertEqual(coordinator.profile?.id, registry.unbound)
        XCTAssertTrue(coordinator.stores === coordinator.signedOutStores)
    }

    func testTheAccountOnScreenSurvivesARelaunch() async throws {
        let service = try await FakeAccountService.start()
        defer { service.stop() }
        let file = support.appendingPathComponent("accounts.json")
        let first = AccountRegistry(file: file)
        let coordinator = coordinator(first)
        coordinator.start()
        _ = await coordinator.started()
        await sign(coordinator, in: "ada", at: service)
        await sign(coordinator, in: "bob", at: service)
        first.select(AccountId("ada"))
        let ada = first.profile
        coordinator.stop()

        let again = AccountRegistry(file: file)
        let relaunched = self.coordinator(again)
        defer { relaunched.stop() }
        relaunched.start()
        _ = await relaunched.started()
        XCTAssertEqual(again.selected, AccountId("ada"))
        XCTAssertEqual(again.profile, ada)
        XCTAssertEqual(relaunched.profile?.id, ada)
        XCTAssertEqual(Set(again.accounts.map(\.id)), [AccountId("ada"), AccountId("bob")])
        await eventually("bob paused, ada live") {
            binding(relaunched, ada) == .signedIn
                && relaunched.profiles.filter { $0.account.binding == .paused }.count == 1
        }
    }

    func testAStoreThatCannotOpenStopsTheApp() async {
        let registry = AccountRegistry()
        let coordinator = RuntimeCoordinator(
            registry: registry, support: support, deviceName: "Test Phone",
            signedOut: StoreBundle(account: AccountId("signed-out")),
            starter: { _, _ throws(RuntimeFailure) in throw RuntimeFailure("the store is damaged") })
        var told: [String?] = []
        coordinator.storeFailureChanged = { told.append($0) }
        coordinator.start()
        _ = await coordinator.started()
        XCTAssertEqual(coordinator.storeFailure, "the store is damaged")
        XCTAssertEqual(told, ["the store is damaged"])
    }
}

/// An account service on loopback that knows every account: a refresh
/// token `refresh-<name>` redeems for the access token `access-<name>`,
/// which names that account. Anything else it is asked is not found.
final class FakeAccountService: @unchecked Sendable {
    let url: URL
    private let listener: NWListener
    private static let queue = DispatchQueue(label: "fake-account-service")

    private init(url: URL, listener: NWListener) {
        self.url = url
        self.listener = listener
    }

    static func start() async throws -> FakeAccountService {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        let listener = try NWListener(using: parameters)
        listener.newConnectionHandler = { connection in
            connection.start(queue: queue)
            read(connection, Data())
        }
        let port: UInt16 = try await withCheckedThrowingContinuation { continuation in
            let once = Once()
            listener.stateUpdateHandler = { state in
                switch state {
                case .ready: once.run { continuation.resume(returning: listener.port?.rawValue ?? 0) }
                case .failed(let error): once.run { continuation.resume(throwing: error) }
                default: break
                }
            }
            listener.start(queue: queue)
        }
        return FakeAccountService(url: URL(string: "http://127.0.0.1:\(port)")!, listener: listener)
    }

    func stop() { listener.cancel() }

    /// Reads one request whole, answers it and closes.
    private static func read(_ connection: NWConnection, _ held: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { data, _, done, error in
            var held = held
            if let data { held.append(data) }
            if let request = Request(held) {
                answer(request, on: connection)
            } else if done || error != nil {
                connection.cancel()
            } else {
                read(connection, held)
            }
        }
    }

    private static func answer(_ request: Request, on connection: NWConnection) {
        let (status, body): (Int, String)
        switch request.path {
        case "/connect/token":
            let token = request.body.split(separator: "&")
                .first { $0.hasPrefix("refresh_token=") }?
                .dropFirst("refresh_token=".count)
            if let name = token.flatMap({ $0.hasPrefix("refresh-") ? $0.dropFirst(8) : nil }) {
                (status, body) = (200, """
                    {"access_token":"access-\(name)","token_type":"bearer","expires_in":3600,\
                    "refresh_token":"refresh-\(name)"}
                    """)
            } else {
                (status, body) = (400, #"{"error":"invalid_grant"}"#)
            }
        case "/connect/userinfo":
            if let bearer = request.headers["authorization"], bearer.hasPrefix("Bearer access-") {
                let name = bearer.dropFirst("Bearer access-".count)
                (status, body) = (200, """
                    {"sub":"\(name)","name":"\(name)","email":"\(name)@example.com"}
                    """)
            } else {
                (status, body) = (401, "{}")
            }
        default:
            (status, body) = (404, "{}")
        }
        let response = "HTTP/1.1 \(status) Answer\r\nContent-Type: application/json\r\n"
            + "Content-Length: \(body.utf8.count)\r\nConnection: close\r\n\r\n\(body)"
        connection.send(content: Data(response.utf8), completion: .contentProcessed { _ in
            connection.cancel()
        })
    }

    /// One HTTP request, once all of it has arrived.
    private struct Request {
        var path: String
        var headers: [String: String]
        var body: String

        init?(_ data: Data) {
            guard let end = data.range(of: Data("\r\n\r\n".utf8)) else { return nil }
            let head = String(decoding: data[..<end.lowerBound], as: UTF8.self)
            var lines = head.components(separatedBy: "\r\n")
            let start = lines.removeFirst().split(separator: " ")
            guard start.count >= 2 else { return nil }
            path = String(start[1].split(separator: "?").first ?? "")
            headers = [:]
            for line in lines {
                guard let colon = line.firstIndex(of: ":") else { continue }
                headers[line[..<colon].lowercased()] =
                    line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            }
            let length = Int(headers["content-length"] ?? "0") ?? 0
            let rest = data[end.upperBound...]
            guard rest.count >= length else { return nil }
            body = String(decoding: rest.prefix(length), as: UTF8.self)
        }
    }

    private final class Once: @unchecked Sendable {
        private let lock = NSLock()
        private var done = false

        func run(_ body: () -> Void) {
            let first = lock.withLock {
                defer { done = true }
                return !done
            }
            if first { body() }
        }
    }
}
