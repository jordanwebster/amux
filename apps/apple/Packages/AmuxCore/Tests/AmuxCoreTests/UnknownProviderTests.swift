import Foundation
import XCTest
@testable import AmuxCore

/// An agent run by a provider this build has no case for.
///
/// One machine on an account can run a newer amux than the phone. What comes
/// back then is a real agent with a provider name this build has never heard
/// of, and there are only two honest answers: keep it and say so, or throw the
/// whole fleet away. These pin the first.
final class UnknownProviderTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)

    private func card(kind: JSONValue) throws -> AgentCard {
        let body = JSONValue.object([
            "agent": .object([
                "id": .string("00000000-0000-0000-0000-0000000000B1"),
                "host_id": .string("00000000-0000-0000-0000-0000000000AA"),
                "name": .string("legacy-port"),
                "command": .string("something-else"),
                "working_dir": .string("~/src/legacy"),
                "kind": kind,
                "readonly": .bool(false),
                "args": .array([]),
                "created_at": .string("2026-09-01T09:00:00Z"),
            ]),
            "display_name": .string("legacy-port"),
            "attention": .object(["attention": .string("idle")]),
            "phase": .object(["phase": .string("running")]),
            "last_activity": .string("2026-09-01T09:14:00Z"),
        ])
        return try AmuxJSON.decoder.decode(
            AgentCard.self, from: AmuxJSON.encoder.encode(body))
    }

    /// A provider name this build has never seen arrives under that name
    /// rather than throwing. Refusing it would take the whole fleet down with
    /// it, which turns one agent nobody can read into a phone showing nothing.
    func testAnUnrecognisedProviderIsKeptUnderItsOwnName() throws {
        let card = try card(kind: .object(["kind": .string("gemini")]))

        XCTAssertEqual(card.agent.kind, .unknown("gemini"))
        XCTAssertFalse(AgentRow(card: card, unread: false).readable)
    }

    /// The name it arrived under is the name it goes back out under. A phone
    /// that renamed a provider on the way through would be reporting something
    /// the host never said.
    func testTheProvidersOwnNameSurvivesTheRoundTrip() throws {
        let card = try card(kind: .object(["kind": .string("gemini")]))
        let round = try AmuxJSON.decoder.decode(
            AgentCard.self, from: AmuxJSON.encoder.encode(card))

        XCTAssertEqual(round.agent.kind, .unknown("gemini"))
    }

    /// The providers this build does know are still read as themselves, and
    /// their conversations are still offered.
    func testTheProvidersThisBuildKnowsAreStillReadable() throws {
        let claude = try card(kind: .object([
            "kind": .string("claude"), "driver": .string("pty")]))
        let sdk = try card(kind: .object([
            "kind": .string("claude"), "driver": .string("sdk")]))
        let codex = try card(kind: .object(["kind": .string("codex")]))

        XCTAssertEqual(claude.agent.kind, .claude(driver: .pty))
        // The SDK layer is a Claude driver this build knows, and a session on
        // it opens into the layer's own typed unsupported state rather than
        // being refused at the door.
        XCTAssertEqual(sdk.agent.kind, .claude(driver: .sdk))
        XCTAssertEqual(codex.agent.kind, .codex)
        for card in [claude, sdk, codex] {
            XCTAssertTrue(AgentRow(card: card, unread: false).readable)
        }
    }
}
