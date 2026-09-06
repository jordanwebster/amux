import AmuxCore
import Foundation
import XCTest
@testable import AmuxFeatures

/// What the conversation's chrome says, without drawing it.
///
/// Two facts are worth pinning here rather than only in a screenshot: what the
/// changes chip counts, and where the pill's three words come from. A capture
/// proves they are drawn; these prove they are right.
@MainActor
final class ConversationChromeTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)
    private let host = HostId(UUID(uuidString: "00000000-0000-0000-0000-0000000000AA")!)
    private let agent = AgentId(UUID(uuidString: "00000000-0000-0000-0000-0000000000B1")!)

    private func document(_ lines: [String]...) -> DiffDocument {
        DiffDocument(
            numbering: .absolute,
            hunks: lines.enumerated().map { index, rows in
                DiffHunk(oldStart: UInt32(index + 1), newStart: UInt32(index + 1),
                         header: nil, lines: rows)
            },
            truncated: false)
    }

    /// The chip counts the patch it opens rather than the agent card's totals
    /// for its last turn. A number that disagreed with the page behind it
    /// would be worse than no number at all.
    func testTheChipCountsEveryHunkInTheDocument() {
        let changes = document(
            ["  context", "- gone", "- gone too", "+ new"],
            ["- also gone", "+ another", "  context"])

        XCTAssertEqual(changes.insertions, 2)
        XCTAssertEqual(changes.deletions, 3)
    }

    /// A document with nothing but context is not a change, and a chip that
    /// appeared for it would offer a page with nothing on it.
    func testAPatchOfNothingButContextIsNotAChange() {
        XCTAssertTrue(document(["  context", "  more context"]).isEmpty)
        XCTAssertTrue(document().isEmpty)
        XCTAssertFalse(document(["+ one line"]).isEmpty)
    }

    /// The pill names the agent, its machine and its directory, and it takes
    /// all three from the fleet rather than keeping any of them itself.
    func testThePillNamesTheAgentItsMachineAndItsDirectory() {
        let fleet = FleetStore(now: now)
        fleet.apply(.fleet(Fleet(
            epoch: 1,
            agents: [AgentCard(
                agent: Agent(
                    id: agent, hostId: host, name: "refactor-auth", command: "claude",
                    workingDir: "~/src/amux", kind: .claude(driver: .pty),
                    createdAt: now.addingTimeInterval(-3600)),
                displayName: "refactor-auth", attention: .working, phase: .running,
                lastActivity: now)],
            hosts: [HostState(
                entry: HostEntry(id: host, name: "Studio", online: true), epoch: 1)],
            reconciled: true)))

        let subject = ConversationSubject(agent: agent, in: fleet)

        XCTAssertEqual(subject.name, "refactor-auth")
        XCTAssertEqual(subject.place, "Studio · ~/src/amux")
    }

    /// A conversation can be opened on the frame the tap happened, before the
    /// fleet that names the agent has arrived. It still says whose it is.
    func testAConversationOpenedBeforeTheFleetStillNamesItself() {
        let subject = ConversationSubject(agent: agent, in: FleetStore(now: now))

        XCTAssertEqual(subject.name, agent.description)
        XCTAssertEqual(subject.place, "")
    }
}
