import Foundation
import XCTest
@testable import AmuxCore

/// A cloud that answers one way, so the store's three outcomes can each be
/// reached without a browser.
private struct OneAnswer: CloudService, @unchecked Sendable {
    var answer: Result<SignedInAccount, CloudError>
    var entitlement: Entitlement = .active(grant: .purchased(.web), renews: nil)
    /// What was asked for and what was let go of, in order.
    let asked = Asked()

    final class Asked: @unchecked Sendable {
        var intents: [SignInIntent] = []
        var forgotten: [AccountId] = []
    }

    func forgetSession(_ id: AccountId) async throws { asked.forgotten.append(id) }
    func signIn(_ intent: SignInIntent, presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount {
        asked.intents.append(intent)
        switch answer {
        case .success(let account): return account
        case .failure(let error): throw error
        }
    }

    func account(_ id: AccountId) async throws(CloudError) -> AccountFacts {
        throw .unauthenticated
    }

    func recordPurchase(_ id: AccountId, signedTransaction: String) async throws(CloudError) {
        throw .unauthenticated
    }

    func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement { entitlement }

    func connectToken(_ id: AccountId) async throws(CloudError) -> ConnectToken {
        throw .unauthenticated
    }

    func requestDeletion(
        _ id: AccountId, confirmedEmail: String
    ) async throws(CloudError) -> DeletionOutcome {
        throw .unauthenticated
    }

    func uploadReport(
        _ id: AccountId, bundle: ReportBundle
    ) async throws(CloudError) -> ReportReceipt {
        throw .unauthenticated
    }
}

private struct Silent: WebAuthPresenter {
    func present(_ url: URL, callbackScheme: String) async throws(CloudError) -> URL {
        URL(string: "amux://callback")!
    }
}

@MainActor
final class SignInStoreTests: XCTestCase {
    private let ada = SignedInAccount(id: AccountId("ada"), email: "ada@example.com")

    func testASignInThatSucceedsAddsTheAccountWithWhatItIsEntitledTo() async {
        let store = SignInStore()
        let registry = AccountRegistry()
        let cloud = OneAnswer(answer: .success(ada), entitlement: .active(grant: .purchased(.web), renews: nil))

        let account = await store.signIn(with: cloud, presenting: Silent(), into: registry)

        XCTAssertEqual(account, ada)
        XCTAssertEqual(store.phase, .signedIn(ada))
        XCTAssertEqual(registry.accounts.map(\.id), [ada.id])
        XCTAssertEqual(registry.gate, .ready)
    }

    func testASignInTheCloudRefusesSaysWhatTheCloudSaid() async {
        let store = SignInStore()
        let cloud = OneAnswer(answer: .failure(.refused("that address is not recognised")))

        await store.signIn(with: cloud, presenting: Silent())

        XCTAssertEqual(store.phase, .failed("that address is not recognised"))
    }

    func testComingBackWithoutSigningInLeavesNothingToDismiss() async {
        let store = SignInStore()
        let cloud = OneAnswer(answer: .failure(.cancelled))

        await store.signIn(with: cloud, presenting: Silent())

        // Cancelling is a decision, not a failure: the screen is where it was
        // before the button was pressed, with nothing on it to clear.
        XCTAssertEqual(store.phase, .ready)
    }

