import Foundation
import XCTest
@testable import AmuxCore

/// The panels, read from the asks the bridge actually projects.
///
/// The file this reads is written by the Rust side from recorded sessions —
/// a real permission with a real suggestion, a real multi-select question, a
/// real plan, a real Codex approval — so a change to a provider's vocabulary
/// fails here rather than at a panel that quietly stops offering something.
final class AskPanelTests: XCTestCase {
    private func pinned(_ name: String) throws -> [Ask] {
        let url = try XCTUnwrap(
            Bundle.module.url(forResource: "asks", withExtension: "json"),
            "the pinned ask projection is missing from the test bundle")
        let all = try AmuxJSON.decoder.decode(
            [String: [Ask]].self, from: Data(contentsOf: url))
        return try XCTUnwrap(all[name], "the pinned asks have no \(name)")
    }

    func testAPermissionCarriesItsCommandVerbatimAndTheHostsOwnScope() throws {
        let panel = try XCTUnwrap(pinned("permission").panel)
        guard case .permission(let permission) = panel.kind else {
            return XCTFail("expected a permission, got \(panel.kind)")
        }
        XCTAssertEqual(permission.headline, "Wants to run a command")
        XCTAssertEqual(
            permission.subject,
            "printf allow-scoped > allow-scoped.txt; printf allow-scoped")
        XCTAssertTrue(permission.literal)
        XCTAssertEqual(permission.purpose, "Create file and print content as requested")
        XCTAssertNil(permission.unanswerable)
        let scope = try XCTUnwrap(permission.scope, "the host offered a directory grant")
        XCTAssertEqual(scope.title, "Always allow access")
        XCTAssertEqual(scope.suggestion, 0)
        XCTAssertNotNil(scope.directory)
    }

    /// Claude builds its permission menu from the host's suggestions and the
    /// core refuses every answer to a menu shape nobody has checked. A panel
    /// that offered buttons there would offer refusals.
    func testAnUncheckedMenuShapeOffersNothing() throws {
        let ask = try XCTUnwrap(pinned("permission").first)
        var body = ask.body
        guard case .object(var fields) = body, case .object(var kind) = fields["kind"] else {
            return XCTFail("expected an ask with a kind")
        }
        kind["suggestions"] = .array([])
        fields["kind"] = .object(kind)
        body = .object(fields)
        let panel = try XCTUnwrap(Ask(layer: .claudePty, body: body).panel)
        guard case .permission(let permission) = panel.kind else {
            return XCTFail("expected a permission, got \(panel.kind)")
        }
        XCTAssertNil(permission.scope)
        XCTAssertNotNil(permission.unanswerable)
    }

    func testAMultiSelectQuestionKeepsItsOptionsAndTheirDescriptions() throws {
        let panel = try XCTUnwrap(pinned("question").panel)
        guard case .question(let questions) = panel.kind else {
            return XCTFail("expected a question, got \(panel.kind)")
        }
        XCTAssertEqual(questions.count, 1)
        let question = try XCTUnwrap(questions.first)
        XCTAssertEqual(question.header, "Tools")
        XCTAssertEqual(question.prompt, "Which tools would you like to use?")
        XCTAssertTrue(question.multiSelect)
        XCTAssertEqual(question.options.map(\.label), ["Hammer", "Saw", "Drill"])
        XCTAssertEqual(
            question.options.first?.description,
            "A tool for driving nails and general striking")
    }

    /// A plan arrives as a permission whose payload is the plan. It is drawn
    /// as a plan and answered as one, which is what keeps Approve and Send
    /// Back off every other permission.
    func testAPlanIsAPermissionCarryingMarkdown() throws {
        let panel = try XCTUnwrap(pinned("plan").panel)
        guard case .plan(let plan) = panel.kind else {
            return XCTFail("expected a plan, got \(panel.kind)")
        }
        XCTAssertTrue(plan.markdown.hasPrefix("# Context"))
        XCTAssertTrue(plan.markdown.contains("# Verification"))
        XCTAssertNotNil(plan.path)
    }

    func testCodexOffersItsOwnDecisionsInItsOwnOrder() throws {
        let panel = try XCTUnwrap(pinned("codex-approval").panel)
        guard case .approval(let approval) = panel.kind else {
            return XCTFail("expected an approval, got \(panel.kind)")
        }
        XCTAssertEqual(approval.headline, "Wants to run a command")
        XCTAssertEqual(approval.subject, "/bin/zsh -lc '/usr/bin/touch <MACHINE_PATH>'")
        XCTAssertEqual(
            approval.reason, "Allow the exact requested command to create the approval file?")
        XCTAssertEqual(approval.choices.map(\.label),
                       ["Accept", "Accept and Allow Similar", "Cancel"])
        // The middle choice is an object-valued decision the V1 backend does
        // not take. It is listed, and it cannot be pressed.
        XCTAssertEqual(approval.choices.map(\.decision), ["accept", nil, "cancel"])
    }

