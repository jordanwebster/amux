import Foundation
import XCTest

/// A phone taken out of its box, with nobody signed in and no relay anywhere,
/// against one machine the runner is really running on its own network.
///
/// Everything an account buys is absent here on purpose: there is no relay to
/// reach, no credential to reach it with and no subscription to pay for it. So
/// every machine this phone sees, it sees because its own browser found it,
/// and every byte it exchanges afterwards goes straight there. What the
/// machine then holds is read back from the machine, because a phone reporting
/// what it asked for would be quoting its own request.
///
/// Two things a person's phone does are said through the app's door rather
/// than done to it, because a simulator cannot do them: only the system may
/// browse, and a simulator's browser looks at the Mac's real network instead
/// of at the one the runner is running — so what a browser would have resolved
/// is handed over exactly as the app's own browser hands it over; and iOS asks
/// about the local network once, so a refusal cannot be provoked twice.
final class OnrampTests: JourneyCase {
    /// The one machine, as the journey named it.
    private struct Cast {
        let workstation: String
        let key: String
        /// An agent the topology is running on it, which is what the phone has
        /// to be able to see once it is paired.
        let agent: String

        init(_ environment: [String: String]) throws {
            func required(_ name: String) throws -> String {
                try XCTUnwrap(environment[name], "the journey did not pass \(name)")
            }
            workstation = try required("AMUX_WORKSTATION")
            key = try required("AMUX_WORKSTATION_FINGERPRINT")
            agent = try required("AMUX_AGENT_NAME")
        }
    }

    private var runner: Runner!
    private var cast: Cast!
    private var control: Lines!
    /// The launch every act works through. One launch for the whole journey:
    /// what this story is about is a phone being set up, and quitting between
    /// the steps would be somebody else's story.
    private var app: XCUIApplication!
    /// What the machine last put on this network, kept so the browsing this
    /// test stands in for can hand the same set over again.
    private var advertised: [String: Any] = [:]

    /// One act of this journey: what a person does in it, and the cheapest way
    /// to leave behind what doing it leaves behind.
    private struct Act {
        let name: String
        let perform: () throws -> Void
        let shortcut: () throws -> Void

        init(_ name: String, _ perform: @escaping () throws -> Void,
             shortcut: @escaping () throws -> Void = {}) {
            self.name = name
            self.perform = perform
            self.shortcut = shortcut
        }
    }

    private func script() -> [Act] {
        [
            Act("first-run", aPhoneNobodyHasSignedInOn),
            Act("found-host", theMachineOnThisNetwork, shortcut: { try self.handOverWhatIsOnTheNetwork() }),
            Act("code-entry", sixDigitsOffTheMachine, shortcut: { try self.trustThroughTheDoor() }),
            // Confirming is the second half of the same screen the digits are
            // typed on, so re-entering at it without having typed them is
            // re-entering at the pairing itself.
            Act("paired", theMachineAndWhatItIsRunning, shortcut: { try self.trustThroughTheDoor() }),
            Act("withdrawn-and-back", theMachineLeavesThisNetworkAndComesBack),
            Act("permission-refused", aNetworkThisPhoneIsNotAllowedToLookAt),
        ]
    }

