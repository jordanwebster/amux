import Foundation
import XCTest

@testable import AmuxCore

/// The shared runtime itself, started in this process on a scratch
/// directory: what the app does at every launch, with no machine to reach.
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

    func testTheSignedOutPhoneStartsItsOwnInstallationAndListsItself() async {
        let registry = AccountRegistry()
        let coordinator = coordinator(registry)
        defer { coordinator.stop() }
        coordinator.start()
        let runtime = await coordinator.started(registry.installation)
        XCTAssertNotNil(runtime)
        XCTAssertNil(coordinator.storeFailure)
        let stores = coordinator.stores
        await eventually("this phone listed") { stores.hosts.local != nil }
        XCTAssertEqual(stores.hosts.local?.name, "Test Phone")
        XCTAssertTrue(stores.fleet.rows.isEmpty)
        await eventually("the account read") { stores.hosts.account != nil }
        XCTAssertEqual(stores.hosts.account?.binding, .unbound)
        await eventually("the roster read") { stores.hosts.roster != nil }
        XCTAssertEqual(stores.hosts.roster?.identity.name, "Test Phone")
        XCTAssertEqual(stores.hosts.roster?.identity.fingerprint.count, 64)
        XCTAssertTrue(stores.hosts.devices.isEmpty)
        XCTAssertTrue(FileManager.default.fileExists(
            atPath: coordinator.directory(of: registry.installation).path))
    }

    func testTheRuntimeLendsNoBearerBeforeAnySignIn() async {
        let registry = AccountRegistry()
        let coordinator = coordinator(registry)
        defer { coordinator.stop() }
        coordinator.start()
        guard let runtime = await coordinator.started(registry.installation) else {
            return XCTFail("the runtime did not start")
        }
        guard case .failure = await runtime.accessToken() else {
            return XCTFail("a bearer was lent for no account")
        }
        runtime.discovered([FoundHost(
            host: HostId(UUID()), name: "elsewhere", version: 1, addrs: ["127.0.0.1:9"],
            scope: "another-scope").found])
        runtime.setSourcePolicy(listed: false)
        runtime.setSourcePolicy(listed: true)
    }

    /// Put away, only the chats a push opens keep a source; in front of
    /// somebody, every listed agent does.
    func testBackgroundAndForegroundSwitchTheSourcePolicy() async {
        let registry = AccountRegistry()
        let coordinator = coordinator(registry)
        defer { coordinator.stop() }
        coordinator.start()
        guard let runtime = await coordinator.started(registry.installation) else {
            return XCTFail("the runtime did not start")
        }
        XCTAssertTrue(runtime.listsSources)
        coordinator.setActive(false)
        XCTAssertFalse(runtime.listsSources)
        coordinator.setActive(true)
        XCTAssertTrue(runtime.listsSources)
    }

    func testSwitchingAccountRunsTheOtherInstallation() async {
        let registry = AccountRegistry()
        let coordinator = coordinator(registry)
        defer { coordinator.stop() }
        coordinator.start()
        let signedOut = registry.installation
        _ = await coordinator.started(signedOut)
        registry.add(
            SignedInAccount(id: AccountId("ada"), email: "ada@example.com"),
            installation: "ada-installation")
        let runtime = await coordinator.started("ada-installation")
        XCTAssertNotNil(runtime)
        XCTAssertEqual(coordinator.running, "ada-installation")
        XCTAssertTrue(coordinator.stores === registry.stores)
        await eventually("ada's stores fed") { registry.stores?.hosts.local != nil }
    }

    func testRemovingAnAccountDeletesItsInstallationOnceNothingRunsFromIt() async {
        let registry = AccountRegistry()
        let coordinator = coordinator(registry)
        defer { coordinator.stop() }
        registry.add(
            SignedInAccount(id: AccountId("ada"), email: "ada@example.com"),
            installation: "ada-installation")
        coordinator.start()
        _ = await coordinator.started("ada-installation")
        let directory = coordinator.directory(of: "ada-installation")
        XCTAssertTrue(FileManager.default.fileExists(atPath: directory.path))
        coordinator.delete(installation: "ada-installation")
        XCTAssertTrue(FileManager.default.fileExists(atPath: directory.path), "still running")
        let removed = registry.forget(AccountId("ada"))
        XCTAssertEqual(removed, "ada-installation")
        coordinator.delete(installation: "ada-installation")
        XCTAssertFalse(FileManager.default.fileExists(atPath: directory.path))
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
        _ = await coordinator.started(registry.installation)
        XCTAssertEqual(coordinator.storeFailure, "the store is damaged")
        XCTAssertEqual(told, ["the store is damaged"])
    }
}
