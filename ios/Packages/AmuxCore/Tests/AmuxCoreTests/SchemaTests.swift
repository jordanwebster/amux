import Foundation
import XCTest
@testable import AmuxCore

/// The bridge pins its projection as a schema file; this reads that exact file
/// so a change to the Rust DTOs fails here instead of at a screen that quietly
/// stops showing something.
final class SchemaTests: XCTestCase {
    private func pinnedEvents() throws -> [Event] {
        let url = try XCTUnwrap(
            Bundle.module.url(forResource: "schema", withExtension: "json"),
            "the pinned projection schema is missing from the test bundle")
        return try AmuxJSON.decoder.decode([Event].self, from: Data(contentsOf: url))
    }

    func testEveryPinnedEventDecodes() throws {
        let events = try pinnedEvents()
        XCTAssertEqual(events.count, 16)
    }

    func testTheFleetCarriesItsAgentsHostsAndReconciliation() throws {
        let events = try pinnedEvents()
        guard case .fleet(let fleet) = events[1] else { return XCTFail("expected a Fleet, got \(events[1])") }
        XCTAssertEqual(fleet.epoch, 1)
        XCTAssertTrue(fleet.reconciled)
        let card = try XCTUnwrap(fleet.agents.first)
        XCTAssertEqual(card.displayName, "Fix login")
        XCTAssertEqual(card.attention, .idle)
        XCTAssertEqual(card.phase, .running)
        XCTAssertEqual(card.agent.kind, .claude(driver: .pty))
        XCTAssertEqual(card.agent.workingDir, "/work")
        XCTAssertEqual(card.lastActivity, Date(timeIntervalSince1970: 1_700_000_000))
        let host = try XCTUnwrap(fleet.hosts.first)
        XCTAssertEqual(host.entry.name, "studio")
        XCTAssertEqual(host.entry.trustStatus, .trusted)
        XCTAssertTrue(host.entry.online)
    }

    func testTheThreeLayersStayApart() throws {
        let events = try pinnedEvents()
        guard case .fleet(let codex) = events[9], case .fleet(let sdk) = events[12] else {
            return XCTFail("expected the codex and SDK fleets")
        }
        XCTAssertEqual(codex.agents.first?.agent.kind, .codex)
        XCTAssertEqual(sdk.agents.first?.agent.kind, .claude(driver: .sdk))
        XCTAssertEqual(sdk.agents.first?.attention, .unknown)

        guard case .session(let pty) = events[2],
              case .session(let codexSession) = events[10],
              case .session(let sdkSession) = events[13] else {
            return XCTFail("expected one session per layer")
        }
        XCTAssertEqual(pty.gate, .claudePty(.ready))
        XCTAssertEqual(pty.phase.phase, "idle")
        XCTAssertEqual(pty.stream, .live)
        XCTAssertEqual(pty.settingsGate, .ptySettingsUnavailable)
        XCTAssertNil(pty.queue)
        XCTAssertEqual(codexSession.gate, .codex(.ready))
        XCTAssertEqual(codexSession.settingsGate, .ready)
        XCTAssertEqual(sdkSession.gate, .unavailable)
        XCTAssertEqual(sdkSession.settingsGate, .claudeSdk(reason: .unknown))
        // This build cannot read the SDK chat layer, and the projection says so
        // rather than presenting an empty conversation as an idle one.
        XCTAssertEqual(sdkSession.facts, .claudeSdk(supported: false))
    }

    func testSDKModelChoicesDecodeFromRecordedInitialization() throws {
        guard case .session(let session) = try pinnedEvents()[13] else {
            return XCTFail("expected SDK session facts")
        }
        XCTAssertEqual(session.provider.model, "claude-haiku-4-5-20251001")
        XCTAssertEqual(session.provider.models.map(\.id), [
            "default", "opus[1m]", "claude-fable-5[1m]", "sonnet", "haiku"
        ])
        XCTAssertEqual(session.provider.models.first?.name, "Default (recommended)")
        XCTAssertEqual(session.provider.models.first?.efforts, ["low", "medium", "high", "xhigh", "max"])
        XCTAssertTrue(session.provider.models.allSatisfy { $0.defaultEffort == nil })
        XCTAssertEqual(session.provider.models.last?.efforts, [])
        XCTAssertTrue(session.provider.efforts.isEmpty)
    }

