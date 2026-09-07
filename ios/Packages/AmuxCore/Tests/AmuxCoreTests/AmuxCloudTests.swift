import Foundation
import XCTest
@testable import AmuxCore

/// A transport that answers what the test says and keeps what it was asked.
///
/// Nothing here reaches a network. Every branch of the adapter — the redirect
/// it builds, the code it redeems, the refusal it reports, the deletion the
/// billing system blocks — is driven from this side, which is why the suite is
/// offline and takes no account with it.
private final class Answers: CloudTransport, @unchecked Sendable {
    struct Reply {
        var status: Int
        var body: String
    }

    private let lock = NSLock()
    private var replies: [String: Reply] = [:]
    private(set) var asked: [URLRequest] = []

    init(_ replies: [String: Reply]) {
        self.replies = replies
    }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        lock.withLock { asked.append(request) }
        let path = request.url?.path() ?? ""
        let reply = lock.withLock { replies[path] } ?? Reply(status: 404, body: "{}")
        let response = HTTPURLResponse(
            url: request.url!, statusCode: reply.status,
            httpVersion: nil, headerFields: nil)!
        return (Data(reply.body.utf8), response)
    }

    func plus(_ path: String, status: Int, body: String) {
        lock.withLock { replies[path] = Reply(status: status, body: body) }
    }

    func request(_ path: String) -> URLRequest? {
        lock.withLock { asked.first { $0.url?.path() == path } }
    }

    func body(_ path: String) -> String {
        request(path)?.httpBody.flatMap { String(data: $0, encoding: .utf8) } ?? ""
    }

    func bearer(_ path: String) -> String? {
        request(path)?.value(forHTTPHeaderField: "Authorization")
    }
}

/// A presenter that answers with the callback amux.sh would have sent, and
/// records the URL it was handed.
private final class Handed: WebAuthPresenter, @unchecked Sendable {
    private let lock = NSLock()
    private var answer: (URL) -> Result<URL, CloudError>
    private(set) var opened: URL?

    init(_ answer: @escaping (URL) -> Result<URL, CloudError>) {
        self.answer = answer
    }

    func present(_ url: URL, callbackScheme: String) async throws(CloudError) -> URL {
        lock.withLock { opened = url }
        switch answer(url) {
        case .success(let callback): return callback
        case .failure(let error): throw error
        }
    }

    /// The callback the account service sends back, echoing the state the app
    /// put in the authorize URL.
    static func returning(code: String) -> Handed {
        Handed { url in
            let state = URLComponents(url: url, resolvingAgainstBaseURL: false)?
                .queryItems?.first { $0.name == "state" }?.value ?? ""
            return .success(URL(string: "amux://callback?code=\(code)&state=\(state)")!)
        }
    }
}

/// The moment every answer in this suite is read at.
private let now = Date(timeIntervalSince1970: 1_700_000_000)

final class AmuxCloudTests: XCTestCase {
    private let endpoint = CloudEndpoint(
        base: URL(string: "https://amux.test")!, clientID: "mobile",
        callback: URL(string: "amux://callback")!,
        scopes: ["openid", "profile", "email", "offline_access", "api"])

    private func service(_ answers: Answers) -> AmuxCloudService {
        AmuxCloudService(endpoint: endpoint, transport: answers, now: { now })
    }

    private var signedIn: Answers {
        Answers([
            "/connect/token": .init(status: 200, body: """
                {"access_token":"at-1","refresh_token":"rt-1","expires_in":3600}
                """),
            "/connect/userinfo": .init(status: 200, body: """
                {"sub":"ada","email":"ada@example.com","name":"Ada"}
                """),
        ])
    }

    func testSignInHandsOffWithPkceAndRedeemsTheCodeItComesBackWith() async throws {
        let answers = signedIn
        let presenter = Handed.returning(code: "code-1")
        let account = try await service(answers).signIn(presenting: presenter)

        XCTAssertEqual(account.id, AccountId("ada"))
        XCTAssertEqual(account.email, "ada@example.com")
        XCTAssertEqual(account.displayName, "Ada")

        let opened = try XCTUnwrap(presenter.opened)
        let query = try XCTUnwrap(
            URLComponents(url: opened, resolvingAgainstBaseURL: false)?.queryItems)
        func item(_ name: String) -> String? { query.first { $0.name == name }?.value }
        XCTAssertEqual(opened.host(), "amux.test")
        XCTAssertEqual(opened.path(), "/connect/authorize")
        XCTAssertEqual(item("client_id"), "mobile")
        XCTAssertEqual(item("response_type"), "code")
        XCTAssertEqual(item("redirect_uri"), "amux://callback")
        XCTAssertEqual(item("scope"), "openid profile email offline_access api")
        XCTAssertEqual(item("code_challenge_method"), "S256")

        // The verifier is what the app keeps and the challenge is its hash:
        // the redemption must carry the one the authorize URL committed to,
        // or nothing this app opened is what it redeemed.
        let redeemed = answers.body("/connect/token")
        let verifier = try XCTUnwrap(
            redeemed.split(separator: "&").first { $0.hasPrefix("code_verifier=") })
            .dropFirst("code_verifier=".count)
        XCTAssertEqual(item("code_challenge"), AmuxCloudService.challenge(for: String(verifier)))
        XCTAssertTrue(redeemed.contains("grant_type=authorization_code"))
        XCTAssertTrue(redeemed.contains("code=code-1"))
        XCTAssertTrue(redeemed.contains("client_id=mobile"))
        XCTAssertEqual(answers.bearer("/connect/userinfo"), "Bearer at-1")
    }

