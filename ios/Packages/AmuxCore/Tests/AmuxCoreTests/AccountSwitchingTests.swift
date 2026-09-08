import Foundation
import XCTest
@testable import AmuxCore

/// The account list as the switcher and the You page read it, proven without a
/// screen: what each row says, what switching does to the stores behind it,
/// and what a result that arrives after a switch is allowed to touch.
@MainActor
final class AccountSwitchingTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)
    private let ada = SignedInAccount(id: AccountId("ada"), email: "ada@example.com",
                                      displayName: "Ada")
    private let acme = SignedInAccount(id: AccountId("acme"), email: "ada@acme.example",
                                       displayName: "Acme")

    private func phone() -> AccountRegistry {
        let registry = AccountRegistry()
        registry.restore([
            AccountEntry(account: ada, entitlement: .active(source: .web, renews: nil), hosts: 3),
            AccountEntry(account: acme, entitlement: .active(source: .appStore, renews: nil)),
        ])
        return registry
    }

    func testEveryAccountThisPhoneKnowsIsListedWithWhatItKnowsAboutIt() {
        let registry = phone()

        XCTAssertEqual(registry.accounts.map(\.name), ["Ada", "Acme"])
        XCTAssertEqual(registry.selected, ada.id)
        // The account with a connection behind it counts its machines; the one
        // without says its address rather than claiming none.
        XCTAssertEqual(registry.accounts[0].line, "3 hosts")
        XCTAssertEqual(registry.accounts[1].line, "ada@acme.example")
    }

    func testOneHostIsOneHost() {
        let registry = AccountRegistry()
        registry.restore([AccountEntry(account: ada, hosts: 1)])
        XCTAssertEqual(registry.accounts[0].line, "1 host")
    }

    func testSwitchingAccountsPointsEverythingAtTheOtherOne() {
        let registry = phone()
        registry.select(acme.id)

        XCTAssertEqual(registry.selected, acme.id)
        XCTAssertEqual(registry.stores?.account, acme.id)
        XCTAssertEqual(registry.gate, .ready)
    }

    func testASignedOutAccountStaysListedAndOffersToSignBackIn() {
        let registry = phone()
        registry.signOut(ada.id)

        XCTAssertEqual(registry.accounts.map(\.id), [ada.id, acme.id])
        XCTAssertFalse(registry.accounts[0].signedIn)
        XCTAssertEqual(registry.accounts[0].line, "Signed out")
        // Nothing of a signed-out account stays on screen, and the home says
        // why the list it is showing cannot change.
        XCTAssertNil(registry.stores)
        XCTAssertEqual(registry.gate, .signedOut)
    }

    func testSigningOutOneAccountLeavesTheOtherAlone() {
        let registry = phone()
        registry.select(acme.id)
        registry.signOut(ada.id)

        XCTAssertEqual(registry.selected, acme.id)
        XCTAssertEqual(registry.stores?.account, acme.id)
        XCTAssertTrue(registry.accounts[1].signedIn)
    }

    func testAddingAnAccountFromEitherPlaceSelectsTheFirstOne() {
        let registry = AccountRegistry()
        registry.add(ada, entitlement: .active(source: .web, renews: nil))
        registry.add(acme)

        XCTAssertEqual(registry.accounts.map(\.id), [ada.id, acme.id])
        // The first account added is the one on screen; a second one is added
        // and waits to be switched to, because adding is not switching.
        XCTAssertEqual(registry.selected, ada.id)
    }

    func testNothingIsSaidAboutAnInactiveAccountUntilSomethingHasLooked() {
        let registry = phone()

        // The badge in the switcher renders from this and nothing else. No
        // connection to the other account means no count, and no count means
        // no badge — never a zero and never a guess.
        XCTAssertNil(registry.accounts[1].attention)

        registry.attention(2, for: acme.id)
        XCTAssertEqual(registry.accounts[1].attention, 2)

        // And it goes away again when whatever was watching says so.
        registry.attention(nil, for: acme.id)
        XCTAssertNil(registry.accounts[1].attention)
    }

    func testALateResultForTheAccountYouSwitchedAwayFromIsDropped() {
        let registry = phone()
        registry.select(acme.id)

        let landed = registry.deliver(
            [Made.fleet([Made.card(1, name: "ada-agent", minutesAgo: 1, now: now)],
                        reconciled: true)],
            for: ada.id)

        XCTAssertFalse(landed)
        XCTAssertEqual(registry.dropped, 1)
        XCTAssertTrue(registry.stores?.fleet.rows.isEmpty ?? false)
    }

    func testAnAccountsOwnHostCountComesFromItsOwnConnection() {
        let registry = phone()
        registry.select(acme.id)

        registry.deliver([Made.fleet([], hosts: [
            Made.hostEntry(Made.host, name: "studio"),
            Made.hostEntry(Made.other, name: "mini"),
        ], reconciled: true)], for: acme.id)

        // Counted from what the connection answered, which is why the row can
        // say it at all.
        XCTAssertEqual(registry.accounts[1].line, "2 hosts")
        // And the account nobody switched to is untouched by it.
        XCTAssertEqual(registry.accounts[0].line, "3 hosts")
    }
}
