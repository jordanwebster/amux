import Foundation
import XCTest

/// A phone signed in to an account that has not paid for the relay, and the
/// three machines the runner is really running around it.
///
/// Nothing here is broken. The account works, the phone is signed in, the
/// machines are up: what has not been bought is the tunnel, and the only thing
/// that changes is which machines this phone can reach. So the story is a
/// person leaving the room their machine is in. At home it is on this network
/// and everything runs; away from it the relay can still see it and will not
/// carry anything to it, and what the phone knows about it is the last thing
/// it was told — said as exactly that, never as an agent that is working.
///
/// The account service is scripted and the App Store is never reached, for the
/// reason every journey about money has: the store's sheet belongs to another
/// process and nobody outside it can press it. What a purchase leaves behind
/// is what this drives — amux.sh starts saying the account is subscribed, and
/// the phone asks its own link to read that again — because that, and not the
/// receipt, is what puts a machine back in reach.
final class FreeTierTests: JourneyCase {
    /// The three machines, as the journey named them.
    private struct Cast {
        /// Signed in to this account, on this network at first, and the one
        /// running an agent.
        let workstation: String
        /// Signed in to the same account and only ever seen through the relay:
        /// the machine a code cannot reach on a free link.
        let studio: String
        /// On this network and signed in to nothing at all.
        let spare: String
        /// The agent workstation is running.
        let agent: String

        init(_ environment: [String: String]) throws {
            func required(_ name: String) throws -> String {
                try XCTUnwrap(environment[name], "the journey did not pass \(name)")
            }
            workstation = try required("AMUX_WORKSTATION")
            studio = try required("AMUX_STUDIO")
            spare = try required("AMUX_SPARE")
            agent = try required("AMUX_AGENT")
        }
    }

    private var runner: Runner!
    private var cast: Cast!
    private var control: Lines!
    private var app: XCUIApplication!

    func testAnAccountThatHasNotPaidReachesThisNetworkAndIsOfferedTheRest() throws {
        runner = try Runner()
        cast = try Cast(ProcessInfo.processInfo.environment)
        control = try Lines(address: runner.control)
        defer { try? write("free-tier.json") }

        try aPhoneSignedInOnAnAccountWithNothingBought()
        try theMachinesOnThisNetwork()
        try awayFromThem()
        try theAgentOnAMachineTheRelayCanSee()
        try aCodeForAMachineOnlyTheRelayHasSeen()
        try andThenItWasPaidFor()
    }

    // MARK: - Signed in, nothing bought

    /// The app opened and signed in through its own sign-in, against an
    /// account service that says this account has bought nothing.
    ///
    /// Signed in rather than handed a credential, because what this journey
    /// turns on is the account service's answer: the credential it mints
    /// carries what the account may do, and the relay admits the link on it.
    private func aPhoneSignedInOnAnAccountWithNothingBought() throws {
        app = launch(cloud: script(entitlement: "none"))
        waitFor(app, "home.empty.signIn", "a phone with no account did not offer to sign in")
        press(app, "home.empty.signIn")
        waitFor(app, "sign-in", "sign-in did not open")
        press(app, "sign-in.continue")
        waitFor(app, "sign-in.signed-in", "the scripted account service did not sign in")
        press(app, "sign-in.continue")
        XCTAssertTrue(waitUntil { (try? self.started()) == true },
                      "signing in started no runtime of its own")
    }