    func testACallbackThatAnswersADifferentRequestIsNeverRedeemed() async {
        let answers = signedIn
        // Another app claiming the callback cannot know the state this phone
        // just made, so a code arriving with the wrong one is not this
        // sign-in's code.
        let presenter = Handed { _ in
            .success(URL(string: "amux://callback?code=stolen&state=someone-else")!)
        }
        await assert(.refused("that sign-in answered a different request")) {
            try await self.service(answers).signIn(presenting: presenter)
        }
        XCTAssertNil(answers.request("/connect/token"))
    }

    func testACallbackNamingAnErrorIsReportedInTheCloudsOwnWords() async {
        let presenter = Handed { _ in
            .success(URL(string:
                "amux://callback?error=access_denied&error_description=that%20address%20is%20not%20recognised")!)
        }
        await assert(.refused("that address is not recognised")) {
            try await self.service(self.signedIn).signIn(presenting: presenter)
        }
    }

    func testClosingTheBrowserIsCancelledRatherThanFailed() async {
        let presenter = Handed { _ in .failure(.cancelled) }
        await assert(.cancelled) {
            try await self.service(self.signedIn).signIn(presenting: presenter)
        }
    }

    func testAConnectTokenIsAskedForWithTheAccessTokenTheSignInReturned() async throws {
        let answers = signedIn
        answers.plus("/api/connect", status: 200, body: """
            {"host":"relay.amux.test","port":443,"token":"relay-jwt",
             "expires_at":"2023-11-14T23:13:20Z"}
            """)
        let cloud = service(answers)
        let account = try await cloud.signIn(presenting: Handed.returning(code: "code-1"))
        let token = try await cloud.connectToken(account.id)

        XCTAssertEqual(token.bearer, "relay-jwt")
        XCTAssertEqual(token.expiresAt, Date(timeIntervalSince1970: 1_700_003_600))
        XCTAssertEqual(answers.bearer("/api/connect"), "Bearer at-1")
    }

