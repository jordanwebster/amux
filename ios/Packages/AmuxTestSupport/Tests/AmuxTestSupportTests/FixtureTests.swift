import AmuxCore
import AmuxFeatures
import XCTest
@testable import AmuxTestSupport

@MainActor
final class FixtureTests: XCTestCase {
    func testEveryFixtureIsReachableByName() {
        XCTAssertFalse(Fixtures.all.isEmpty)
        for fixture in Fixtures.all {
            XCTAssertEqual(Fixtures.named(fixture.id)?.id, fixture.id)
        }
        XCTAssertNil(Fixtures.named("no-such-fixture"))
    }

    func testFixtureIdentifiersAreUnique() {
        let identifiers = Fixtures.all.map(\.id)
        XCTAssertEqual(Set(identifiers).count, identifiers.count)
    }

    func testEveryScreenTheDesignDescribesHasAFixture() {
        let covered = Set(Fixtures.all.map(\.screen))
        let missing = Screen.allCases.filter { !covered.contains($0) }
        XCTAssertTrue(missing.isEmpty, "screens with no named state: \(missing.map(\.rawValue))")
        XCTAssertGreaterThanOrEqual(Fixtures.all.count, 40)
    }

    /// The point of a fixture is that it can be loaded without a network, a
    /// host or a screen. If one cannot fill fresh stores, nothing that depends
    /// on it — a golden, a journey, the door — can be trusted either.
    func testEveryFixtureLoadsIntoFreshStores() {
        for fixture in Fixtures.all {
            let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
            fixture.apply(bundle)
            // The catalogue, as loading it actually goes: what each named
            // state puts on screen, printed so a reader can see the whole set
            // rather than take the count on trust.
            print("fixture \(fixture.id) → screen \(fixture.screen.rawValue), "
                + "\(bundle.fleet.rows.count) agents, \(bundle.hosts.hosts.count) hosts, "
                + "\(bundle.conversations.values.reduce(0) { $0 + $1.entries.count }) rows"
                + (fixture.typeSize.map { ", type \($0)" } ?? ""))
            XCTAssertTrue(bundle.fleet.rows.allSatisfy { bundle.fleet.host($0.hostId) != nil }
                || bundle.fleet.rows.isEmpty,
                "\(fixture.id) shows an agent on a host it does not list")
            for conversation in bundle.conversations.values {
                XCTAssertTrue(conversation.invariants.isEmpty,
                              "\(fixture.id): \(conversation.invariants)")
            }
        }
    }

    func testTheStatesAScreenshotOfAGoodMorningNeverShows() {
        let names = Set(Fixtures.states.map(\.id))
        for expected in ["ask-permission-codex", "pair-confirm", "host-lost", "unreadable",
                         "send-refused", "upload-failed", "home-accessibility"] {
            XCTAssertTrue(names.contains(expected), "no fixture named \(expected)")
        }
        XCTAssertEqual(Fixtures.named("home-accessibility")?.typeSize, "accessibility3")
    }

    func testTheHomeFixtureShowsTheMorningTheDesignDescribes() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("home")!.apply(bundle)

