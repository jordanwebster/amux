import Foundation
import XCTest

@testable import AmuxCore

@MainActor
final class NewAgentTests: XCTestCase {
    private let listed = Directories(
        recent: [Directory(path: "/src/amux", name: "amux", lastUsedMs: 5)],
        repositories: [
            Directory(path: "/src/amux", name: "amux", lastUsedMs: nil),
            Directory(path: "/src/relay", name: "relay", lastUsedMs: nil),
        ],
        roots: ["/src"])

    func testTheMostRecentDirectoryIsPrefilledAndNamedNewOnItsHost() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.remember([AgentRow(row: Cards.row(1, "amux"), unread: false)])
        store.listed(.success(listed), asked: store.asking())
        XCTAssertEqual(store.listing, .ready)
        XCTAssertEqual(store.directory, "/src/amux")
        XCTAssertEqual(store.name, "amux-2", "amux is taken on the desk")
        XCTAssertTrue(store.ready)
        XCTAssertEqual(
            store.request,
            NewAgent(hostId: Cards.desk.bytes, kind: .claudeSdk, cwd: "/src/amux",
                     name: "amux-2", effort: nil, mode: nil, model: nil, newWorktree: nil, permission: nil))
    }

    func testAHostThatCannotListOffersWhereItsAgentsWork() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.remember([AgentRow(row: Cards.row(1, "web"), unread: false)])
        store.listed(.failure(RuntimeFailure("not available")), asked: store.asking())
        XCTAssertEqual(store.listing, .unavailable)
        XCTAssertEqual(store.recent.map(\.path), ["/Users/pat/source/web"])
        XCTAssertEqual(store.directory, "/Users/pat/source/web")
    }

    func testChoosingAnotherHostForgetsTheLastOnesListing() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.listed(.success(listed), asked: store.asking())
        let other = HostId(UUID())
        store.point(at: other)
        XCTAssertEqual(store.listing, .none)
        XCTAssertEqual(store.directory, "")
        XCTAssertFalse(store.ready)
    }

    func testAStaleListingIsDropped() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        let stale = store.asking()
        store.point(at: HostId(UUID()))
        store.listed(.success(listed), asked: stale)
        XCTAssertEqual(store.listing, .none)
    }

    func testCodexStartsCodexAndClaudeStartsTheSdkDriver() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.choose(directory: "/src/x")
        XCTAssertEqual(store.request?.kind, .claudeSdk)
        store.choose(provider: .codex)
        XCTAssertEqual(store.request?.kind, .codex)
    }

    func testAnEmptyNameBlocksStartingAndARefusalIsSaid() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.choose(directory: "/src/x")
        store.choose(name: "  ")
        XCTAssertFalse(store.ready)
        store.choose(name: "x")
        store.starts()
        XCTAssertFalse(store.ready)
        store.started(.failure(RuntimeFailure("the host is busy")))
        XCTAssertEqual(store.failure, "the host is busy")
        store.started(.success(Cards.key(9)))
        XCTAssertEqual(store.created, Cards.key(9))
    }

    func testSearchingFiltersHereAndAsksTheHostOnlyWhenItsListWasCut() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.listed(.success(listed), asked: store.asking())
        store.query = "rel"
        XCTAssertEqual(store.found.map(\.name), ["relay"])
        XCTAssertFalse(store.searchesTheMachine)
    }
}
