import Foundation
import XCTest

@testable import AmuxCore

@MainActor
final class NewAgentTests: XCTestCase {
    private final class Sent {
        var commands: [BridgeCommand] = []
        var ops: [OpId] = []
    }

    private let studio = HostId(UUID(uuidString: "40000000-0000-0000-0000-000000000001")!)
    private let mini = HostId(UUID(uuidString: "40000000-0000-0000-0000-000000000002")!)

    private func bundle() -> (StoreBundle, Sent) {
        let sent = Sent()
        let stores = StoreBundle(account: AccountId("test"))
        stores.dispatch = { command in
            let op = OpId(UUID())
            sent.commands.append(command)
            sent.ops.append(op)
            return op
        }
        return (stores, sent)
    }

    /// Opening the screen on a machine asks that machine what it has.
    func testOpeningAsksTheMachineForItsDirectories() {
        let (stores, sent) = bundle()

        stores.startNewAgent(on: studio)

        XCTAssertEqual(
            sent.commands,
            [.listRepositories(host: studio, query: nil, limit: NewAgentStore.limit)])
        XCTAssertEqual(stores.newAgent.machine, studio)
        XCTAssertEqual(stores.newAgent.listing, .asking)
    }

    /// Pointing at another machine asks that one instead, and drops everything
    /// the last one offered: a directory on one machine names nothing on
    /// another.
    func testAnotherMachineIsAskedAndTheOldAnswerGoes() {
        let (stores, sent) = bundle()
        stores.startNewAgent(on: studio)
        deliver(stores, sent, .repositories(
            host: studio,
            recent: [Project(path: "~/src/amux", name: "amux")],
            repositories: [], roots: ["~/src"]))
        XCTAssertEqual(stores.newAgent.directory, "~/src/amux")

        stores.point(at: mini)

        XCTAssertEqual(stores.newAgent.machine, mini)
        XCTAssertEqual(stores.newAgent.directory, "")
        XCTAssertTrue(stores.newAgent.recent.isEmpty)
        XCTAssertEqual(
            sent.commands.last,
            .listRepositories(host: mini, query: nil, limit: NewAgentStore.limit))
    }

    /// A machine that will not list its directories leaves the typed path, not
    /// an error to recover from.
    func testAMachineThatWillNotListLeavesATypedPath() {
        let (stores, sent) = bundle()
        stores.startNewAgent(on: studio)

        deliver(stores, sent, .repositoriesUnavailable(host: studio))

        XCTAssertEqual(stores.newAgent.listing, .unavailable)
    }

    /// Starting Claude names the SDK driver. This is the whole point of the
    /// command: a request that left the driver unsaid would start a terminal
    /// session that looks like every other agent until somebody tried to do
    /// something only the SDK can do.
    func testStartingClaudeNamesTheSdkDriver() throws {
        let (stores, sent) = bundle()
        stores.startNewAgent(on: studio)
        deliver(stores, sent, .repositories(
            host: studio, recent: [Project(path: "~/src/amux", name: "amux")],
            repositories: [], roots: ["~/src"]))

        XCTAssertTrue(stores.startAgent())

        XCTAssertEqual(
            sent.commands.last,
            .createAgent(host: studio, directory: "~/src/amux", name: "amux", agent: .claude))
        // And on the wire, where the bridge reads it.
        let json = try JSONSerialization.jsonObject(
            with: AmuxJSON.encoder.encode(sent.commands.last)) as? [String: Any]
        let agent = try XCTUnwrap(json?["agent"] as? [String: Any])
        XCTAssertEqual(agent["provider"] as? String, "claude")
        XCTAssertEqual(agent["driver"] as? String, "sdk")
        XCTAssertEqual(json?["command"] as? String, "create_agent")
    }

