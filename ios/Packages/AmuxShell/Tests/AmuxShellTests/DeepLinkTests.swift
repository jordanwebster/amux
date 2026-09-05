import AmuxCore
import XCTest

@testable import AmuxShell

@MainActor
final class DeepLinkTests: XCTestCase {
    /// A link of the shape `amux pair --qr` prints: the host's JSON offer in
    /// URL-safe base64 without padding. Pinned as a whole string so a change
    /// to how the host writes an invitation breaks here rather than on a
    /// phone that cannot read a QR code any more.
    private let link = URL(string: """
        amux://pair?payload=eyJob3N0X2lkIjoiM2ExZjlkMmMtMDAwMC00MDAwLTgwMDAtMDAwMDAwMDAwMDAxIiwi\
        Y2xvdWRfdXJsIjoiaHR0cHM6Ly9yZWxheS5hbXV4LnNoIiwic2VjcmV0IjpbMSwyLDMsNCw1LDYsNyw4LDksMTAs\
        MTEsMTIsMTMsMTQsMTUsMTYsMTcsMTgsMTksMjAsMjEsMjIsMjMsMjQsMjUsMjYsMjcsMjgsMjksMzAsMzEsMzJd\
        fQ
        """)!

    func testAPairingLinkReadsAsTheOfferTheHostWrote() throws {
        let parsed = try XCTUnwrap(DeepLink(link))
        guard case .pair(let invitation) = parsed else {
            return XCTFail("expected a pairing invitation, got \(parsed)")
        }
        XCTAssertEqual(invitation.host, HostId("3a1f9d2c-0000-4000-8000-000000000001"))
        XCTAssertEqual(invitation.cloudURL, "https://relay.amux.sh")
        XCTAssertEqual(invitation.secret, Array(UInt8(1)...UInt8(32)))
    }

    func testAPairingLinkPairsWithNobodyAndAsksInstead() throws {
        let router = Router()
        let parsed = try XCTUnwrap(router.open(link))
        guard case .pair(let invitation) = parsed else {
            return XCTFail("expected a pairing invitation, got \(parsed)")
        }

        // The link lands on a page that names the machine and waits. Trust is
        // committed by the person looking at the fingerprint, never by the
        // arrival of a URL anybody could send.
        XCTAssertEqual(router.tab, .hosts)
        XCTAssertEqual(router.path, [.pairConfirmation(invitation)])
    }

    func testAnInvitationDoesNotPrintItsSecret() {
        guard case .pair(let invitation)? = DeepLink(link) else {
            return XCTFail("expected a pairing invitation")
        }
        XCTAssertEqual(invitation.description, "invitation from 3a1f9d2c-0000-4000-8000-000000000001")
    }

    func testASignInCallbackIsNotNavigation() throws {
        let router = Router()
        let callback = URL(string: "amux://callback?code=abc&state=xyz")!

        let parsed = try XCTUnwrap(router.open(callback))

        guard case .signInCallback(let url) = parsed else {
            return XCTFail("expected a sign-in callback, got \(parsed)")
        }
        XCTAssertEqual(url, callback)
        XCTAssertEqual(router.path, [])
        XCTAssertEqual(router.tab, .agents)
    }

    func testACallbackIsHeldUntilItIsCollectedAndThenOnlyOnce() {
        let router = Router()
        let callback = URL(string: "amux://callback?code=abc")!
        router.open(callback)

        XCTAssertNotNil(router.held)
        XCTAssertNil(router.takeHeld { if case .pair = $0 { true } else { false } })
        XCTAssertNotNil(router.takeHeld())
        XCTAssertNil(router.takeHeld())
    }

    func testALinkThatCannotBeReadIsRefusedRatherThanShown() {
        let router = Router()
        for text in [
            // Not this app's scheme.
            "https://amux.sh/pair?payload=abc",
            // This app's scheme, nothing it knows how to do.
            "amux://unknown",
            // A pairing link with no offer in it.
            "amux://pair",
            // An offer that is not base64.
            "amux://pair?payload=%25%25%25",
            // Base64 that is not the JSON a host writes.
            "amux://pair?payload=eyJoZWxsbyI6IndvcmxkIn0",
            // The JSON a host writes, with something that is not a host id.
            "amux://pair?payload=eyJob3N0X2lkIjoibm90LWEtdXVpZCIsImNsb3VkX3VybCI6IiIsInNlY3JldCI6W119",
        ] {
            let url = URL(string: text)!
            XCTAssertNil(DeepLink(url), "\(text) should not read as a link")
            XCTAssertNil(router.open(url), "\(text) should lead nowhere")
        }
        XCTAssertEqual(router.path, [])
        XCTAssertEqual(router.tab, .agents)
        XCTAssertNil(router.held)
    }

    func testBase64AsAURLCarriesIt() {
        // The two characters a URL substitutes, and the padding it drops.
        XCTAssertEqual(Data(base64URLEncoded: "-_-_"), Data([0xfb, 0xff, 0xbf]))
        XCTAssertEqual(Data(base64URLEncoded: "YQ"), Data("a".utf8))
        XCTAssertEqual(Data(base64URLEncoded: "YWI"), Data("ab".utf8))
        XCTAssertEqual(Data(base64URLEncoded: "YWJj"), Data("abc".utf8))
        XCTAssertNil(Data(base64URLEncoded: "not base64"))
    }
}
