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
        XCTAssertEqual(events.count, 26)
    }

    /// Which removed accounts a start is really rid of. The app may drop a
    /// pending removal on this event and on nothing else, so a build that
    /// could not read it would show accounts as gone that are still here.
    func testAStartNamesTheRemovedAccountsItIsReallyRidOf() throws {
        XCTAssertEqual(try pinnedEvents().last, .forgotten(accounts: ["work"]))
    }

    /// The keys the phone holds arrive whole: a screen that showed half a
    /// fingerprint because the DTO lost a field would look right and be wrong.
    func testThisPhoneAndItsPairedDevicesCarryTheirFingerprints() throws {
        let events = try pinnedEvents()
        let rosters = events.compactMap { event -> DeviceRoster? in
            guard case .devices(let roster) = event else { return nil }
            return roster
        }
        guard let roster = rosters.last else {
            return XCTFail("expected a device roster in the pinned events")
        }
        XCTAssertEqual(roster.identity.name, "iPhone")
        XCTAssertEqual(roster.identity.fingerprint.count, 64)
        let device = try XCTUnwrap(roster.devices.first)
        XCTAssertEqual(device.name, "studio")
        XCTAssertEqual(device.fingerprint.count, 64)
        XCTAssertEqual(device.pairedAt, Date(timeIntervalSince1970: 1_700_000_000))
    }

    func testTheFleetCarriesItsAgentsHostsAndReconciliation() throws {
        let events = try pinnedEvents()
        guard case .fleet(let fleet) = events[2] else { return XCTFail("expected a Fleet, got \(events[2])") }
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
        guard case .fleet(let codex) = events[12], case .fleet(let sdk) = events[16] else {
            return XCTFail("expected the codex and SDK fleets")
        }
        XCTAssertEqual(codex.agents.first?.agent.kind, .codex)
        XCTAssertEqual(sdk.agents.first?.agent.kind, .claude(driver: .sdk))
        XCTAssertEqual(sdk.agents.first?.attention, .unknown)

        guard case .session(let pty) = events[3],
              case .session(let codexSession) = events[13],
              case .session(let sdkSession) = events[17] else {
            return XCTFail("expected one session per layer")
        }
        XCTAssertEqual(pty.gate, .claudePty(.unknown))
        XCTAssertEqual(pty.phase.phase, "unknown")
        XCTAssertEqual(pty.stream, .live)
        XCTAssertEqual(pty.settingsGate, .ptySettingsUnavailable)
        XCTAssertNil(pty.queue)
        XCTAssertEqual(codexSession.gate, .codex(.ready))
        XCTAssertEqual(codexSession.settingsGate, .ready)
        XCTAssertEqual(sdkSession.gate, .claudeSdk(.unknown))
        XCTAssertEqual(sdkSession.settingsGate, .claudeSdk(reason: .unknown))
        guard case .claudeSdk(let facts) = sdkSession.facts else {
            return XCTFail("expected SDK session facts")
        }
        XCTAssertEqual(facts["session"]?["model"]?.stringValue, sdkSession.provider.model)
    }

    func testSDKModelChoicesDecodeFromRecordedInitialization() throws {
        guard case .session(let session) = try pinnedEvents()[17] else {
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
        guard case .feed(let first) = events[4], case .feed(let rewritten) = events[10],
              case .feed(let codex) = events[14] else {
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
            ["Hello\n\nUpdated"])

        XCTAssertEqual(codex.append[0].layer, .codex)
        XCTAssertEqual(codex.append[0].seq, 2)
    }

    /// A break in stored history arrives as a row of its own, drawn as a rule.
    func testABreakInStoredHistoryDecodesAsItsOwnRow() throws {
        let events = try pinnedEvents()
        guard case .feed(let stored) = events[21] else {
            return XCTFail("expected a stored feed with a break, got \(events[21])")
        }
        XCTAssertEqual(stored.append.count, 1)
        XCTAssertEqual(stored.append[0].layer, .history)
        XCTAssertEqual(stored.append[0].rowId, 2)
        XCTAssertEqual(stored.append[0].kind, .historyBreak(label: "missing history"))
    }

    func testOutcomesDiffsTokensAndInvariants() throws {
        let events = try pinnedEvents()
        guard case .opResult(let sent) = events[6], case .opResult(let refused) = events[7] else {
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
        guard case .diff(let diff) = events[8] else { return XCTFail("expected a Diff") }
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

        XCTAssertEqual(events[5], .tokenRequest(requestId: 7, account: "personal"))
        XCTAssertEqual(events[19], .invariant(detail: "example diagnostic"))
        // What an account nobody is looking at has waiting, named for itself.
        XCTAssertEqual(events[21], .attention(account: "work", waiting: 2))
        XCTAssertEqual(events[20], .storeFailure(message: "store /cache/personal.sqlite: the disk is full; free space and relaunch"))
    }

    func testTheLinkStatesWhyItIsDown() throws {
        let events = try pinnedEvents()
        XCTAssertEqual(events[0], .connection(ConnectionUpdate(state: .connecting)))
        XCTAssertEqual(
            events[18],
            .connection(ConnectionUpdate(state: .disconnected, reason: .unreachable)))
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

extension SchemaTests {
    /// What this device's link is doing and what the account buys arrive as
    /// their own event, so nothing has to ask an account service to know what
    /// the relay will do for this phone.
    func testTheCloudStateCarriesItsTierAndCarrier() throws {
        let events = try pinnedEvents()
        let states = events.compactMap { event -> CloudState? in
            guard case .cloudState(let state) = event else { return nil }
            return state
        }
        XCTAssertEqual(states.last, .connected(tier: .pro, carrier: .quic))
        XCTAssertEqual(states.first, .signedOut, "a phone starts signed out")
        XCTAssertEqual(states.last?.tier, .pro)
    }
}