    /// The other driver cannot be spelled at all. A request naming it is not a
    /// request this app makes, and reading one back is refused rather than
    /// quietly taken as a Claude agent.
    func testATerminalDriverIsNotAThingThisAppCanAskFor() {
        let json = Data(#"{"provider":"claude","driver":"pty"}"#.utf8)

        XCTAssertThrowsError(try AmuxJSON.decoder.decode(NewAgentKind.self, from: json))
    }

    /// Codex left alone carries no model, so the machine starts it under its
    /// own default rather than under one this app made up.
    func testCodexWithNoModelChosenLeavesItToTheMachine() {
        let (stores, sent) = bundle()
        stores.startNewAgent(on: studio)
        deliver(stores, sent, .repositories(
            host: studio, recent: [Project(path: "~/work/atlas", name: "atlas")],
            repositories: [], roots: ["~/work"]))
        stores.newAgent.choose(provider: .codex)

        XCTAssertTrue(stores.startAgent())

        XCTAssertEqual(
            sent.commands.last,
            .createAgent(
                host: studio, directory: "~/work/atlas", name: "atlas",
                agent: .codex(model: nil)))
    }

    /// Codex carries the model that was chosen.
    func testCodexCarriesTheChosenModel() {
        let (stores, sent) = bundle()
        stores.startNewAgent(on: studio)
        deliver(stores, sent, .repositories(
            host: studio, recent: [Project(path: "~/work/atlas", name: "atlas")],
            repositories: [], roots: ["~/work"]))
        stores.newAgent.choose(provider: .codex)
        stores.newAgent.choose(model: "gpt-5.2-mini")

        XCTAssertTrue(stores.startAgent())

        XCTAssertEqual(
            sent.commands.last,
            .createAgent(
                host: studio, directory: "~/work/atlas", name: "atlas",
                agent: .codex(model: "gpt-5.2-mini")))
    }

    /// A path the machine rejects is said in the machine's own words, and
    /// nothing is started.
    func testAPathTheMachineRejectsIsReported() throws {
        let (stores, sent) = bundle()
        stores.startNewAgent(on: studio)
        deliver(stores, sent, .repositoriesUnavailable(host: studio))
        stores.newAgent.typed = "~/not/here"
        stores.newAgent.choose(directory: stores.newAgent.typedPath)

        XCTAssertTrue(stores.startAgent())
        XCTAssertTrue(stores.newAgent.starting)

        deliver(stores, sent, .failed(try failure("no such directory: ~/not/here")))

        XCTAssertFalse(stores.newAgent.starting)
        XCTAssertEqual(stores.newAgent.failure, "no such directory: ~/not/here")
        XCTAssertNil(stores.newAgent.created)
    }

    /// Nothing chosen, nothing sent. A screen with no directory has nothing to
    /// start and says so rather than sending a create with an empty path.
    func testNothingIsStartedWithoutADirectory() {
        let (stores, sent) = bundle()
        stores.startNewAgent(on: studio)

        XCTAssertFalse(stores.startAgent())
        XCTAssertEqual(sent.commands.count, 1)
    }

    /// The agent the machine started is in the fleet at once, so the
    /// conversation this phone opens on it can name where it runs instead of
    /// showing a blank line until the next inventory.
    func testTheStartedAgentNamesWhereItRuns() {
        let (stores, sent) = bundle()
        stores.startNewAgent(on: studio)
        deliver(stores, sent, .repositories(
            host: studio, recent: [Project(path: "~/src/amux", name: "amux")],
            repositories: [], roots: ["~/src"]))
        stores.apply([.fleet(Fleet(
            epoch: 1, agents: [],
            hosts: [HostState(entry: HostEntry(id: studio, name: "Studio", online: true), epoch: 1)],
            reconciled: true))])
        XCTAssertTrue(stores.startAgent())

        let started = Agent(
            id: AgentId(UUID()), hostId: studio, name: "amux", command: "claude",
            workingDir: "~/src/amux", kind: .claude(driver: .sdk), createdAt: Date())
        deliver(stores, sent, .agentCreated(started))

        XCTAssertEqual(stores.newAgent.created, started)
        XCTAssertEqual(stores.fleet.rows.map(\.id), [started.id])
        XCTAssertEqual(stores.fleet.rows.first?.workingDirectory, "~/src/amux")
        XCTAssertEqual(stores.fleet.host(studio)?.name, "Studio")
    }

    /// Searching filters what the machine already sent. A machine that answered
    /// under the limit has nothing more to give, so nothing is asked again.
    func testSearchingFiltersWhatTheMachineAlreadySent() {
        let (stores, sent) = bundle()
        stores.startNewAgent(on: studio)
        deliver(stores, sent, .repositories(
            host: studio,
            recent: [Project(path: "~/src/amux", name: "amux")],
            repositories: [
                Project(path: "~/work/atlas", name: "atlas"),
                Project(path: "~/work/ledger", name: "ledger"),
            ],
            roots: ["~/src", "~/work"]))

        stores.newAgent.query = "led"
        stores.searchDirectories()

        XCTAssertEqual(stores.newAgent.found.map(\.name), ["ledger"])
        XCTAssertFalse(stores.newAgent.searchesTheMachine)
        XCTAssertEqual(sent.commands.count, 1)
    }

    // MARK: - Helpers

    /// Answers the last request this test dispatched, under the identifier the
    /// store is actually waiting on.
    private func deliver(_ stores: StoreBundle, _ sent: Sent, _ outcome: OpOutcome) {
        guard let op = sent.ops.last else { return XCTFail("nothing was dispatched") }
        stores.apply([.opResult(OpResult(op: op, outcome: outcome))])
    }

    private func failure(_ message: String) throws -> OpFailure {
        let json = Data(#"{"error":"invalid_request","message":"\#(message)"}"#.utf8)
        return try AmuxJSON.decoder.decode(OpFailure.self, from: json)
    }
}
