import Foundation
import Observation
import XCTest
@testable import AmuxCore

@MainActor
final class FleetStoreTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)

    func testACachedHomeArrivesBeforeItIsConfirmed() {
        let store = FleetStore(now: now)
        store.apply(Made.fleet([
            Made.card(1, name: "alpha", attention: .working, minutesAgo: 2, now: now, awaiting: true),
            Made.card(2, name: "beta", attention: .idle, minutesAgo: 20, now: now, awaiting: true),
        ], reconciled: false))

        XCTAssertEqual(store.rows.map(\.name), ["alpha", "beta"])
        XCTAssertFalse(store.reconciled)
        // Rows the cache remembers are shown, and marked as unconfirmed rather
        // than dressed up as fact.
        XCTAssertEqual(store.rows.map(\.confirmed), [false, false])
    }

    /// A phone that was picked up and never heard from a host again would
    /// still say it was reconciled, because it was — before it was put down.
    /// Only a count can say a fleet arrived since, so only a count can be
    /// timed.
    func testEachConfirmedFleetMovesTheCountThatTheFlagCannot() {
        let store = FleetStore(now: now)
        let cards = [Made.card(1, name: "alpha", attention: .idle, minutesAgo: 2, now: now)]
        XCTAssertEqual(store.reconciliations, 0)

        store.apply(Made.fleet(cards, reconciled: false))
        XCTAssertEqual(store.reconciliations, 0, "a remembered fleet confirms nothing")

        store.apply(Made.fleet(cards, reconciled: true))
        XCTAssertTrue(store.reconciled)
        XCTAssertEqual(store.reconciliations, 1)

        store.apply(Made.fleet(cards, reconciled: true))
        XCTAssertEqual(store.reconciliations, 2, "the second confirmation is a second arrival")
        XCTAssertTrue(store.reconciled, "and the flag it could have been read from has not moved")
    }

    func testConfirmingIdenticalCachedContentDoesNotRebuildTheVisibleRows() {
        let store = FleetStore(now: now)
        let cards = [Made.card(1, name: "alpha", attention: .idle, minutesAgo: 2, now: now)]
        store.apply(Made.fleet(cards, reconciled: false))
        let rowsChanged = ObservationFlag()
        withObservationTracking {
            _ = store.rows
        } onChange: {
            rowsChanged.set()
        }

        store.apply(Made.fleet(cards, reconciled: true))

        XCTAssertTrue(store.reconciled)
        XCTAssertEqual(store.reconciliations, 1)
        XCTAssertFalse(
            rowsChanged.value,
            "confirmation redrew a list whose visible content was identical")
    }

    func testSyncConfirmsTheRowsWithoutRegroupingThem() {
        let store = FleetStore(now: now)
        let cached = [
            Made.card(1, name: "alpha", attention: .working, minutesAgo: 2, now: now, awaiting: true),
            Made.card(2, name: "beta", attention: .idle, minutesAgo: 20, now: now, awaiting: true),
            Made.card(3, name: "gamma", attention: .idle, minutesAgo: 40, now: now, awaiting: true),
        ]
        store.apply(Made.fleet(cached, reconciled: false))
        let placedFirst = store.rows.map(\.id)

        // The sync says beta just did something and gamma now needs you. Both
        // would sort somewhere else on a fresh screen; neither may move under
        // the user's thumb.
        store.apply(Made.fleet([
            Made.card(1, name: "alpha", attention: .working, minutesAgo: 2, now: now),
            Made.card(2, name: "beta", attention: .working, minutesAgo: 0, now: now),
            Made.card(3, name: "gamma", attention: .needsYou(why: .permission), minutesAgo: 40, now: now),
        ], reconciled: true))

        XCTAssertEqual(store.rows.map(\.id), placedFirst)
        XCTAssertEqual(store.sections.map(\.kind), [.everythingElse])
        XCTAssertTrue(store.reconciled)
        XCTAssertEqual(store.rows.map(\.confirmed), [true, true, true])

        // Regrouping is something the screen asks for, not something a sync
        // does to it.
        store.refreshOrder(now: now)
        XCTAssertEqual(store.sections.map(\.kind), [.needsYou, .everythingElse])
        XCTAssertEqual(store.sections[0].rows.map(\.name), ["gamma"])
    }

    /// Hosts answer one at a time, so rows are confirmed one at a time.
    /// Nothing waits for the slowest machine on the account, and nothing moves
    /// while the answers come in.
    func testEachRowIsConfirmedAsItsOwnHostAnswers() {
        let store = FleetStore(now: now)
        func cards(studioAwaiting: Bool, laptopAwaiting: Bool) -> [AgentCard] {
            [
                Made.card(1, name: "alpha", attention: .working, minutesAgo: 2, now: now,
                          awaiting: studioAwaiting),
                Made.card(2, name: "beta", attention: .idle, minutesAgo: 20, now: now,
                          host: Made.other, awaiting: laptopAwaiting),
                Made.card(3, name: "gamma", attention: .idle, minutesAgo: 40, now: now,
                          awaiting: studioAwaiting),
            ]
        }
        let hosts = [
            Made.hostEntry(Made.host, name: "studio"),
            Made.hostEntry(Made.other, name: "laptop"),
        ]
        store.apply(Made.fleet(cards(studioAwaiting: true, laptopAwaiting: true),
                               hosts: hosts, reconciled: false))
        let placed = store.rows.map(\.id)
        XCTAssertEqual(store.rows.filter { !$0.confirmed }.count, 3)

        // One machine has answered; the fleet as a whole has not.
        store.apply(Made.fleet(cards(studioAwaiting: false, laptopAwaiting: true),
                               hosts: hosts, reconciled: false))
        XCTAssertEqual(store.rows.map(\.confirmed), [true, false, true])
        XCTAssertFalse(store.reconciled)
        XCTAssertEqual(store.rows.map(\.id), placed)

        store.apply(Made.fleet(cards(studioAwaiting: false, laptopAwaiting: false),
                               hosts: hosts, reconciled: true))
        XCTAssertEqual(store.rows.filter { !$0.confirmed }.count, 0)
        XCTAssertEqual(store.rows.map(\.id), placed)
    }

    func testAnArrivingAgentLandsWhereTheOrderingPutsIt() {
        let store = FleetStore(now: now)
        let cached = [
            Made.card(1, name: "alpha", attention: .idle, minutesAgo: 5, now: now),
            Made.card(2, name: "beta", attention: .idle, minutesAgo: 50, now: now),
        ]
        store.apply(Made.fleet(cached, reconciled: false))
        store.apply(Made.fleet(cached + [
            Made.card(3, name: "gamma", attention: .idle, minutesAgo: 20, now: now),
        ], reconciled: true))

        XCTAssertEqual(store.rows.map(\.name), ["alpha", "gamma", "beta"])
    }

    func testADeletedAgentLeavesAndTheRestStayPut() {
        let store = FleetStore(now: now)
        let cards = (1...3).map { Made.card($0, name: "agent-\($0)", minutesAgo: Double($0), now: now) }
        store.apply(Made.fleet(cards, reconciled: true))
        store.apply(Made.fleet([cards[0], cards[2]], reconciled: true))
        XCTAssertEqual(store.rows.map(\.name), ["agent-1", "agent-3"])
    }

    func testTheSubtitleCountsWhatIsWaitingOrSaysNothingIs() {
        let store = FleetStore(now: now)
        store.apply(Made.fleet([
            Made.card(1, name: "alpha", attention: .needsYou(why: .question), minutesAgo: 5, now: now),
            Made.card(2, name: "beta", attention: .working, minutesAgo: 1, now: now),
        ], reconciled: true))
        XCTAssertEqual(store.subtitle, "1 need you · 2 agents")

        let quiet = FleetStore(now: now)
        quiet.apply(Made.fleet([
            Made.card(1, name: "alpha", attention: .working, minutesAgo: 5, now: now),
            Made.card(2, name: "beta", attention: .idle, minutesAgo: 1, now: now),
        ], reconciled: true))
        XCTAssertEqual(quiet.subtitle, "Nothing needs you · 1 running")
    }

    func testAQuietHomeSaysSomethingOnlyWhenAHostIsMissing() {
        let store = FleetStore(now: now)
        store.apply(Made.fleet(
            [Made.card(1, name: "alpha", minutesAgo: 5, now: now)],
            hosts: [Made.hostEntry(Made.host, name: "studio")],
            reconciled: true))
        XCTAssertNil(store.exceptions)

        store.apply(Made.fleet(
            [Made.card(1, name: "alpha", minutesAgo: 5, now: now)],
            hosts: [Made.hostEntry(Made.host, name: "studio"),
                    Made.hostEntry(Made.other, name: "mini", online: false)],
            reconciled: true))
        XCTAssertEqual(store.exceptions, "mini offline")

        store.apply(.connection(ConnectionUpdate(state: .disconnected, reason: .unreachable)))
        XCTAssertEqual(store.exceptions, "Offline · check your connection")

        // Every kind the core can send has words of its own; none of them is
        // a transport error read out to somebody looking at their agents.
        for reason in [OfflineReason.rejected, .timedOut, .ended, .stopped, .suspended] {
            store.apply(.connection(ConnectionUpdate(state: .disconnected, reason: reason)))
            let line = store.exceptions ?? ""
            XCTAssertTrue(line.hasPrefix("Offline · "), "\(reason) reads \(line)")
            XCTAssertFalse(line.contains("::") || line.contains("error"), "\(reason) reads \(line)")
        }
    }

    /// Put away and brought back. The phone holds no connection while it is
    /// away and says so; coming back confirms the same list where it stood,
    /// rather than emptying the home and drawing it again.
    func testGoingAwayAndComingBackConfirmsTheSameListInPlace() {
        let store = FleetStore(now: now)
        let cards = (1...3).map { Made.card($0, name: "agent-\($0)", minutesAgo: Double($0), now: now) }
        store.apply(.connection(ConnectionUpdate(state: .connected)))
        store.apply(Made.fleet(cards, reconciled: true))
        let placed = store.rows.map(\.name)
        XCTAssertTrue(store.reconciled)

        // Away: no link, and nothing this phone knows is confirmed by anybody.
        store.apply(.connection(ConnectionUpdate(state: .disconnected, reason: .suspended)))
        store.apply(Made.fleet(cards, reconciled: false))
        XCTAssertEqual(store.exceptions, "Offline · reconnecting")
        XCTAssertFalse(store.reconciled)
        XCTAssertEqual(store.rows.map(\.name), placed,
                       "being put away moved what this phone remembers")

        // Back: the connection returns and the machines answer for the same
        // rows, in the same order, with nothing left unconfirmed.
        store.apply(.connection(ConnectionUpdate(state: .connected)))
        store.apply(Made.fleet(cards, reconciled: true))
        XCTAssertNil(store.exceptions)
        XCTAssertTrue(store.reconciled)
        XCTAssertEqual(store.rows.map(\.name), placed)
    }

    func testOpeningAnAgentClearsItsUnreadWeight() {
        let store = FleetStore(now: now)
        store.apply(Made.fleet([Made.card(1, name: "alpha", minutesAgo: 5, now: now)], reconciled: true))
        XCTAssertTrue(store.rows[0].unread)
        store.opened(Made.agentId(1), at: now)
        XCTAssertFalse(store.rows[0].unread)
    }

    // MARK: - The two kinds of machine a home may offer an account for

    private func home(_ hosts: [HostState], cloud: CloudState) -> FleetStore {
        let store = FleetStore(now: now)
        store.apply(.cloudState(cloud))
        store.apply(Made.fleet(
            [Made.card(1, name: "alpha", attention: .idle, minutesAgo: 2, now: now)],
            hosts: hosts, reconciled: true))
        return store
    }

    private func machine(
        _ slug: String, via: HostVia, online: Bool = true, signedIn: Bool? = true
    ) -> HostState {
        HostState(
            entry: HostEntry(
                id: HostId(UUID()), name: slug, online: online, via: via, signedIn: signedIn),
            epoch: 1)
    }

    /// The ordinary morning. Every machine can be used, so nothing on the home
    /// asks for an account: amux is free on the network this phone is on, and
    /// a screen that sold something while everything worked would be selling
    /// nothing.
    func testNothingIsNamedWhenEveryMachineIsReachable() {
        let signedOut = home([machine("studio", via: .direct)], cloud: .signedOut)
        XCTAssertNil(signedOut.awayHost)
        XCTAssertNil(signedOut.unreachableHost)

        let paid = home(
            [machine("studio", via: .direct), machine("mini", via: .relay)],
            cloud: .connected(tier: .pro, carrier: .quic))
        XCTAssertNil(paid.awayHost)
        XCTAssertNil(paid.unreachableHost)
    }

    /// A machine the relay can see and this account may not tunnel to. What is
    /// missing is the subscription, and the line names the machine.
    func testAMachineTheRelayCanSeeIsNamedAsAway() {
        let store = home(
            [machine("studio", via: .direct), machine("mini", via: .relay)],
            cloud: .connected(tier: .free, carrier: .quic))

        XCTAssertEqual(store.awayHost, "mini")
        XCTAssertNil(store.unreachableHost)
    }

    /// A machine nothing reaches is named apart from one the relay can see.
    /// The home offers an account for the first only where nobody is signed
    /// in, which is the registry's question rather than the fleet's.
    func testAMachineNothingReachesIsNamedApart() {
        let store = home(
            [machine("studio", via: .direct), machine("air", via: .offline, online: false)],
            cloud: .signedOut)

        XCTAssertNil(store.awayHost)
        XCTAssertEqual(store.unreachableHost, "air")
    }

    /// Both at once, each under its own name, so the screen can put the one
    /// that is a question of money first.
    func testBothKindsOfUnreachableMachineAreNamedSeparately() {
        let store = home(
            [machine("mini", via: .relay), machine("air", via: .offline, online: false)],
            cloud: .connected(tier: .free, carrier: .quic))

        XCTAssertEqual(store.awayHost, "mini")
        XCTAssertEqual(store.unreachableHost, "air")
    }

    /// Nothing has said what the relay will carry, so nothing claims a machine
    /// is away. A fixture, a launch before the link has answered and a phone
    /// with no relay at all are the same here: silence is not a free tier.
    func testAMachineIsNeverCalledAwayBeforeTheLinkHasSaidAnything() {
        let store = home([machine("mini", via: .relay)], cloud: .signedOut)

        XCTAssertNil(store.awayHost)
        XCTAssertEqual(store.hosts.values.map { store.reach(of: $0) }, [.throughTheRelay])
    }

    /// A machine that says it has no account is never "away". The relay is not
    /// seeing it, so nothing about it is a question of money, and grouping it
    /// with the machines a subscription would reach would sell a fix that is
    /// not one.
    func testAMachineThatNeverSignedInIsNotSoldASubscription() {
        let store = home(
            [machine("homelab", via: .offline, online: false, signedIn: false)],
            cloud: .connected(tier: .free, carrier: .quic))

        XCTAssertNil(store.awayHost)
        XCTAssertEqual(store.unreachableHost, "homelab")
        XCTAssertEqual(store.hosts.values.map { store.reach(of: $0) }, [.offline])
    }
}

private final class ObservationFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var changed = false

    var value: Bool { lock.withLock { changed } }

    func set() {
        lock.withLock { changed = true }
    }
}