    func testAPhoneWithNoAccountFindsPairsWithAndUsesAMachineOnItsOwnNetwork() throws {
        runner = try Runner(withoutAnAccount: true)
        cast = try Cast(ProcessInfo.processInfo.environment)
        control = try Lines(address: runner.control)
        defer { try? write("onramp.json") }

        let acts = script()
        let asked = (ProcessInfo.processInfo.environment["AMUX_ACTS"] ?? "")
            .split(separator: ",").map(String.init).filter { !$0.isEmpty }
        let unknown = asked.filter { name in !acts.contains { $0.name == name } }
        XCTAssertTrue(unknown.isEmpty,
                      "this journey has no act called \(unknown.joined(separator: ", ")); "
                      + "it has \(acts.map { $0.name })")
        guard unknown.isEmpty else { return }
        let last = asked.isEmpty
            ? acts.count - 1
            : (acts.lastIndex { asked.contains($0.name) } ?? acts.count - 1)
        var performed: [String] = []
        var shortcut: [String] = []
        record["actsPerformed"] = performed
        record["actsShortcut"] = shortcut
        var seconds: [String: Int] = [:]
        // Every act after the first works on a phone that is already running,
        // so a run re-entering partway through launches one before the
        // shortcuts write anything into it.
        if !asked.isEmpty && !asked.contains("first-run") { launchWithNobodySignedIn() }
        for act in acts[...last] {
            let began = Date()
            if asked.isEmpty || asked.contains(act.name) {
                try act.perform()
                performed.append(act.name)
            } else {
                try act.shortcut()
                shortcut.append(act.name)
            }
            seconds[act.name] = Int(Date().timeIntervalSince(began).rounded())
            record["secondsPerAct"] = seconds
            record["actsPerformed"] = performed
            record["actsShortcut"] = shortcut
        }
    }

    // MARK: - A phone nobody has signed in on

    /// The app as somebody opens it for the first time: no account, no relay,
    /// and nothing found yet.
    private func aPhoneNobodyHasSignedInOn() throws {
        launchWithNobodySignedIn()
        pressTab(app, "Hosts")
        waitFor(app, "hosts", "the machines never came up")
        record["accountsAtFirstRun"] = try accountsOnThisPhone()
        record["machinesAtFirstRun"] = try machinesOnScreen()
        record["offersAtFirstRun"] = offersOnScreen()
        XCTAssertEqual(try accountsOnThisPhone(), 0,
                       "a phone nobody has signed in on knows an account")
        XCTAssertEqual(try machinesOnScreen(), [], "a phone that has paired with nothing has hosts")
        waitFor(app, "hosts.empty", "the machines are empty and the screen does not say so")
        photograph(app, "first-run")
    }

    private func launchWithNobodySignedIn() {
        app = launch(runner, signedIn: false)
        let running = waitUntil { (try? self.started()) == true }
        // What the runtime said on its way down, where it did. A phone with no
        // account starts a runtime for reasons nothing on screen explains, so
        // a failure here is unreadable without it.
        record["runtimeFailure"] = (try? bridge())?["failure"] as? String ?? ""
        XCTAssertTrue(running,
                      "the phone never started a runtime of its own: "
                      + "\(record["runtimeFailure"] ?? "no reason given")")
    }

    // MARK: - The machine on this network

    /// A machine puts itself on this network, and the phone offers it.
    private func theMachineOnThisNetwork() throws {
        try handOverWhatIsOnTheNetwork()
        waitFor(app, "hosts.offer.\(cast.workstation)",
                "the machine on this network was never offered")
        let offered = try declared(runner)
        let offer = said(offered, "hosts.offer.\(cast.workstation)")
        record["foundName"] = offer?.label ?? ""
        record["foundState"] = offer?.value ?? ""
        record["pairAction"] = said(offered, "hosts.pair.\(cast.workstation)")?.label ?? ""
        XCTAssertTrue((offer?.label ?? "").contains("workstation"),
                      "the card on this network reads \(offer?.label ?? "nothing")")
        XCTAssertEqual(said(offered, "hosts.pair.\(cast.workstation)")?.label,
                       "Pair with workstation",
                       "the machine on this network is offered without a way to pair with it")
        photograph(app, "found-host")
    }

    /// Puts the machine on this network and hands over what a browser would
    /// have resolved on it.
    private func handOverWhatIsOnTheNetwork() throws {
        advertised = try announce()
        try door(runner, .init(kind: "found", hosts: [advertised]))
    }

    /// What the machine looks like to a browser, the moment it announces.
    @discardableResult
    private func announce() throws -> [String: Any] {
        let answer = try control.ask(["Announce": ["daemon": "workstation"]])
        return try XCTUnwrap((answer["Ack"] as? [String: Any])?["found"] as? [String: Any],
                             "the runner announced nothing")
    }

