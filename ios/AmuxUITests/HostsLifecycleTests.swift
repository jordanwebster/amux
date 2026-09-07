import Foundation
import XCTest

/// What this phone's connection does over time, against machines the runner is
/// really running.
///
/// Everything here is about the link rather than about a screen: that a relay
/// taken away and put back recovers with nobody pressing anything, that a
/// phone holds one connection per machine and asks for nothing while it is
/// idle, that being put away releases the link and coming back restores it,
/// and that a conversation somebody closed is not silently reopened by any of
/// it. It is a UI test because two of those are things only a finger can do —
/// pressing Retry Now, and putting the app away.
final class HostsLifecycleTests: JourneyCase {
    /// How long the phone is left alone to prove it asks for nothing.
    private let idle: TimeInterval = 6

    func testTheConnectionRecoversAloneIsOnePerMachineAndSurvivesBeingPutAway() throws {
        let runner = try Runner()
        let environment = ProcessInfo.processInfo.environment
        let account = try XCTUnwrap(environment["AMUX_ACCOUNT"])
        let hosts = try XCTUnwrap(environment["AMUX_HOST_IDS"]).split(separator: ",")
            .map(String.init)
        let second = try XCTUnwrap(environment["AMUX_SECOND_AGENT"])
        let machine = try XCTUnwrap(environment["AMUX_HOST_ID"])
        let code = try XCTUnwrap(environment["AMUX_PIN"])
        let control = try Lines(address: runner.control)
        let app = launch(runner)

        // Trusting the machine by the code it printed, through the same two
        // steps and the same store the pairing screen drives. The launch
        // itself trusts nobody: what this test is about begins once there is
        // a link to a machine that has admitted this phone.
        try door(runner, .init(kind: "pairByCode", host: machine, pin: code))

        /// What the relay is holding for this account: one entry per host
        /// connected to it, with how many links that host holds. A phone that
        /// opened a connection per conversation, or that reconnected on a
        /// timer, shows up here; a phone multiplexing one link does not, and a
        /// phone that has gone away does not appear at all.
        func inventory() throws -> [String] {
            let answer = try control.ask(["Connections": ["user": account]])
            let ack = answer["Ack"] as? [String: Any]
            return (ack?["links"] as? [String] ?? []).sorted()
        }
        /// What the runtime under the app has arrived at.
        func bridge() throws -> [String: Any] {
            let answer = try door(runner, .init(kind: "bridge"))
            return answer["bridge"] as? [String: Any] ?? [:]
        }
        func reconciled() throws -> Bool { try bridge()["reconciled"] as? Bool ?? false }
        func watching() throws -> [String] { try bridge()["watching"] as? [String] ?? [] }
        func dials() throws -> Int { try bridge()["relayAttempts"] as? Int ?? -1 }
        func asked() throws -> Int { try bridge()["relayRetries"] as? Int ?? -1 }

        // MARK: The fleet both machines answer for.
        waitFor(app, "home.row.\(runner.agent)",
                "the machines never answered for the agents the runner is running")
        XCTAssertTrue(waitUntil { (try? reconciled()) == true },
                      "the fleet was never confirmed by a machine")
        let settled = try inventory()
        record["connectionsWhenReached"] = settled
        // One link each, and one more than there are machines: the extra one
        // is this phone.
        XCTAssertEqual(settled.count, hosts.count + 1,
                       "the relay holds \(settled) for an account of \(hosts.count) machines "
                       + "and one phone")
        XCTAssertTrue(settled.allSatisfy { $0.hasSuffix(": 1") },
                      "something is holding more than one connection to the relay: \(settled)")

        // MARK: One connection per machine, however many conversations are open.
        //
        // Opening a conversation asks its machine for a stream. If each stream
        // were its own connection the machine would say so here.
        press(app, "home.row.\(runner.agent)")
        waitFor(app, "conversation", "pressing a row did not open its conversation")
        // A second conversation on the same machine, opened without going to
        // it: only one conversation can be on screen, and what is being
        // claimed here is about how many connections two streams take.
        try door(runner, .init(kind: "watch", agent: second))
        let bothOpen = waitUntil { ((try? watching())?.count ?? 0) >= 2 }
        let heldStreams = (try? watching()) ?? []
        XCTAssertTrue(bothOpen, "two conversations open at once hold \(heldStreams) streams")
        let withTwoOpen = try inventory()
        record["connectionsWithTwoConversationsOpen"] = withTwoOpen
        XCTAssertEqual(withTwoOpen, settled,
                       "opening two conversations on the same machines changed what those "
                       + "machines are holding")

        // MARK: And nothing periodic while nobody is doing anything.
        let dialsBeforeIdle = try dials()
        Thread.sleep(forTimeInterval: idle)
        let afterIdle = try inventory()
        record["connectionsAfterIdle"] = afterIdle
        record["dialsBeforeIdle"] = dialsBeforeIdle
        let dialsAfterIdle = try dials()
        record["dialsAfterIdle"] = dialsAfterIdle
        XCTAssertEqual(afterIdle, settled,
                       "sitting idle for \(idle)s changed what the machines are holding")
        XCTAssertEqual(dialsAfterIdle, dialsBeforeIdle,
                       "the phone dialled the relay again while it was sitting idle")

        // MARK: A conversation closed before anything is disturbed.
        //
        // Leaving it releases the stream it asked for, and nothing later —
        // no outage, no recovery — is allowed to ask for it again.
        pressTab(app, "Agents")
        // Reaching for the tab you are already on is the way out of a
        // conversation, so this is a person leaving one.
        waitFor(app, "home", "leaving the conversation did not lead back to the fleet")
        XCTAssertFalse(element(app, "conversation").exists,
                       "the conversation is still on screen after leaving it")
        let released = waitUntil { (try? watching())?.contains(runner.agent) == false }
        let afterClosing = try watching()
        record["watchingAfterClosing"] = afterClosing
        record["releasedAfterClosing"] = try bridge()["releasedStreams"] as? [String] ?? []
        XCTAssertTrue(released,
                      "leaving a conversation left its stream open: \(afterClosing)")
        XCTAssertTrue(afterClosing.contains(second),
                      "leaving one conversation closed another nobody left: \(afterClosing)")

        // MARK: The relay goes away and comes back, with nobody pressing anything.
        try control.ask("CloudOffline")
        try door(runner, .init(kind: "awaitOffline", seconds: 30))
        record["offline"] = try bridge()["connection"] as? String ?? "unread"
        let dialsWhileGone = try asked()
        try control.ask("CloudOnline")
        XCTAssertTrue(waitUntil { (try? reconciled()) == true },
                      "the relay came back and the phone never reconnected by itself")
        record["reconciledWithoutAnyoneAsking"] = true
        record["connectionsAfterRecovery"] = try inventory()
        XCTAssertEqual(try asked(), dialsWhileGone,
                       "the recovery that happened on its own was credited to a press")

        // MARK: A second outage, and the offer to try again reaches the link.
        //
        // What is counted is dials the connection made early because it was
        // asked, read before the relay is allowed back: a dial at a relay that
        // is not there arrives nowhere, and the connection dials on its own
        // schedule anyway, so an attempt alone cannot tell a press apart from
        // the backoff coming round.
        press(app, "home.row.\(second)")
        waitFor(app, "conversation", "the other agent's conversation did not open")
        try control.ask("CloudOffline")
        waitFor(app, "conversation.retry",
                "an unreachable machine offered no way to ask again")
        let before = try asked()
        press(app, "conversation.retry")
        let reached = waitUntil { ((try? asked()) ?? before) > before }
        record["askedBeforePress"] = before
        record["askedAfterPress"] = try asked()
        XCTAssertTrue(reached,
                      "Retry Now was pressed and the connection was never asked to dial")
        try control.ask("CloudOnline")
        XCTAssertTrue(waitUntil { (try? reconciled()) == true },
                      "the relay came back and the phone stayed offline")

        // MARK: Put away, and brought back.
        //
        // A phone in a pocket holds no connection: the machines it was
        // watching see it leave rather than holding a link nobody is reading.
        XCUIDevice.shared.press(.home)
        let away = waitUntil { ((try? inventory())?.count ?? settled.count) < settled.count }
        let whileAway = try inventory()
        record["connectionsWhilePutAway"] = whileAway
        XCTAssertTrue(away,
                      "the machines still held every link after the phone was put away: "
                      + "\(whileAway) against \(settled)")
        app.activate()
        waitFor(app, "conversation", "coming back did not return to the same conversation")
        XCTAssertTrue(waitUntil { (try? reconciled()) == true },
                      "coming back never confirmed the fleet again")
        record["connectionsAfterComingBack"] = try inventory()
        record["reconciledAfterComingBack"] = true

        // MARK: And the conversation closed at the start is still closed.
        let atTheEnd = try watching()
        record["watchingAtTheEnd"] = atTheEnd
        XCTAssertFalse(atTheEnd.contains(runner.agent),
                       "an outage and a suspension between them reopened a conversation "
                       + "nobody asked for: \(atTheEnd)")
        XCTAssertTrue(atTheEnd.contains(second),
                      "the conversation still open lost its stream: \(atTheEnd)")
        try write()
    }

    /// Everything this test read, left where the Mac can collect it.
    private func write() throws {
        let data = try JSONSerialization.data(
            withJSONObject: record, options: [.prettyPrinted, .sortedKeys])
        try data.write(to: Self.inContainer("hosts-lifecycle.json"))
    }
}
