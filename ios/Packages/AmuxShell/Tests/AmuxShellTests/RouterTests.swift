import AmuxCore
import XCTest

@testable import AmuxShell

/// Records what it was asked to load, and what the router had already done by
/// the time it was asked.
@MainActor
private final class RecordingLoader: RouteLoader {
    private(set) var loaded: [Route] = []
    /// The stack the router was showing at the moment each load was asked for.
    private(set) var pathWhenAsked: [[Route]] = []
    weak var router: Router?
    /// Work that has been started and has not finished, standing in for a
    /// fetch from a host.
    private(set) var outstanding = 0

    func load(_ route: Route) {
        loaded.append(route)
        pathWhenAsked.append(router?.path ?? [])
        outstanding += 1
    }
}

@MainActor
final class RouterTests: XCTestCase {
    private func agent() -> AgentId { AgentId(UUID()) }

    func testOpeningPushesOnTheSameTurnOfTheRunLoop() {
        let router = Router()
        let agent = agent()

        // No `await` between the call and the assertion, and none is
        // available: `open` is not `async`. Navigation is what happened, not
        // what was started.
        router.open(.conversation(agent))

        XCTAssertEqual(router.path, [.conversation(agent)])
        XCTAssertEqual(router.top, .conversation(agent))
    }

    func testThePageIsUpBeforeAnythingIsAskedToLoadIt() {
        let loader = RecordingLoader()
        let router = Router(loader: loader)
        loader.router = router
        let agent = agent()

        router.open(.conversation(agent))

        XCTAssertEqual(loader.loaded, [.conversation(agent)])
        // What the loader saw: the page it is being asked to fill was already
        // on the stack. Loading first and pushing after would show the empty
        // stack here.
        XCTAssertEqual(loader.pathWhenAsked, [[.conversation(agent)]])
    }

    func testNavigationDoesNotWaitForTheLoadToFinish() {
        let loader = RecordingLoader()
        let router = Router(loader: loader)
        loader.router = router
        let first = agent()
        let second = agent()

        router.open(.conversation(first))
        router.open(.conversation(second))

        // Two loads started, neither finished, and both pages are on the
        // stack: nothing about where the app is depends on a host answering.
        XCTAssertEqual(loader.outstanding, 2)
        XCTAssertEqual(router.path, [.conversation(first), .conversation(second)])
    }

    func testOpeningARouteFromAnotherTabBringsItsTabWithIt() {
        let router = Router()
        XCTAssertEqual(router.tab, .agents)

        router.open(.pairByCode)

        XCTAssertEqual(router.tab, .hosts)
        XCTAssertEqual(router.path, [.pairByCode])
        // The tab that was on show is where it was left.
        XCTAssertEqual(router.path(.agents), [])
    }

    func testEachTabKeepsItsOwnStack() {
        let router = Router()
        let agent = agent()

        router.open(.conversation(agent))
        router.open(.help)

        XCTAssertEqual(router.tab, .you)
        XCTAssertEqual(router.path(.agents), [.conversation(agent)])
        XCTAssertEqual(router.path(.you), [.help])

        router.select(.agents)
        XCTAssertEqual(router.path, [.conversation(agent)])
    }

    func testTheSystemCanWriteBackWhatTheBackGestureDid() {
        let router = Router()
        let agent = agent()
        router.open(.conversation(agent))
        router.open(.changes(agent))

        router.setPath([.conversation(agent)], for: .agents)

        XCTAssertEqual(router.path, [.conversation(agent)])
    }

    func testPoppingAnEmptyStackIsNotAnError() {
        let router = Router()
        router.pop()
        XCTAssertEqual(router.path, [])
    }

    func testPopToRootClearsOnlyTheTabOnShow() {
        let router = Router()
        let agent = agent()
        router.open(.conversation(agent))
        router.open(.pairByCode)

        router.popToRoot()

        XCTAssertEqual(router.path(.hosts), [])
        XCTAssertEqual(router.path(.agents), [.conversation(agent)])
    }

    func testEveryRouteBelongsToOneTab() {
        XCTAssertEqual(Route.conversation(agent()).tab, .agents)
        XCTAssertEqual(Route.changes(agent()).tab, .agents)
        XCTAssertEqual(Route.newAgent.tab, .hosts)
        XCTAssertEqual(Route.pairByCode.tab, .hosts)
        XCTAssertEqual(Route.host(HostId(UUID())).tab, .hosts)
        XCTAssertEqual(Route.accounts.tab, .you)
        XCTAssertEqual(Route.appearance.tab, .you)
        XCTAssertEqual(Route.help.tab, .you)
    }
}