    // MARK: - What an answer becomes

    private var agent: AgentId { AgentId("00000000-0000-0000-0000-000000000001")! }

    func testAnAllowIsAddressedToTheAskClaudeNamed() throws {
        let panel = try XCTUnwrap(pinned("permission").panel)
        XCTAssertEqual(panel.address, .claude(ask: 0))
        let command = try XCTUnwrap(panel.command(.allowOnce, agent: agent))
        XCTAssertEqual(command["command"]?.stringValue, "claude")
        XCTAssertEqual(command["claude_command"]?.stringValue, "answer_ask")
        XCTAssertEqual(command["ask"]?.intValue, 0)
        XCTAssertEqual(command["answer"]?["answer"]?.stringValue, "permission")
        XCTAssertEqual(command["answer"]?["permission"]?.stringValue, "allow_once")
    }

    func testAScopedAllowCarriesWhichSuggestionWasTaken() throws {
        let panel = try XCTUnwrap(pinned("permission").panel)
        let command = try XCTUnwrap(panel.command(.allowScoped(suggestion: 0), agent: agent))
        XCTAssertEqual(command["answer"]?["permission"]?.stringValue, "allow_scoped")
        XCTAssertEqual(command["answer"]?["suggestion"]?.intValue, 0)
    }

    func testAPlanVerdictKeepsLaterEditsBeingAskedAbout() throws {
        let panel = try XCTUnwrap(pinned("plan").panel)
        let approve = try XCTUnwrap(panel.command(.approvePlan, agent: agent))
        XCTAssertEqual(approve["answer"]?["plan"]?.stringValue, "approve_manual")
        let back = try XCTUnwrap(
            panel.command(.sendPlanBack(feedback: "Leave the daemon alone"), agent: agent))
        XCTAssertEqual(back["answer"]?["plan"]?.stringValue, "request_changes")
        XCTAssertEqual(back["answer"]?["feedback"]?.stringValue, "Leave the daemon alone")
    }

    func testEveryQuestionIsAnsweredEvenWhenOnlyOneWasTapped() throws {
        let panel = try XCTUnwrap(pinned("question").panel)
        let command = try XCTUnwrap(
            panel.command(.answered([AskDecision.Reply(selected: [0, 2])]), agent: agent))
        XCTAssertEqual(command["answer"]?["answer"]?.stringValue, "question")
        let answers = try XCTUnwrap(command["answer"]?["answers"]?.arrayValue)
        XCTAssertEqual(answers.count, 1)
        XCTAssertEqual(answers[0]["selected"]?.arrayValue?.compactMap(\.intValue), [0, 2])
    }

    func testACodexDecisionGoesBackWithTheRequestItAnswers() throws {
        let panel = try XCTUnwrap(pinned("codex-approval").panel)
        let command = try XCTUnwrap(panel.command(.decided("accept"), agent: agent))
        XCTAssertEqual(command["command"]?.stringValue, "codex")
        XCTAssertEqual(command["codex_command"]?.stringValue, "answer")
        XCTAssertEqual(command["decision"]?.stringValue, "accept")
        XCTAssertEqual(command["request_id"]?.intValue, 0)
    }

    /// The two vocabularies never cross: a Claude answer addressed at Codex,
    /// or the other way round, is not a command this build will send.
    func testAnAnswerFromTheWrongVocabularyIsNotSent() throws {
        let claude = try XCTUnwrap(pinned("permission").panel)
        XCTAssertNil(claude.command(.decided("accept"), agent: agent))
        let codex = try XCTUnwrap(pinned("codex-approval").panel)
        XCTAssertNil(codex.command(.allowOnce, agent: agent))
    }

    /// An ask the person has already answered draws nothing. Putting the same
    /// question back while the answer travels invites answering it twice.
    func testAnAnsweredAskIsNoLongerAPanel() throws {
        let ask = try XCTUnwrap(pinned("permission").first)
        guard case .object(var fields) = ask.body else { return XCTFail("expected an object") }
        fields["state"] = .object([
            "state": .string("answered_optimistic"),
            "op": .string("00000000-0000-0000-0000-0000000000AA"),
            "answer": .object(["answer": .string("permission"),
                               "permission": .string("allow_once")]),
        ])
        XCTAssertNil(Ask(layer: .claudePty, body: .object(fields)).panel)
    }
}
