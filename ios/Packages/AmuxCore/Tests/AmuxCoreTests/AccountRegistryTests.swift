import Foundation
import XCTest
@testable import AmuxCore

@MainActor
final class AccountRegistryTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)
    private let ada = SignedInAccount(id: AccountId("ada"), email: "ada@example.com")
    private let bo = SignedInAccount(id: AccountId("bo"), email: "bo@example.com")

    func testAccountsGrantsAndSelectionSurviveLaunchWithoutSecrets() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("accounts.json")
        let registry = AccountRegistry(file: file)
        registry.add(ada, entitlement: .active(grant: .granted, renews: nil))
        registry.add(bo, entitlement: .lapsed(grant: .purchased(.web), endedAt: now))
        registry.select(bo.id)
        let restored = AccountRegistry(file: file)
        XCTAssertEqual(restored.accounts, registry.accounts)
        XCTAssertEqual(restored.selected, bo.id)
        XCTAssertEqual(restored.stores?.account, bo.id)
        XCTAssertFalse(registry.persistenceFailed)
        let saved = try String(contentsOf: file, encoding: .utf8)
        XCTAssertFalse(saved.contains("token"))

        restored.signOut(bo.id)
        let signedOut = AccountRegistry(file: file)
        XCTAssertEqual(signedOut.selected, bo.id)
        XCTAssertNil(signedOut.stores)
        XCTAssertEqual(signedOut.gate, .signedOut)
        signedOut.forget(bo.id)
        let remaining = AccountRegistry(file: file)
        XCTAssertEqual(remaining.accounts.map(\.id), [ada.id])
        XCTAssertEqual(remaining.selected, ada.id)
        XCTAssertEqual(remaining.gate, .ready)
    }

    func testSigningBackIntoSelectedAccountRecreatesItsStoresAndNotifiesRuntime() {
        let registry = AccountRegistry()
        registry.add(ada)
        registry.signOut(ada.id)
        var changes = 0
        registry.changed = { changes += 1 }
        registry.add(ada, entitlement: .active(grant: .granted, renews: nil))
        XCTAssertEqual(registry.stores?.account, ada.id)
        XCTAssertEqual(changes, 1)
    }

    func testTheFirstAccountAddedIsTheSelectedOne() {
        let registry = AccountRegistry()
        registry.add(ada)
        XCTAssertEqual(registry.selected, ada.id)
        XCTAssertEqual(registry.stores?.account, ada.id)
    }

    func testALateResultForADeselectedAccountIsDropped() {
        let registry = AccountRegistry()
        registry.add(ada)
        registry.add(bo)
        registry.select(bo.id)

        // Ada's connection answers a question that was asked before the
        // switch. Writing it now would show Ada's agents under Bo's name.
        let landed = registry.deliver(
            [Made.fleet([Made.card(1, name: "ada-agent", minutesAgo: 1, now: now)], reconciled: true)],
            for: ada.id)

        XCTAssertFalse(landed)
        XCTAssertEqual(registry.dropped, 1)
        XCTAssertEqual(registry.stores?.fleet.rows.count, 0)

        let mine = registry.deliver(
            [Made.fleet([Made.card(2, name: "bo-agent", minutesAgo: 1, now: now)], reconciled: true)],
            for: bo.id)
        XCTAssertTrue(mine)
        XCTAssertEqual(registry.stores?.fleet.rows.map(\.name), ["bo-agent"])
    }

    func testACloudAnswerForADeselectedAccountIsRefusedToo() {
        let registry = AccountRegistry()
        registry.add(ada)
        registry.add(bo)
        registry.select(bo.id)
        XCTAssertNil(registry.accept(Entitlement.active(grant: .purchased(.web), renews: nil), for: ada.id))
        XCTAssertEqual(registry.accept(Entitlement.none, for: bo.id), Entitlement.none)
        XCTAssertEqual(registry.dropped, 1)
    }

    func testSwitchingAccountsStartsFromThatAccountsOwnStores() {
        let registry = AccountRegistry()
        registry.add(ada)
        registry.deliver(
            [Made.fleet([Made.card(1, name: "ada-agent", minutesAgo: 1, now: now)], reconciled: true)],
            for: ada.id)
        XCTAssertEqual(registry.stores?.fleet.rows.count, 1)

        registry.add(bo)
        registry.select(bo.id)
        XCTAssertEqual(registry.stores?.account, bo.id)
        XCTAssertTrue(registry.stores?.fleet.rows.isEmpty == true)
    }

    func testASignedOutAccountStaysListedAndItsStoresGoAway() {
        let registry = AccountRegistry()
        registry.add(ada, entitlement: .active(grant: .purchased(.appStore), renews: nil))
        registry.signOut(ada.id)
        XCTAssertEqual(registry.accounts.map(\.id), [ada.id])
        XCTAssertFalse(registry.accounts[0].signedIn)
        XCTAssertEqual(registry.accounts[0].entitlement, .none)
        XCTAssertNil(registry.stores)
        XCTAssertFalse(registry.deliver([], for: ada.id))
    }
}
