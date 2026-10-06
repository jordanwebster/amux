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

    private let codex = Catalogue(
        hash: [1],
        models: [
            OfferedModel(value: "gpt-6-astra", displayName: "GPT-6-Astra", description: "", efforts: ["low", "high"], resolvedModel: "", defaultEffort: "low"),
            OfferedModel(value: "gpt-6-sol", displayName: "GPT-6-Sol", description: "", efforts: ["low"], resolvedModel: "", defaultEffort: "low"),
        ],
        commands: [],
        permissions: [
            OfferedPermission(value: "default", displayName: "Default", normal: true, neverAsks: false, settable: true, models: []),
            OfferedPermission(value: "auto", displayName: "Auto", normal: false, neverAsks: false, settable: true, models: ["gpt-6-astra"]),
            OfferedPermission(value: "locked", displayName: "Locked", normal: false, neverAsks: false, settable: false, models: []),
        ],
        modes: [
            OfferedMode(value: "default", displayName: "Default", normal: true, settable: true),
            OfferedMode(value: "plan", displayName: "Plan", normal: false, settable: true),
        ])

    func testTheChoicesComeFromTheHostsCatalogueAndGoInTheRequest() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.choose(directory: "/src/x")
        store.choose(provider: .codex)
        let asked = store.askingOffer(for: .codex)
        XCTAssertNil(store.catalogue, "nothing is offered until the host says")
        store.offered(.success(codex), for: .codex, asked: asked)
        XCTAssertEqual(store.catalogue, codex)

        XCTAssertEqual(store.offeredPermissions.map(\.value), ["default"],
                       "a permission naming models waits for one of them; one not settable is never offered")
        store.choose(model: "gpt-6-astra")
        XCTAssertEqual(store.offeredEfforts, ["low", "high"])
        XCTAssertEqual(store.offeredPermissions.map(\.value), ["default", "auto"])
        XCTAssertEqual(store.offeredModes.map(\.value), ["default", "plan"])
        store.choose(effort: "high")
        store.choose(permission: "auto")
        store.choose(mode: "plan")
        store.newWorktree = true
        XCTAssertEqual(
            store.request,
            NewAgent(hostId: Cards.desk.bytes, kind: .codex, cwd: "/src/x", name: "x",
                     effort: "high", mode: "plan", model: "gpt-6-astra", newWorktree: true,
                     permission: "auto"))

        // A model that takes neither drops them back to the host's defaults.
        store.choose(model: "gpt-6-sol")
        XCTAssertNil(store.effort)
        XCTAssertNil(store.permission)
    }

    func testAnotherProviderStartsAfreshAndOnlyCodexTakesAMode() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.choose(directory: "/src/x")
        store.choose(provider: .codex)
        store.offered(.success(codex), for: .codex, asked: store.askingOffer(for: .codex))
        store.choose(model: "gpt-6-astra")
        store.choose(mode: "plan")
        store.choose(provider: .claude)
        XCTAssertNil(store.model)
        XCTAssertNil(store.mode)
        XCTAssertNil(store.catalogue, "Claude's catalogue was not asked for")
        XCTAssertNil(store.request?.mode)
        XCTAssertNil(store.request?.newWorktree, "the switch is off until turned on")
    }

    func testAnOfferForAHostLeftBehindIsDropped() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.choose(provider: .codex)
        let stale = store.askingOffer(for: .codex)
        store.point(at: HostId(UUID()))
        store.offered(.success(codex), for: .codex, asked: stale)
        XCTAssertNil(store.offers[.codex])
        let failed = store.askingOffer(for: .codex)
        store.offered(.failure(RuntimeFailure("no route")), for: .codex, asked: failed)
        XCTAssertEqual(store.offers[.codex], .unavailable)
    }

    func testAskingBothCataloguesLeavesTheListingCurrent() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        let listing = store.asking()
        _ = store.askingOffer(for: .claude)
        _ = store.askingOffer(for: .codex)
        store.listed(.success(listed), asked: listing)
        XCTAssertEqual(store.listing, .ready)
        XCTAssertEqual(store.directory, "/src/amux")
    }
}