    func testAnAccountWhoseEntitlementCannotBeReadIsStillSignedIn() async {
        struct Quiet: CloudService {
            let account: SignedInAccount
            func forgetSession(_ id: AccountId) async throws {}
            func signIn(_ intent: SignInIntent, presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount {
                account
            }
            func account(_ id: AccountId) async throws(CloudError) -> AccountFacts {
                throw .timeout
            }
            func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement {
                throw .timeout
            }
            func recordPurchase(
                _ id: AccountId, signedTransaction: String
            ) async throws(CloudError) {
                throw .timeout
            }
            func connectToken(_ id: AccountId) async throws(CloudError) -> ConnectToken {
                throw .timeout
            }
            func requestDeletion(
                _ id: AccountId, confirmedEmail: String
            ) async throws(CloudError) -> DeletionOutcome {
                throw .timeout
            }
            func uploadReport(
                _ id: AccountId, bundle: ReportBundle
            ) async throws(CloudError) -> ReportReceipt {
                throw .timeout
            }
        }
        let store = SignInStore()
        let registry = AccountRegistry()

        await store.signIn(with: Quiet(account: ada), presenting: Silent(), into: registry)

        // The account exists; what it is allowed to do is not yet known, and
        // the gate stays closed rather than the sign-in being called a failure.
        XCTAssertEqual(store.phase, .signedIn(ada))
        XCTAssertEqual(registry.accounts.map(\.id), [ada.id])
        XCTAssertEqual(registry.gate, .unsubscribed)
    }

    func testASecondPressWhileTheBrowserIsUpStartsNothing() async {
        let store = SignInStore(phase: .handingOff)
        let cloud = OneAnswer(answer: .success(ada))

        let account = await store.signIn(with: cloud, presenting: Silent())

        XCTAssertNil(account)
        XCTAssertEqual(store.phase, .handingOff)
    }

    func testAddingAnAccountAsksForTheChooserAndSigningBackInNamesTheAccount() async {
        let store = SignInStore()
        let cloud = OneAnswer(answer: .success(ada))
        await store.signIn(with: cloud, presenting: Silent())
        store.begin(.returning(ada))
        await store.signIn(with: cloud, presenting: Silent())

        XCTAssertEqual(cloud.asked.intents, [.adding, .returning(ada)])
        XCTAssertEqual(store.phase, .signedIn(ada))
    }

    /// Asked for one account and handed another: nothing is added until the
    /// person chooses, and the screen names both.
    func testSigningBackInAsSomebodyElseAddsNobody() async {
        let work = SignedInAccount(id: AccountId("work"), email: "team@acme.example")
        let registry = AccountRegistry()
        registry.add(ada)
        registry.add(work)
        registry.signOut(work.id)
        let store = SignInStore()
        store.begin(.returning(work))
        let stranger = SignedInAccount(id: AccountId("stranger"), email: "jw@example.com")

        let account = await store.signIn(
            with: OneAnswer(answer: .success(stranger)), presenting: Silent(), into: registry)

        XCTAssertNil(account)
        XCTAssertEqual(store.phase, .mismatched(wanted: work, got: stranger))
        XCTAssertEqual(registry.accounts.map(\.id), [ada.id, work.id])
        XCTAssertEqual(registry.accounts.map(\.signedIn), [true, false])
    }

    func testKeepingTheAccountThatCameBackAddsIt() async {
        let work = SignedInAccount(id: AccountId("work"), email: "team@acme.example")
        let stranger = SignedInAccount(id: AccountId("stranger"), email: "jw@example.com")
        let registry = AccountRegistry()
        registry.add(ada)
        let store = SignInStore(phase: .mismatched(wanted: work, got: stranger),
                                intent: .returning(work))
        let cloud = OneAnswer(answer: .success(stranger))

        await store.keep(with: cloud, into: registry)

        XCTAssertEqual(store.phase, .signedIn(stranger))
        XCTAssertEqual(registry.accounts.map(\.id), [ada.id, stranger.id])
        XCTAssertEqual(cloud.asked.forgotten, [])
    }

    /// Turned down, the session that sign-in left is let go — unless it
    /// belongs to an account already signed in here, which would otherwise be
    /// signed out by a press about somebody else.
    func testTurningDownTheAccountThatCameBackLetsGoOfItsSessionOnlyIfUnused() async {
        let work = SignedInAccount(id: AccountId("work"), email: "team@acme.example")
        let stranger = SignedInAccount(id: AccountId("stranger"), email: "jw@example.com")
        let registry = AccountRegistry()
        registry.add(ada)

        let cloud = OneAnswer(answer: .success(stranger))
        let store = SignInStore(phase: .mismatched(wanted: work, got: stranger))
        await store.discard(with: cloud, from: registry)
        XCTAssertEqual(store.phase, .ready)
        XCTAssertEqual(cloud.asked.forgotten, [stranger.id])
        XCTAssertEqual(registry.accounts.map(\.id), [ada.id])

        let signedInElsewhere = OneAnswer(answer: .success(ada))
        let again = SignInStore(phase: .mismatched(wanted: work, got: ada))
        await again.discard(with: signedInElsewhere, from: registry)
        XCTAssertEqual(signedInElsewhere.asked.forgotten, [])
        XCTAssertEqual(registry.accounts.first?.signedIn, true)
    }

    func testTheScreenNamesTheHostTheHandOffOpens() {
        XCTAssertEqual(SignInStore().host, "amux.sh")
        XCTAssertEqual(CloudEndpoint.production.callback.absoluteString, "amux://callback")
    }
}