        XCTAssertEqual(bundle.fleet.rows.count, 10)
        XCTAssertEqual(bundle.fleet.subtitle, "6 need you · 10 agents")
        // Two agents are genuinely blocked; the rest of the waiting are turns
        // that ended and nobody has read. Longest-waiting leads, so the
        // two-day-old unread turn is at the top rather than buried.
        let waiting = bundle.fleet.sections.first { $0.kind == .needsYou }
        XCTAssertEqual(waiting?.rows.map(\.name),
                       ["flake-hunt", "changelog", "pairing-copy", "relay-cleanup", "docs-pass",
                        "refactor-auth"])
        XCTAssertEqual(bundle.fleet.exceptions, "air offline")
    }

    func testTheQuietHomeSaysNothingNeedsYouAndNamesTheMissingMachine() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("home-quiet")!.apply(bundle)
        XCTAssertTrue(bundle.fleet.subtitle.hasPrefix("Nothing needs you · "))
        XCTAssertEqual(bundle.fleet.exceptions, "air offline")
        XCTAssertTrue(bundle.fleet.rows.allSatisfy { !$0.unread })
    }

    func testTheCachedHomeIsShownBeforeItIsConfirmed() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("home-cached")!.apply(bundle)
        XCTAssertFalse(bundle.fleet.reconciled)
        XCTAssertTrue(bundle.fleet.rows.allSatisfy { !$0.confirmed })
    }

    func testTheEmptyHomeIsEmptyRatherThanLoading() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("home-empty")!.apply(bundle)
        XCTAssertTrue(bundle.fleet.rows.isEmpty)
        XCTAssertTrue(bundle.fleet.reconciled)
    }

    /// The plain conversation carries its rows and nothing is waiting on
    /// anybody. An ask replaces the composer wherever there is one, so a
    /// fixture that carried one would be a picture of the ask panel rather
    /// than of the chrome and the feed this state is about.
    func testTheConversationFixturesCarryTheirRows() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("run")!.apply(bundle)
        let conversation = bundle.conversation(Scenario.focus)
        XCTAssertEqual(conversation.entries.count, Transcript.pairingCopy.count)
        XCTAssertEqual(conversation.entries.first?.entryKind, "prompt")
        XCTAssertTrue(conversation.asks.isEmpty)
        XCTAssertEqual(conversation.gate, .claudePty(.ready))
    }

    /// Each ask state is the same conversation with a different thing waiting
    /// on it, and each one draws its own panel.
    func testEachAskStateCarriesTheAskItIsAbout() {
        let expected: [(String, AgentId, String)] = [
            ("ask-permission", Scenario.focus, "permission"),
            ("ask-question", Scenario.focus, "question"),
            ("plan", Scenario.focus, "permission"),
        ]
        for (name, agent, kind) in expected {
            let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
            Fixtures.named(name)!.apply(bundle)
            let conversation = bundle.conversation(agent)
            XCTAssertEqual(conversation.gate, .claudePty(.needsYou), name)
            XCTAssertEqual(conversation.asks.first?.layer, .claudePty, name)
            XCTAssertEqual(
                conversation.asks.first?.body["kind"]?["ask"]?.stringValue, kind, name)
            XCTAssertNotNil(conversation.asks.panel, name)
        }
    }

    /// The finished turn's panel is offered off the fleet's own vocabulary
    /// rather than off the gate: an agent that ended a turn nobody has read is
    /// one of the things that need you.
    func testTheFinishedTurnIsOfferedForReview() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("finished")!.apply(bundle)
        XCTAssertEqual(
            bundle.fleet.rows.first(where: { $0.id == Scenario.focus })?.attention,
            .needsYou(why: .finished))
        XCTAssertNotNil(bundle.conversation(Scenario.focus).changes)
    }

    func testBothPermissionVocabulariesStayApart() {
        let claudeBundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("ask-permission")!.apply(claudeBundle)
        let claudeAsk = claudeBundle.conversation(Scenario.focus).asks.first
        XCTAssertEqual(claudeAsk?.layer, .claudePty)
        XCTAssertEqual(claudeAsk?.body["kind"]?["ask"]?.stringValue, "permission")

        let codexBundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("ask-permission-codex")!.apply(codexBundle)
        let codexAsk = codexBundle.conversation(Scenario.agentId("spec-suite")).asks.first
        XCTAssertEqual(codexAsk?.layer, .codex)
        // Codex offers its own choices, including one Claude does not have.
        // These are the four V1 decisions the frozen backend accepts, spelled
        // as the backend spells them; the words this used to assert were not
        // any layer's, so a panel built on them would have offered decisions
        // no host would take.
        XCTAssertEqual(
            codexAsk?.body["actions"]?.arrayValue?.compactMap { $0["wire"]?.stringValue },
            ["accept", "acceptForSession", "decline"])
        // The object-valued choice in the middle is real and is not a scalar,
        // which is why it has no wire word here and cannot be pressed.
        XCTAssertEqual(codexAsk?.body["actions"]?.arrayValue?.count, 4)
    }

    func testTheUnreadableAgentSaysSoRatherThanLookingIdle() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("unreadable")!.apply(bundle)
        let conversation = bundle.conversation(Scenario.agentId("legacy-port"))
        XCTAssertEqual(conversation.facts, .claudeSdk(supported: false))
        XCTAssertEqual(conversation.gate, .unavailable)
        XCTAssertFalse(conversation.gate.accepts)
    }

    func testTheRefusedSendCarriesTheReasonTheCoreGave() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("send-refused")!.apply(bundle)
        let conversation = bundle.conversation(Scenario.focus)
        XCTAssertFalse(conversation.gate.accepts)
        guard case .failed(let failure) = conversation.results.first?.outcome else {
            return XCTFail("expected a refusal, got \(String(describing: conversation.results.first))")
        }
        XCTAssertEqual(failure.message, "the session is replaying history")
    }

    func testTheLostHostLeavesTheFeedReadable() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("host-lost")!.apply(bundle)
        XCTAssertFalse(bundle.conversation(Scenario.focus).entries.isEmpty)
        XCTAssertEqual(bundle.hosts.offline.map(\.name), ["air", "Studio"])
        XCTAssertEqual(bundle.fleet.connection.state, .disconnected)
    }

    func testTheExitedAgentStatesItsCode() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("exited")!.apply(bundle)
        XCTAssertEqual(bundle.fleet.rows.first { $0.name == "refactor-auth" }?.phase,
                       .exited(exitCode: 1))
        XCTAssertEqual(bundle.conversation(Scenario.focus).gate, .claudePty(.exited))
    }

    func testTheWorkingFixtureCarriesTheProvidersOwnTaskList() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("working")!.apply(bundle)
        let todos = bundle.conversation(Scenario.focus).provider.todos
        XCTAssertEqual(todos?.done, 3)
        XCTAssertEqual(todos?.total, 7)
        XCTAssertEqual(todos?.items.count, 7)
    }

    func testTheQueuedFixtureHoldsOneMessage() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("queued")!.apply(bundle)
        XCTAssertNotNil(bundle.conversation(Scenario.focus).queued)
    }

    func testTheReviewFixtureCarriesTheChanges() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("review-cta")!.apply(bundle)
        XCTAssertEqual(bundle.conversation(Scenario.focus).changes?.hunks.count, 2)
    }

    func testTheUnpairedMachineIsAnOfferRatherThanAHost() {
        let bundle = StoreBundle(account: AccountId("ada"), now: Scenario.now)
        Fixtures.named("pair-confirm")!.apply(bundle)
        let homelab = bundle.hosts.host(Scenario.homelab)
        XCTAssertEqual(homelab?.trustStatus, .untrustedButOnline)
        XCTAssertTrue(bundle.fleet.rows.allSatisfy { $0.hostId != Scenario.homelab })
    }

    func testScenarioIdentifiersAreStable() {
        // A capture taken on another machine has to name the same agent.
        XCTAssertEqual(Scenario.agentId("refactor-auth"), Scenario.agentId("refactor-auth"))
        XCTAssertNotEqual(Scenario.agentId("refactor-auth"), Scenario.agentId("docs-pass"))
        XCTAssertEqual(Scenario.focus, Scenario.agents[0].id)
    }
}