    func testFeedRowsKeepTheirLayerPositionAndKind() throws {
        let events = try pinnedEvents()
        guard case .feed(let first) = events[3], case .feed(let rewritten) = events[8],
              case .feed(let codex) = events[11] else {
            return XCTFail("expected the three feed updates")
        }
        XCTAssertEqual(first.base, 0)
        XCTAssertEqual(first.append.count, 1)
        XCTAssertEqual(first.append[0].layer, .claudePty)
        XCTAssertEqual(first.append[0].entryKind, "message")
        XCTAssertEqual(first.append[0].seq, 1)

        XCTAssertEqual(rewritten.base, 1)
        XCTAssertTrue(rewritten.append.isEmpty)
        XCTAssertEqual(rewritten.replace.count, 1)
        XCTAssertEqual(rewritten.replace[0].position, 0)
        XCTAssertEqual(
            rewritten.replace[0].entry.row["kind"]?["segments"]?.arrayValue?.compactMap(\.stringValue),
            ["Hello", "Updated"])

        XCTAssertEqual(codex.append[0].layer, .codex)
        XCTAssertEqual(codex.append[0].seq, 2)
    }

    func testOutcomesDiffsTokensAndInvariants() throws {
        let events = try pinnedEvents()
        guard case .opResult(let sent) = events[5], case .opResult(let refused) = events[6] else {
            return XCTFail("expected two operation results")
        }
        XCTAssertEqual(sent.outcome, .inputSent)
        guard case .failed(let failure) = refused.outcome else {
            return XCTFail("expected a refusal, got \(refused.outcome)")
        }
        XCTAssertEqual(failure.error, "general")
        XCTAssertEqual(failure.message, "send refused")
        XCTAssertFalse(failure.authRequired)
        XCTAssertFalse(failure.subscriptionRequired)

        // A patch arrives split into files and numbered on both sides. The
        // phone parses no diffs of its own, so what the core sends is what the
        // page draws and what a comment is anchored against.
        guard case .diff(let diff) = events[7] else { return XCTFail("expected a Diff") }
        XCTAssertEqual(diff.diff.digest.hasPrefix("sha256:"), true)
        let file = try XCTUnwrap(diff.document.files.first)
        XCTAssertEqual(file.path, "one.rs")
        XCTAssertEqual(file.rows.map(\.text), ["-old", "+new"])
        XCTAssertEqual(file.rows.map(\.kind), [.removed, .added])
        XCTAssertEqual(file.rows.map(\.old), [1, nil])
        XCTAssertEqual(file.rows.map(\.new), [nil, 1])
        XCTAssertEqual(file.hunkStarts, [0])
        XCTAssertEqual(diff.document.identity.base, .workingTree)
        XCTAssertEqual(diff.document.identity.head, "abc")

        XCTAssertEqual(events[4], .tokenRequest(requestId: 7))
        XCTAssertEqual(events[15], .invariant(detail: "example diagnostic"))
    }

    func testTheLinkStatesWhyItIsDown() throws {
        let events = try pinnedEvents()
        XCTAssertEqual(events[0], .connection(ConnectionUpdate(state: .connecting)))
        XCTAssertEqual(
            events[14],
            .connection(ConnectionUpdate(state: .disconnected, reason: "relay unavailable")))
    }

    func testEveryPinnedEventSurvivesARoundTrip() throws {
        let events = try pinnedEvents()
        let encoded = try AmuxJSON.encoder.encode(events)
        let again = try AmuxJSON.decoder.decode([Event].self, from: encoded)
        XCTAssertEqual(events, again)
    }

    func testFractionalTimestampsAreReadRatherThanRefused() throws {
        // The core writes anything from no fractional seconds to nine digits
        // of them. A Date cannot hold nanoseconds this far from its epoch, so
        // what matters is that a precise timestamp is read at all rather than
        // failing the whole batch it arrived in.
        let precise = try XCTUnwrap(AmuxJSON.timestamp("2023-11-14T22:13:20.123456789Z"))
        XCTAssertEqual(precise.timeIntervalSince1970, 1_700_000_000.123456789, accuracy: 0.000_001)
        XCTAssertEqual(AmuxJSON.timestamp("2023-11-14T22:13:20+00:00"),
                       Date(timeIntervalSince1970: 1_700_000_000))
        XCTAssertNil(AmuxJSON.timestamp("yesterday"))
    }
}
