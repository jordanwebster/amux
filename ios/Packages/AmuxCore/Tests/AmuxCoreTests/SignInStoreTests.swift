import Foundation
import XCTest
@testable import AmuxCore

/// A cloud that answers one way, so the store's three outcomes can each be
/// reached without a browser.
private struct OneAnswer: CloudService, @unchecked Sendable {
    var answer: Result<SignedInAccount, CloudError>
    var entitlement: Entitlement = .active(source: .web, renews: nil)

    func signIn(presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount {
        switch answer {
        case .success(let account): return account
        case .failure(let error): throw error
        }
    }

    func account(_ id: AccountId) async throws(CloudError) -> AccountFacts {
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
        let cloud = OneAnswer(answer: .success(ada), entitlement: .active(source: .web, renews: nil))

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
            func signIn(presenting: any WebAuthPresenter) async throws(CloudError) -> SignedInAccount {
                account
            }
            func account(_ id: AccountId) async throws(CloudError) -> AccountFacts {
                throw .timeout
            }
            func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement {
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

    func testTheScreenNamesTheHostTheHandOffOpens() {
        XCTAssertEqual(SignInStore().host, "amux.sh")
        XCTAssertEqual(CloudEndpoint.production.callback.absoluteString, "amux://callback")
    }
}