    // MARK: - Six digits

    /// The code the machine printed, typed on the keypad.
    private func sixDigitsOffTheMachine() throws {
        press(app, "hosts.pair.\(cast.workstation)")
        waitFor(app, "pin", "the offer to pair did not lead to the keypad")
        photograph(app, "code-entry")
        let printed = try code()
        let head = String(printed.dropLast())
        for digit in head { press(app, "pin.key.\(digit)") }
        XCTAssertEqual(try waitForValue(runner, "pin.code", head), head,
                       "the keypad did not take the digits it was given")
        press(app, "pin.key.\(printed.last ?? "0")")
        waitFor(app, "pair-confirm.trust", "a code the machine printed reached no confirmation")
    }

    /// A code that machine has just printed.
    private func code() throws -> String {
        let answer = try control.ask(
            ["StartPinPairing": ["daemon": "workstation", "ttl_secs": 600]])
        return try XCTUnwrap((answer["Ack"] as? [String: Any])?["pin"] as? String,
                             "workstation printed no code")
    }

    // MARK: - Trusted, and what it is running

    /// The machine named and its key read before anything is written, then
    /// trusted — and what it is running arriving over the link that opens.
    private func theMachineAndWhatItIsRunning() throws {
        let offered = try declared(runner)
        record["confirmedMachine"] = said(offered, "pair-confirm.name")?.value ?? ""
        let key = (said(offered, "pair-confirm.fingerprint")?.value ?? "")
            .replacingOccurrences(of: " ", with: "")
        record["confirmedKey"] = key
        XCTAssertEqual(said(offered, "pair-confirm.name")?.value, "workstation",
                       "the code was answered by a machine nobody typed it for")
        XCTAssertEqual(key, cast.key, "the key on screen is not the key workstation holds")
        press(app, "pair-confirm.trust")
        waitFor(app, "pair-confirm.trusted", "trusting workstation was never confirmed")
        press(app, "pair-confirm.done")
        waitFor(app, "hosts", "the pairing did not lead back to the machines")
        waitFor(app, "hosts.row.\(cast.workstation)", "workstation never joined the machines")
        photograph(app, "paired")

        let held = try inventory().devices
        record["workstationDevices"] = held.map { $0["name"] as? String ?? "" }
        XCTAssertEqual(held.count, 1,
                       "workstation holds \(held.count) keys after one phone paired with it")
        XCTAssertTrue(waitUntil { (try? self.fleetOnScreen())?.contains(self.cast.agent) == true },
                      "the phone never saw what workstation is running")
        record["agentsAfterPairing"] = try fleetOnScreen()
        record["machinesAfterPairing"] = try machinesOnScreen()
        // Said out loud because the whole claim rests on it: there is no relay
        // in this story, so everything above arrived over a link this phone
        // opened to an address its own browser resolved.
        record["accountsAfterPairing"] = try accountsOnThisPhone()
        XCTAssertEqual(try accountsOnThisPhone(), 0,
                       "pairing on this network signed somebody in")
    }

    /// Trusts workstation by the code it prints, through the door rather than
    /// through the keypad. The same two steps the pairing screen takes.
    private func trustThroughTheDoor() throws {
        guard try !machinesOnScreen().contains("workstation") else { return }
        try door(runner, .init(kind: "pairByCode", host: cast.workstation, pin: try code()))
        XCTAssertTrue(waitUntil { (try? self.machinesOnScreen())?.contains("workstation") == true },
                      "the shortcut did not leave workstation paired")
    }

    // MARK: - Off this network and back

