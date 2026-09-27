import Foundation
import XCTest

@testable import AmuxCore

@MainActor
final class AccountRegistryTests: XCTestCase {
    private func account(_ name: String) -> SignedInAccount {
        SignedInAccount(id: AccountId(name), email: "\(name)@example.com", displayName: name)
    }

    private func file() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString).appendingPathComponent("accounts.json")
    }

    func testTheFirstSignInAdoptsTheSignedOutPhonesInstallation() {
        let registry = AccountRegistry()
        let signedOut = registry.installation
        XCTAssertNil(registry.selected)
        let ada = account("ada")
        XCTAssertEqual(registry.installation(for: ada.id), signedOut)
        registry.add(ada, installation: registry.installation(for: ada.id))
        XCTAssertEqual(registry.selected, ada.id)
        XCTAssertEqual(registry.installation, signedOut, "ada runs what the phone paired before")
        XCTAssertNotEqual(registry.signedOutInstallation, signedOut, "the phone gets a fresh one")
    }

    func testAnotherAccountGetsItsOwnInstallationAndGoesOnScreen() {
        let registry = AccountRegistry()
        let ada = account("ada")
        registry.add(ada, installation: registry.installation(for: ada.id))
        let bob = account("bob")
        let installation = registry.installation(for: bob.id)
        XCTAssertNotEqual(installation, registry.installation)
        var switched: [AccountId?] = []
        registry.switching = { switched.append($0) }
        registry.add(bob, installation: installation)
        XCTAssertEqual(registry.selected, bob.id)
        XCTAssertEqual(registry.installation, installation)
        XCTAssertEqual(switched, [bob.id])
        XCTAssertEqual(registry.stores?.account, bob.id)
    }

    func testSigningBackInKeepsTheAccountsInstallation() {
        let registry = AccountRegistry()
        let ada = account("ada")
        registry.add(ada, installation: registry.installation(for: ada.id))
        let installation = registry.installation
        registry.signOut(ada.id)
        XCTAssertEqual(registry.selectedAccount?.signedIn, false)
        XCTAssertEqual(registry.selected, ada.id, "a signed-out account stays on screen")
        XCTAssertEqual(registry.installation(for: ada.id), installation)
        registry.add(ada, installation: registry.installation(for: ada.id))
        XCTAssertEqual(registry.selectedAccount?.signedIn, true)
        XCTAssertEqual(registry.installation, installation)
    }

    func testForgettingTheAccountOnScreenMovesToASignedInOneAndAnswersItsInstallation() {
        let registry = AccountRegistry()
        let ada = account("ada")
        let bob = account("bob")
        let cid = account("cid")
        registry.add(ada, installation: "ada-dir")
        registry.add(bob, installation: "bob-dir")
        registry.add(cid, installation: "cid-dir")
        registry.signOut(ada.id)
        XCTAssertEqual(registry.forget(cid.id), "cid-dir")
        XCTAssertEqual(registry.selected, bob.id, "the first signed-in account")
        XCTAssertEqual(registry.forget(bob.id), "bob-dir")
        XCTAssertEqual(registry.selected, ada.id, "then any account")
        XCTAssertEqual(registry.forget(ada.id), "ada-dir")
        XCTAssertNil(registry.selected, "then the signed-out phone")
        XCTAssertNil(registry.stores)
    }

    func testTheGateReadsTheLinkThenTheSavedEntitlement() {
        let registry = AccountRegistry()
        XCTAssertEqual(registry.gate, .signedOut)
        let ada = account("ada")
        registry.add(
            ada, entitlement: .active(grant: .purchased(.web), renews: nil), installation: "a")
        XCTAssertEqual(registry.gate, .ready)
        registry.stores?.hosts.show(AccountView(
            binding: .signedIn, email: "", name: "", relay: .connected, pro: false))
        XCTAssertEqual(registry.gate, .unsubscribed, "the link's word outranks the saved one")
        registry.signOut(ada.id)
        XCTAssertEqual(registry.gate, .signedOut)
    }

    func testTheRegistrySurvivesARelaunch() {
        let file = file()
        let first = AccountRegistry(file: file)
        let signedOut = first.signedOutInstallation
        first.add(account("ada"), installation: "ada-dir")
        first.saw(hosts: 2, attention: 1, for: AccountId("ada"))
        let again = AccountRegistry(file: file)
        XCTAssertEqual(again.selected, AccountId("ada"))
        XCTAssertEqual(again.installation, "ada-dir")
        XCTAssertEqual(again.selectedAccount?.line, "2 hosts")
        XCTAssertEqual(again.selectedAccount?.attention, 1)
        XCTAssertEqual(again.signedOutInstallation, signedOut, "ada was not the first on this phone")
        XCTAssertFalse(again.persistenceFailed)
    }

    func testTheSignedOutPhoneKeepsItsInstallationAcrossLaunches() {
        let file = file()
        let first = AccountRegistry(file: file)
        XCTAssertEqual(AccountRegistry(file: file).installation, first.installation)
    }

    func testALateAnswerForAnAccountNotOnScreenIsDropped() {
        let registry = AccountRegistry()
        registry.add(account("ada"), installation: "a")
        XCTAssertEqual(registry.accept(1, for: AccountId("ada")), 1)
        XCTAssertNil(registry.accept(1, for: AccountId("bob")))
        XCTAssertEqual(registry.dropped, 1)
    }
}
