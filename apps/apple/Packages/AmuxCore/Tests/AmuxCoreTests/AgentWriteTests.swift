import Foundation
import XCTest
@testable import AmuxCore

/// The writes that are about the agent rather than to it, and the exact bytes
/// each one puts on the wire.
///
/// The core owns every one of these words. A model, an effort level, a
/// permission and a name are all the host's to keep, so what is pinned here is
/// the vocabulary the phone speaks — a spelling that drifts from the core's
/// fails here rather than at a sheet that quietly stops taking.
@MainActor
final class AgentWriteTests: XCTestCase {
    private let agent = AgentId(UUID(uuidString: "00000000-0000-0000-0000-0000000000C1")!)

    private func bundle(recording sent: @escaping (JSONValue) -> Void) -> StoreBundle {
        let bundle = StoreBundle(account: AccountId("test"))
        bundle.dispatch = { command in
            guard case .shared(let body) = command else { return nil }
            sent(body)
            return OpId(UUID())
        }
        return bundle
    }

    @discardableResult
    private func session(
        _ bundle: StoreBundle, gate: SettingsGate, permission: JSONValue = .null
    ) -> ConversationStore {
        let store = bundle.conversation(agent)
        store.apply(.session(SessionSnapshot(
            agent: agent, gate: .claudePty(.ready), phase: .unavailable, stream: .live,
            asks: [], facts: .unavailable,
            provider: ProviderFacts(
                permission: permission == .null
                    ? .object(["provider": .string("unavailable")]) : permission),
            settingsGate: gate, queue: nil, family: [])))
        return store
    }

    // MARK: - How the agent runs

    func testAModelAndAnEffortAreSpelledAsTheCoreSpellsThem() {
        var written: [JSONValue] = []
        let bundle = bundle { written.append($0) }
        session(bundle, gate: .ready)

        XCTAssertTrue(bundle.setModel("gpt-5.6-luna", of: agent))
        XCTAssertTrue(bundle.setEffort("high", of: agent))
        XCTAssertEqual(written, [
            .object([
                "command": .string("set_model"),
                "agent": .string(agent.description),
                "model": .string("gpt-5.6-luna"),
            ]),
            .object([
                "command": .string("set_effort"),
                "agent": .string(agent.description),
                "effort": .string("high"),
            ]),
        ])
    }

    /// The sheet opens on a session that cannot be changed, because what an
    /// agent is running under is worth reading either way. Pressing a row in
    /// it must not send anything: the layer said it would refuse, and the
    /// sheet is already printing that sentence.
    func testASessionThatRefusesSettingsIsNotWrittenTo() {
        let bundle = bundle { _ in XCTFail("a refused setting left the phone") }
        session(bundle, gate: .ptySettingsUnavailable)

        XCTAssertFalse(bundle.setModel("opus", of: agent))
        XCTAssertFalse(bundle.setEffort("high", of: agent))
        XCTAssertFalse(bundle.setPermission("plan", of: agent))
    }

    /// Claude runs under one named mode and that is its own command, not a
    /// preset with a single axis.
    func testClaudesPermissionIsAMode() {
        var written: [JSONValue] = []
        let bundle = bundle { written.append($0) }
        session(
            bundle, gate: .ready,
            permission: .object(["provider": .string("claude"), "mode": .string("default")]))

        XCTAssertTrue(bundle.setPermission("plan", of: agent))
        XCTAssertEqual(written, [.object([
            "command": .string("claude_sdk"),
            "claude_sdk_command": .string("set_permission_mode"),
            "agent": .string(agent.description),
            "mode": .string("plan"),
        ])])
    }

    /// Codex runs under two axes at once, and the preset a person pressed is
    /// sent as the pair it stands for — the same pair the sheet named under it.
    func testCodexsPermissionIsBothAxesOfThePresetThatWasPressed() {
        var written: [JSONValue] = []
        let bundle = bundle { written.append($0) }
        session(
            bundle, gate: .ready,
            permission: .object([
                "provider": .string("codex"),
                "approval": .string("untrusted"),
                "sandbox": .string("read-only"),
            ]))

        XCTAssertTrue(bundle.setPermission("full-access", of: agent))
        XCTAssertEqual(written, [.object([
            "command": .string("set_preset"),
            "agent": .string(agent.description),
            "approval": .string("never"),
            "sandbox": .string("danger-full-access"),
        ])])
    }

    /// A configuration the presets do not name is drawn so a person can read
    /// where the agent is. It is not somewhere they can send it: nothing on
    /// the phone knows which two axes "custom" would mean.
    func testAConfigurationThePresetsDoNotNameCannotBeChosen() {
        let bundle = bundle { _ in XCTFail("custom is a report, not a choice") }
        session(
            bundle, gate: .ready,
            permission: .object([
                "provider": .string("codex"),
                "approval": .string("never"),
                "sandbox": .string("read-only"),
            ]))

        XCTAssertFalse(bundle.setPermission("custom", of: agent))
    }

    /// A layer reporting no permissions at all is not a permissive one, and
    /// nothing is sent for it.
    func testALayerWithNoPermissionsIsNotWrittenTo() {
        let bundle = bundle { _ in XCTFail("a layer with no permissions was written to") }
        session(bundle, gate: .ready)
        XCTAssertFalse(bundle.setPermission("plan", of: agent))
    }

    // MARK: - The agent as a thing that exists

    func testRenamingSendsTheNameWithoutTheSpaceAroundIt() {
        var written: [JSONValue] = []
        let bundle = bundle { written.append($0) }
        session(bundle, gate: .unavailable)

        XCTAssertTrue(bundle.rename("  tidy-the-parser \n", of: agent))
        XCTAssertEqual(written, [.object([
            "command": .string("rename_agent"),
            "agent": .string(agent.description),
            "name": .string("tidy-the-parser"),
        ])])
    }

    /// Renaming and deleting are about the agent and not about how it runs, so
    /// a session that refuses model changes still answers to both.
    func testANamelessRenameNeverLeavesThePhone() {
        let bundle = bundle { _ in XCTFail("a nameless rename left the phone") }
        session(bundle, gate: .ready)
        XCTAssertFalse(bundle.rename("   ", of: agent))
    }

    func testDeletingAsksAndOnlyTheHostsAnswerSaysItHappened() throws {
        var written: [JSONValue] = []
        var dispatched: OpId?
        let bundle = StoreBundle(account: AccountId("test"))
        bundle.dispatch = { command in
            guard case .shared(let body) = command else { return nil }
            written.append(body)
            let op = OpId(UUID())
            dispatched = op
            return op
        }
        let store = session(bundle, gate: .ptySettingsUnavailable)

        XCTAssertTrue(bundle.delete(agent))
        XCTAssertEqual(written, [.object([
            "command": .string("delete_agent"),
            "agent": .string(agent.description),
        ])])
        XCTAssertFalse(store.deleted, "the press is not the outcome")

        let op = try XCTUnwrap(dispatched)
        bundle.apply(.opResult(OpResult(op: op, outcome: .agentDeleted)))
        XCTAssertTrue(store.deleted)
    }

    /// A deletion answered for some other agent's operation says nothing about
    /// this one, and must not close a conversation somebody is reading.
    func testADeletionSomebodyElseAskedForClosesNothingHere() {
        let bundle = bundle { _ in }
        let store = session(bundle, gate: .ready)
        bundle.apply(.opResult(OpResult(op: OpId(UUID()), outcome: .agentDeleted)))
        XCTAssertFalse(store.deleted)
    }
}