    /// The launch, with the account service and the App Store the app itself
    /// holds replaced by the scripted ones, and the scripted service pointed
    /// at the relay the runner is running.
    private func launch(cloud: [String: Any]) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-amux-door-port", runner.doorPort, "-amux-scripted-cloud",
                               "-amux-cloud-script", json(cloud)]
        app.launch()
        return app
    }

    /// What the account service answers, with this account's real relay
    /// credential in it. Only what the subscription is changes between the
    /// two times this journey says it.
    private func script(entitlement: String) -> [String: Any] {
        let relay = URLComponents(string: runner.relay)
        return [
            "account": runner.user, "email": "\(runner.user)@example.com",
            "displayName": runner.user, "entitlement": entitlement, "source": "appStore",
            "token": runner.token,
            "relayHost": relay?.host ?? "127.0.0.1", "relayPort": relay?.port ?? 0,
        ]
    }

    private func json(_ script: [String: Any]) -> String {
        let json = try? JSONSerialization.data(withJSONObject: script, options: [.sortedKeys])
        return String(decoding: json ?? Data(), as: UTF8.self)
    }

    // MARK: - At home

    /// Both machines on this network, paired with on it, and what one of them
    /// is running arriving over a link this phone opened itself.
    ///
    /// Pairing happens here and not over the relay because on this account it
    /// could not happen over the relay — which is the next act. Nothing about
    /// it needed an account: a machine on your own network is reached by
    /// finding it and typing what it printed.
    private func theMachinesOnThisNetwork() throws {
        try handOverWhatIsOnTheNetwork()
        pressTab(app, "Hosts")
        // Waited for before a code is typed, because what a browser resolved
        // reaches the connection through the runtime rather than with the
        // hand-over: pairing before it lands has no address on this network to
        // dial, falls back to the relay, and leaves a machine in the same room
        // reading as one the relay can merely see.
        for machine in ["workstation", "spare"] {
            waitFor(app, "hosts.offer.\(identity(of: machine))",
                    "\(machine) is on this network and was never offered")
        }
        for machine in ["workstation", "spare"] {
            try door(runner, .init(kind: "pairByCode", host: identity(of: machine),
                                   pin: try code(from: machine)))
        }
        // A browser keeps resolving; this one is told what it sees, once per
        // telling. A machine is dialled on the network when it is seen there
        // and already trusted, and neither was true of these until now — the
        // first sighting was of a stranger and the pairing that followed
        // reached one of them over the relay. So the same sighting is repeated
        // here, which is what a real browser does every few seconds anyway.
        try handOverWhatIsOnTheNetwork()
        let athome = waitUntil { (try? self.reach(of: self.cast.workstation)) == "on-this-network" }
        record["atHome"] = try reaches()
        let carriers = ((try? bridge())?["reach"] as? [String]) ?? []
        record["carriersAtHome"] = carriers
        // What the runtime itself did about these two machines. The screen's
        // word for a machine is the end of a chain of decisions no screen
        // shows, and when it is the wrong word this is the only account of
        // which address was dialled and what came of it.
        record["runtimeAtHome"] = try runtimeLog(runner).split(separator: "\n")
            .filter { $0.contains("Link") || $0.contains("route") || $0.contains("direct") }
            .suffix(80).joined(separator: "\n")
        XCTAssertTrue(athome,
                      "workstation is on this network and the phone reads it as "
                      + "\((try? reach(of: cast.workstation)) ?? "nothing"), over \(carriers)")
        pressTab(app, "Agents")
        waitFor(app, "home.row.\(cast.agent)", "what workstation is running never reached the phone")
        record["agentAtHome"] = said(try declared(runner), "home.row.\(cast.agent)")?.value ?? ""
        photograph(app, "at-home")
    }

    /// Puts the two machines that belong on this network on it, and hands over
    /// what a browser would have resolved.
    ///
    /// The browsing is said rather than done for the reason it is in every
    /// journey about this network: only the system may browse, and a
    /// simulator's browser looks at this Mac's network instead of at the one
    /// the runner is running.
    private func handOverWhatIsOnTheNetwork() throws {
        var advertised: [[String: Any]] = []
        for machine in ["workstation", "spare"] {
            let answer = try control.ask(["Announce": ["daemon": machine]])
            advertised.append(try XCTUnwrap(
                (answer["Ack"] as? [String: Any])?["found"] as? [String: Any],
                "the runner announced nothing for \(machine)"))
        }
        try door(runner, .init(kind: "found", hosts: advertised))
    }

    // MARK: - Away from them

    /// The machines leave this network, and the phone is left with what the
    /// relay will do for an account that has bought nothing.
    ///
    /// Leaving is both halves of leaving: the goodbye the advertisement sends
    /// and the machine no longer answering at the address it advertised. What
    /// is left of workstation is the relay's view of it, which this account
    /// may not tunnel through; what is left of spare is nothing at all,
    /// because a machine signed in to no account has no relay to be seen on.
    private func awayFromThem() throws {
        for machine in ["workstation", "spare"] {
            try control.ask(["Withdraw": ["daemon": machine]])
            try control.ask(["UdpBlocked": ["daemon": machine, "blocked": true]])
        }
        try door(runner, .init(kind: "found", hosts: []))
        pressTab(app, "Hosts")
        XCTAssertTrue(waitUntil(within: 120) {
            (try? self.reach(of: self.cast.workstation)) == "away"
                && (try? self.reach(of: self.cast.spare)) == "offline"
        }, "off this network the phone reads workstation as "
            + "\((try? reach(of: cast.workstation)) ?? "nothing") and spare as "
            + "\((try? reach(of: cast.spare)) ?? "nothing")")
        let machines = try declared(runner)
        record["away"] = try reaches()
        record["awayCaption"] = said(machines, "hosts.caption.away")?.value ?? ""
        // The machine nobody has signed in on says that, rather than being
        // offered a subscription: what is in the way is on the machine, and no
        // money moves it.
        record["neverSignedIn"] = said(machines, "hosts.row.\(cast.spare)")?.label ?? ""
        XCTAssertTrue((said(machines, "hosts.row.\(cast.spare)")?.label ?? "")
            .contains("offline and never signed in"),
                      "the machine signed in to nothing reads "
                      + "\(said(machines, "hosts.row.\(cast.spare)")?.label ?? "nothing")")
        photograph(app, "away")

        pressTab(app, "Agents")
        let home = try declared(runner)
        record["agentWhileAway"] = said(home, "home.row.\(cast.agent)")?.value ?? ""
        record["agentSaysWhileAway"] = said(home, "home.row.\(cast.agent)")?.label ?? ""
        record["homeLineWhileAway"] = said(home, "home.exceptions")?.value ?? ""
        XCTAssertTrue((said(home, "home.row.\(cast.agent)")?.value ?? "").hasPrefix("host-away"),
                      "the agent on a machine the relay can see reads "
                      + "\(said(home, "home.row.\(cast.agent)")?.value ?? "nothing")")
        XCTAssertEqual(said(home, "home.exceptions")?.value,
                       "workstation is away · subscribe to reach your agents from anywhere",
                       "the one line above the list reads "
                       + "\(said(home, "home.exceptions")?.value ?? "nothing")")
    }

    // MARK: - Opening one of its agents

    /// The conversation of an agent on a machine the relay can see: what it
    /// last said, and the offer where the composer would be.
    private func theAgentOnAMachineTheRelayCanSee() throws {
        press(app, "home.row.\(cast.agent)")
        waitFor(app, "conversation", "the remembered agent did not open")
        // The offer's own button rather than the block around it: the block
        // is a container that only holds its children together, and a
        // container is not something a finger or an accessibility client can
        // address on its own.
        waitFor(app, "conversation.subscribe.buy",
                "an agent on a machine the relay can see offered nothing about reaching it")
        let open = try declared(runner)
        record["chatOffer"] = said(open, "conversation.subscribe")?.label ?? ""
        record["chatOfferDetail"] = said(open, "conversation.subscribe")?.value ?? ""
        record["chatOfferAction"] = said(open, "conversation.subscribe.buy")?.label ?? ""
        XCTAssertTrue((said(open, "conversation.subscribe")?.value ?? "").contains("workstation"),
                      "the offer in the conversation does not name the machine: "
                      + "\(said(open, "conversation.subscribe")?.value ?? "nothing")")
        XCTAssertEqual(said(open, "conversation.subscribe.buy")?.label, "Subscribe",
                       "the offer carries nothing to press")
        photograph(app, "chat-away")
        leaveTheConversation()
    }

    /// Back out of a conversation to the list it was opened from, which is the
    /// only way back to the tabs: a conversation covers the tab bar.
    private func leaveTheConversation() {
        let edge = app.coordinate(withNormalizedOffset: CGVector(dx: 0.01, dy: 0.5))
        let inside = app.coordinate(withNormalizedOffset: CGVector(dx: 0.85, dy: 0.5))
        edge.press(forDuration: 0.05, thenDragTo: inside)
        waitFor(app, "home", "leaving the conversation did not return to the list")
    }

    // MARK: - A code for a machine only the relay has seen

    /// The code studio printed, typed on the keypad by somebody who has bought
    /// nothing.
    ///
    /// The code is right and pairing still does not happen: what is missing is
    /// the tunnel, which no number typed here can produce. So the screen says
    /// the one thing that would, rather than refusing digits that were
    /// correct.
    private func aCodeForAMachineOnlyTheRelayHasSeen() throws {
        pressTab(app, "Hosts")
        waitFor(app, "hosts.pair.\(cast.studio)", "studio was never offered to pair with")
        press(app, "hosts.pair.\(cast.studio)")
        waitFor(app, "pin", "the offer to pair did not lead to the keypad")
        for digit in try code(from: "studio") { press(app, "pin.key.\(digit)") }
        waitFor(app, "pin.subscribe.buy",
                "a code that authenticated against a machine only the relay can see was answered "
                + "with something else")
        let keypad = try declared(runner)
        record["keypadOffer"] = said(keypad, "pin.subscribe")?.label ?? ""
        record["keypadOfferDetail"] = said(keypad, "pin.subscribe")?.value ?? ""
        XCTAssertFalse(element(app, "pin.refused").exists,
                       "a code the machine really printed was called a bad code")
        photograph(app, "code-needs-subscription")
        press(app, "pin.back")
        waitFor(app, "hosts", "the keypad did not lead back to the machines")
    }

    // MARK: - Paid for

    /// The subscription bought, and the phone live again at once.
    ///
    /// Two things happen and both are somebody else's: the account now buys
    /// the relay, and amux.sh says so. What the phone does about it is one
    /// thing — ask its own link to read what this account may do again — and
    /// that is what this drives, because the receipt itself is the App Store's
    /// and no test can press that sheet.
    private func andThenItWasPaidFor() throws {
        try control.ask(["Tier": ["user": runner.user, "tier": "pro"]])
        try door(runner, .init(kind: "cloud", cloud: script(entitlement: "active")))
        try door(runner, .init(kind: "refreshEntitlement"))
        // Longer than the ordinary wait: the relay admitted this phone's link
        // on the old tier, so reaching the machine again means a fresh link on
        // a fresh credential and the retry that opens it.
        XCTAssertTrue(waitUntil(within: 180) {
            (try? self.reach(of: self.cast.workstation)) == "through-the-relay"
        },
                      "after the subscription the phone still reads workstation as "
                      + "\((try? reach(of: cast.workstation)) ?? "nothing")")
        record["afterSubscribing"] = try reaches()
        photograph(app, "subscribed")

        pressTab(app, "Agents")
        XCTAssertTrue(waitUntil(within: 180) {
            (try? self.said(self.declared(self.runner, settling: false),
                            "home.row.\(self.cast.agent)")?.value)?
                .hasPrefix("host-away") == false
        }, "the agent still reads as not live after the subscription")
        let home = try declared(runner)
        record["agentAfterSubscribing"] = said(home, "home.row.\(cast.agent)")?.value ?? ""
        record["homeLineAfterSubscribing"] = said(home, "home.exceptions")?.value ?? ""
        // The subscription is gone from what the home says. What is left is
        // spare, which is genuinely off and has no account to be reached on,
        // so a home that said nothing at all here would be hiding a machine
        // that is really unreachable — the line is right to name it and wrong
        // to go on selling a tunnel that is already bought.
        let line = said(home, "home.exceptions")?.value ?? ""
        XCTAssertFalse(line.contains("subscribe") || line.contains("workstation"),
                       "the home still offers a subscription after one was bought: \(line)")
        record["newAgentAfterSubscribing"] = element(app, "home.newAgent").exists
    }

    // MARK: - What each side says

    /// A code that machine has just printed.
    private func code(from machine: String) throws -> String {
        let answer = try control.ask(
            ["StartPinPairing": ["daemon": machine, "ttl_secs": 600]])
        return try XCTUnwrap((answer["Ack"] as? [String: Any])?["pin"] as? String,
                             "\(machine) printed no code")
    }

    private func identity(of machine: String) -> String {
        switch machine {
        case "workstation": cast.workstation
        case "studio": cast.studio
        default: cast.spare
        }
    }

    /// Where the phone says it can reach each machine from, by the word the
    /// screen and this journey agree on.
    private func reaches() throws -> [String: String] {
        let machines = try declared(runner)
        return ["workstation": said(machines, "hosts.row.\(cast.workstation)")?.value ?? "",
                "studio": said(machines, "hosts.row.\(cast.studio)")?.value ?? "",
                "spare": said(machines, "hosts.row.\(cast.spare)")?.value ?? ""]
    }

    private func reach(of machine: String) throws -> String {
        said(try declared(runner, settling: false), "hosts.row.\(machine)")?.value ?? ""
    }

    private func bridge() throws -> [String: Any] {
        try XCTUnwrap(try door(runner, .init(kind: "bridge"))["bridge"] as? [String: Any],
                      "the door said nothing about the runtime")
    }

    private func started() throws -> Bool { try bridge()["started"] as? Bool ?? false }
}