    /// The machine leaves this network and comes back to it.
    ///
    /// Leaving is both halves of what leaving is: the goodbye its advertisement
    /// sends, and the machine no longer answering at the address that
    /// advertisement named. A goodbye alone is a machine that is still there.
    private func theMachineLeavesThisNetworkAndComesBack() throws {
        try control.ask(["Withdraw": ["daemon": "workstation"]])
        try control.ask(["UdpBlocked": ["daemon": "workstation", "blocked": true]])
        try door(runner, .init(kind: "found", hosts: []))
        let gone = waitUntil(within: 120) {
            (try? self.said(self.declared(self.runner, settling: false),
                            "hosts.row.\(self.cast.workstation)")?.value) == "offline"
        }
        record["afterItLeft"] = said(try declared(runner), "hosts.row.\(cast.workstation)")?.value ?? ""
        record["offersWhileItWasGone"] = offersOnScreen()
        XCTAssertTrue(gone, "the machine left this network and the phone still reads it as reachable")

        try control.ask(["UdpBlocked": ["daemon": "workstation", "blocked": false]])
        try handOverWhatIsOnTheNetwork()
        let back = waitUntil(within: 120) {
            (try? self.said(self.declared(self.runner, settling: false),
                            "hosts.row.\(self.cast.workstation)")?.value) == "reachable"
        }
        record["afterItCameBack"] = said(try declared(runner), "hosts.row.\(cast.workstation)")?.value ?? ""
        record["accountsWhenItCameBack"] = try accountsOnThisPhone()
        XCTAssertTrue(back, "the machine came back to this network and the phone never reached it")
    }

    // MARK: - A network this phone is not allowed to look at

    /// The system refused, and the screen says what that means and where it is
    /// undone.
    private func aNetworkThisPhoneIsNotAllowedToLookAt() throws {
        try door(runner, .init(kind: "localNetwork", permission: "denied"))
        waitFor(app, "hosts.localNetwork.refused",
                "a refused local network is not explained anywhere")
        let refused = try declared(runner)
        record["refusalHeadline"] = said(refused, "hosts.localNetwork.refused")?.value ?? ""
        record["refusalSettings"] = said(refused, "hosts.localNetwork.settings")?.label ?? ""
        record["refusalExplained"] =
            onScreen(app, "Turn on Local Network for amux in Settings") != nil
        XCTAssertEqual(said(refused, "hosts.localNetwork.refused")?.value,
                       "amux cannot see this network",
                       "the refusal is on screen without saying what it is")
        XCTAssertNotNil(onScreen(app, "Turn on Local Network for amux in Settings"),
                        "the refusal never says how it is undone")
        XCTAssertEqual(said(refused, "hosts.localNetwork.settings")?.label, "Open Settings",
                       "the refusal explains itself with nowhere to go")
        photograph(app, "permission-refused")
    }

    // MARK: - What each side says

    /// What the phone's own runtime has arrived at.
    private func bridge() throws -> [String: Any] {
        try XCTUnwrap(try door(runner, .init(kind: "bridge"))["bridge"] as? [String: Any],
                      "the door said nothing about the runtime")
    }

    private func started() throws -> Bool { try bridge()["started"] as? Bool ?? false }

    private func machinesOnScreen() throws -> [String] {
        try bridge()["hosts"] as? [String] ?? []
    }

    private func fleetOnScreen() throws -> [String] {
        try bridge()["agents"] as? [String] ?? []
    }

    /// The machines offered on this network, by the names the screen declares.
    private func offersOnScreen() -> [String] {
        identifiers(app, startingWith: "hosts.offer.")
    }

    /// How many accounts this phone knows, which throughout this journey is
    /// none.
    private func accountsOnThisPhone() throws -> Int {
        let answer = try door(runner, .init(kind: "accounts"))
        let accounts = (answer["known"] as? [String: Any])?["accounts"] as? [[String: Any]]
        return accounts?.count ?? 0
    }

    /// What the machine itself says it is running and whom it trusts.
    private func inventory() throws -> (agents: [[String: Any]], devices: [[String: Any]]) {
        let answer = try control.ask(["Inventory": ["daemon": "workstation"]])
        let ack = answer["Ack"] as? [String: Any] ?? [:]
        return (ack["agents"] as? [[String: Any]] ?? [], ack["devices"] as? [[String: Any]] ?? [])
    }
}