    func testAnAccountWithNothingBoughtIsRefusedInTheWordsTheGateUses() async throws {
        let answers = signedIn
        answers.plus("/api/connect", status: 403, body: #"{"error":"payment_required"}"#)
        let cloud = service(answers)
        let account = try await cloud.signIn(presenting: Handed.returning(code: "code-1"))
        await assert(.refused("this account has no subscription")) {
            try await cloud.connectToken(account.id)
        }
    }

    func testEntitlementReadsTheSubscriptionsProviderAndItsRenewal() async throws {
        let ends = Date(timeIntervalSince1970: 1_701_004_800)
        for (provider, source) in [("REVENUE_CAT", EntitlementSource.appStore),
                                   ("STRIPE", EntitlementSource.web)] {
            let answers = signedIn
            answers.plus("/api/graphql", status: 200, body: """
                {"data":{"me":{"subscription":{"status":"ACTIVE","provider":"\(provider)",
                 "willRenew":true,"entitledUntil":"2023-11-26T13:20:00Z"}}}}
                """)
            let cloud = service(answers)
            let account = try await cloud.signIn(presenting: Handed.returning(code: "code-1"))
            let entitlement = try await cloud.entitlement(account.id)
            XCTAssertEqual(entitlement, Entitlement.active(source: source, renews: ends))
        }
    }

    func testASubscriptionRidingOutItsPeriodIsActiveAndRenewsOnNoDate() async throws {
        let answers = signedIn
        answers.plus("/api/graphql", status: 200, body: """
            {"data":{"me":{"subscription":{"status":"CANCELLED","provider":"STRIPE",
             "willRenew":false,"entitledUntil":"2023-11-26T13:20:00Z"}}}}
            """)
        let cloud = service(answers)
        let account = try await cloud.signIn(presenting: Handed.returning(code: "code-1"))
        let entitlement = try await cloud.entitlement(account.id)
        XCTAssertEqual(entitlement, Entitlement.active(source: .web, renews: nil))
    }

    func testAnEntitlementPastItsPeriodIsLapsedWhateverTheBillingSystemCallsIt() async throws {
        let answers = signedIn
        // The provider still says active; the period this account paid for ran
        // out yesterday. The date is what the screen has to say, not the word.
        answers.plus("/api/graphql", status: 200, body: """
            {"data":{"me":{"subscription":{"status":"ACTIVE","provider":"REVENUE_CAT",
             "willRenew":true,"entitledUntil":"2023-11-13T13:20:00Z"}}}}
            """)
        let cloud = service(answers)
        let account = try await cloud.signIn(presenting: Handed.returning(code: "code-1"))
        let entitlement = try await cloud.entitlement(account.id)
        XCTAssertEqual(
            entitlement,
            Entitlement.lapsed(source: .appStore, endedAt: Date(timeIntervalSince1970: 1_699_881_600)))
    }

    func testAnAccountThatBoughtNothingIsEntitledToNothing() async throws {
        let answers = signedIn
        answers.plus("/api/graphql", status: 200, body: #"{"data":{"me":{"subscription":null}}}"#)
        let cloud = service(answers)
        let account = try await cloud.signIn(presenting: Handed.returning(code: "code-1"))
        let entitlement = try await cloud.entitlement(account.id)
        XCTAssertEqual(entitlement, Entitlement.none)
    }

    func testDeletionIsRefusedBeforeItLeavesWhenTheTypedAddressIsNotThisAccounts() async throws {
        let answers = signedIn
        let cloud = service(answers)
        let account = try await cloud.signIn(presenting: Handed.returning(code: "code-1"))
        await assert(.refused("that is not this account's address")) {
            try await cloud.requestDeletion(account.id, confirmedEmail: "bo@example.com")
        }
        XCTAssertNil(answers.request("/api/account"))
    }

    func testDeletionGoesThroughWhenTheAddressMatches() async throws {
        let answers = signedIn
        answers.plus("/api/account", status: 200, body: "")
        let cloud = service(answers)
        let account = try await cloud.signIn(presenting: Handed.returning(code: "code-1"))
        let outcome = try await cloud.requestDeletion(
            account.id, confirmedEmail: "  ADA@example.com ")

        XCTAssertEqual(outcome, .deleted)
        XCTAssertEqual(answers.request("/api/account")?.httpMethod, "DELETE")
    }

    func testADeletionBlockedByAnAppStoreSubscriptionSendsYouToTheSystemsOwnPage() async throws {
        let answers = signedIn
        answers.plus(
            "/api/account", status: 409,
            body: #"{"error":"active_subscription","provider":"revenuecat"}"#)
        let cloud = service(answers)
        let account = try await cloud.signIn(presenting: Handed.returning(code: "code-1"))
        let outcome = try await cloud.requestDeletion(
            account.id, confirmedEmail: "ada@example.com")

        XCTAssertEqual(outcome, .blockedByRenewal(
            source: .appStore, manageURL: AmuxCloudService.appStoreSubscriptions))
        // Nothing this cloud can cancel, so nothing was asked of it.
        XCTAssertNil(answers.request("/api/billing/stripe/portal"))
    }

    func testADeletionBlockedByAWebSubscriptionSendsYouToTheBillingPortal() async throws {
        let answers = signedIn
        answers.plus(
            "/api/account", status: 409,
            body: #"{"error":"active_subscription","provider":"stripe"}"#)
        answers.plus(
            "/api/billing/stripe/portal", status: 200,
            body: #"{"url":"https://billing.test/session/1"}"#)
        let cloud = service(answers)
        let account = try await cloud.signIn(presenting: Handed.returning(code: "code-1"))
        let outcome = try await cloud.requestDeletion(
            account.id, confirmedEmail: "ada@example.com")

        XCTAssertEqual(outcome, .blockedByRenewal(
            source: .web, manageURL: URL(string: "https://billing.test/session/1")!))
    }

    func testAnExpiredAccessTokenIsRefreshedRatherThanSendingSomebodyBackToABrowser() async throws {
        let answers = Answers([
            "/connect/token": .init(status: 200, body: """
                {"access_token":"at-1","refresh_token":"rt-1","expires_in":0}
                """),
            "/connect/userinfo": .init(status: 200, body: """
                {"sub":"ada","email":"ada@example.com","name":"Ada"}
                """),
            "/api/connect": .init(status: 200, body: """
                {"host":"relay.amux.test","port":443,"token":"relay-jwt"}
                """),
        ])
        let cloud = service(answers)
        let account = try await cloud.signIn(presenting: Handed.returning(code: "code-1"))
        _ = try await cloud.connectToken(account.id)

        let exchanges = answers.asked.filter { $0.url?.path() == "/connect/token" }
        XCTAssertEqual(exchanges.count, 2)
        let refresh = String(data: exchanges[1].httpBody ?? Data(), encoding: .utf8) ?? ""
        XCTAssertTrue(refresh.contains("grant_type=refresh_token"), refresh)
        XCTAssertTrue(refresh.contains("refresh_token=rt-1"), refresh)
    }

    func testAnAccountThisPhoneHasNotSignedIntoIsUnauthenticated() async {
        await assert(.unauthenticated) {
            try await self.service(self.signedIn).connectToken(AccountId("nobody"))
        }
    }

    private func assert<Value>(
        _ expected: CloudError, _ act: () async throws -> Value,
        file: StaticString = #filePath, line: UInt = #line
    ) async {
        do {
            _ = try await act()
            XCTFail("expected \(expected)", file: file, line: line)
        } catch let error as CloudError {
            XCTAssertEqual(error, expected, file: file, line: line)
        } catch {
            XCTFail("expected \(expected), got \(error)", file: file, line: line)
        }
    }
}
