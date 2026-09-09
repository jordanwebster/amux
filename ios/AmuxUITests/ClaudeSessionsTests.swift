import Foundation
import XCTest

/// Creates and opens Claude sessions through the phone, then reads their
/// identities and inputs from the daemon that owns them.
final class ClaudeSessionsTests: JourneyCase {
    private let refusal = "model, effort and preset changes are unavailable for Claude PTY sessions"

    func testSDKCreationAndBothDriversRoundTripThroughTheHost() throws {
        continueAfterFailure = false
        let runner = try Runner()
        let control = try Lines(address: runner.control)
        let env = ProcessInfo.processInfo.environment
        let pty = try XCTUnwrap(env["AMUX_PTY_AGENT"])
        let directory = try XCTUnwrap(env["AMUX_DIRECTORY"])
        let app = launch(runner)
        defer { try? write("claude-sessions.json") }
        try pairThroughTheDoor(runner)
        waitFor(app, "home.row.\(runner.agent)", "the existing SDK agent never reached the fleet")
        let before = try inventory(control, runner.host)
        XCTAssertEqual(before.count, 2)

        press(app, "home.newAgent")
        waitFor(app, "new-agent", "New Agent did not open")
        press(app, "new-agent.host.\(try XCTUnwrap(env["AMUX_HOST_ID"]))")
        XCTAssertTrue(waitUntil {
            self.said((try? self.declared(runner, settling: false)) ?? [], "new-agent.directory")?.value == directory
        }, "the host's recent working directory was not selected")
        try start(app, runner)
        waitFor(app, "conversation", "the created SDK agent did not open")
        waitFor(app, "composer", "the created SDK agent has no composer")
        let after = try inventory(control, runner.host)
        let known = Set(before.compactMap { $0["id"] as? String })
        let added = after.filter { !known.contains($0["id"] as? String ?? "") }
        XCTAssertEqual(added.count, 1)
        let created = try XCTUnwrap(added.first)
        let createdID = try XCTUnwrap(created["id"] as? String)
        XCTAssertEqual(created["kind"] as? String, "claude")
        XCTAssertEqual(created["driver"] as? String, "sdk")
        XCTAssertEqual(after.filter { $0["driver"] as? String == "pty" }.count, 1)
        record["created"] = created
        record["inventoryBefore"] = before
        record["inventoryAfter"] = after
        try prompt(app, runner, agent: createdID, text: "Created SDK prompt", reply: "The SDK session received your prompt.")
        record["createdConversation"] = try reading(runner, createdID, layer: "claude_sdk")

        pressTab(app, "Agents")
        press(app, "home.row.\(runner.agent)")
        waitFor(app, "composer", "the existing SDK agent has no composer")
        try prompt(app, runner, agent: runner.agent, text: "Existing SDK prompt", reply: "The SDK session received your prompt.")
        record["sdkConversation"] = try reading(runner, runner.agent, layer: "claude_sdk")
        photograph(app, "sdk-open")
        press(app, "composer.model")
        waitFor(app, "settings.model.haiku", "the SDK session did not offer its model")
        press(app, "settings.model.haiku")
        XCTAssertTrue(waitUntil {
            (try? self.declared(runner, settling: false))?.contains {
                $0.identifier == "composer.model" && $0.value.lowercased().contains("haiku")
            } == true
        }, "the SDK model change was not reported back to the phone")
        record["sdkModel"] = said(try declared(runner), "composer.model")?.value ?? ""

        pressTab(app, "Agents")
        press(app, "home.row.\(pty)")
        waitFor(app, "composer", "the existing PTY agent has no composer")
        try prompt(app, runner, agent: pty, text: "Existing PTY prompt", reply: "The PTY session received your prompt.")
        record["ptyConversation"] = try reading(runner, pty, layer: "claude_pty")
        photograph(app, "pty-open")
        let ptyBefore = try observe(control, pty)
        // The screen disables this setting. Exercise its typed command too,
        // so the proof includes the shared gate and the absence of host input.
        try door(runner, .init(kind: "setModel", agent: pty, name: "haiku"))
        var refused: [String: Any] = [:]
        XCTAssertTrue(waitUntil {
            refused = (try? self.reading(runner, pty, layer: "claude_pty")) ?? [:]
            return ((refused["results"] as? [[String: Any]]) ?? []).contains {
                (($0["outcome"] as? [String: Any])?["error"] as? [String: Any])?["message"] as? String == self.refusal
            }
        }, "the PTY model change has no named shared-gate refusal")
        XCTAssertEqual((refused["settingsGate"] as? [String: Any])?["gate"] as? String, "pty_settings_unavailable")
        record["ptyRefusal"] = refused
        XCTAssertTrue(NSDictionary(dictionary: try observe(control, pty)).isEqual(to: ptyBefore))

        // A directory that does not exist is refused by normal host validation.
        pressTab(app, "Agents")
        press(app, "home.newAgent")
        waitFor(app, "new-agent", "New Agent did not reopen")
        press(app, "new-agent.directory")
        waitFor(app, "new-agent.browse", "the directory chooser did not open")
        try door(runner, .init(kind: "type", text: directory + "/does-not-exist-for-sdk-journey", identifier: "new-agent.typed"))
        press(app, "new-agent.typed.use")
        try start(app, runner)
        waitFor(app, "new-agent.refusal", "the host's refused creation has no designed failure state")
        XCTAssertFalse(element(app, "conversation").exists)
        photograph(app, "refused")
        record["creationRefusal"] = said(try declared(runner), "new-agent.refusal")?.value ?? ""
        let final = try inventory(control, runner.host)
        XCTAssertEqual(Set(final.compactMap { $0["id"] as? String }), Set(after.compactMap { $0["id"] as? String }))
        record["inventoryAfterRefusal"] = final
        record["observed"] = [
            createdID: try observe(control, createdID),
            runner.agent: try observe(control, runner.agent),
            pty: try observe(control, pty),
        ]
        try write("claude-sessions.json")
    }

    private func inventory(_ control: Lines, _ host: String) throws -> [[String: Any]] {
        let answer = try control.ask(["Inventory": ["daemon": host]])
        return try XCTUnwrap((answer["Ack"] as? [String: Any])?["agents"] as? [[String: Any]])
    }

    private func observe(_ control: Lines, _ agent: String) throws -> [String: Any] {
        let answer = try control.ask(["AgentObserve": ["agent": agent]])
        return try XCTUnwrap(answer["Ack"] as? [String: Any])
    }

    private func reading(_ runner: Runner, _ agent: String, layer: String) throws -> [String: Any] {
        let response = try door(runner, .init(kind: "conversation", agent: agent))
        let conversation = try XCTUnwrap(response["conversation"] as? [String: Any])
        XCTAssertEqual(conversation["agent"] as? String, agent)
        XCTAssertEqual((conversation["gate"] as? [String: Any])?["layer"] as? String, layer)
        let entries = try XCTUnwrap(conversation["entries"] as? [[String: Any]])
        XCTAssertFalse(entries.isEmpty)
        XCTAssertTrue(entries.allSatisfy { $0["layer"] as? String == layer })
        return conversation
    }

    private func prompt(_ app: XCUIApplication, _ runner: Runner, agent: String, text: String, reply: String) throws {
        try door(runner, .init(kind: "awaitSendable", agent: agent, seconds: 60))
        let field = app.textViews.matching(identifier: "composer.field").firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: waiting))
        field.tap()
        field.typeText(text)
        press(app, "composer.send")
        XCTAssertTrue(waitUntil {
            self.element(app, "transcript.prose").exists
                && app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", reply)).firstMatch.exists
        }, "the host's reply did not render in the conversation")
        try door(runner, .init(kind: "awaitSendable", agent: agent, seconds: 60))
        XCTAssertEqual(transcriptRows(app).filter { $0 == "transcript.prompt" }.count, 1)
    }

    private func start(_ app: XCUIApplication, _ runner: Runner) throws {
        let button = element(app, "new-agent.start")
        var previous = CGRect.null
        XCTAssertTrue(waitUntil(within: 10) {
            let now = button.exists ? button.frame : .null
            defer { previous = now }
            return now == previous && !now.isNull && button.isHittable
        })
        XCTAssertEqual(said(try declared(runner), "new-agent.provider.claude")?.value, "chosen")
        press(app, "new-agent.start")
    }
}
