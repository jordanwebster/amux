import Foundation
import XCTest

/// A phone nobody has signed in on, after the machine it paired with leaves
/// the network.
///
/// This is the state the product is built around and the one an app that
/// assumed an account gets wrong. Nothing has failed: there is no relay
/// because nobody asked for one, and the machine is simply somewhere else. So
/// the phone keeps what it was told, says the machine is offline, and offers
/// an account underneath as the thing that would reach it from here — not as
/// something missing, and never on a phone that can reach everything it owns.
final class SignedOutTests: JourneyCase {
    private var runner: Runner!
    /// The machine, as the journey named it. The agent it is running is the
    /// runner's own `AMUX_AGENT`, which is what names the row.
    private var machine: String!
    private var control: Lines!
    private var app: XCUIApplication!

    func testAPhoneWithNoAccountKeepsItsAgentsWhenTheMachineLeavesTheNetwork() throws {
        runner = try Runner(withoutAnAccount: true)
        let environment = ProcessInfo.processInfo.environment
        machine = try XCTUnwrap(environment["AMUX_WORKSTATION"],
                                "the journey did not pass AMUX_WORKSTATION")
        control = try Lines(address: runner.control)
        defer { try? write("signed-out.json") }

        try pairedOnThisNetwork()
        try theMachineLeavesTheNetwork()
        try andComesBack()
    }

    // MARK: - Paired, on this network

    /// The phone finds the machine on its own network, pairs with it by the
    /// code it printed, and reads what it is running. No account anywhere.
    private func pairedOnThisNetwork() throws {
        app = launch(runner, signedIn: false)
        XCTAssertTrue(waitUntil { (try? self.started()) == true },
                      "a phone with no account started no runtime of its own")
        try announce()
        try door(runner, .init(kind: "pairByCode", host: machine, pin: try code()))
        waitFor(app, "home.row.\(runner.agent)", "what the machine is running never arrived")
        record["accounts"] = try accountsOnThisPhone()
        XCTAssertEqual(try accountsOnThisPhone(), 0, "this phone signed somebody in")
    }

    /// Puts the machine on this Mac's network, where the phone's own browser
    /// finds it.
    private func announce() throws {
        let answer = try control.ask(["Announce": ["daemon": runner.host]])
        XCTAssertNotNil((answer["Ack"] as? [String: Any])?["found"], "the runner announced nothing")
    }

    // MARK: - The machine leaves

    /// The machine goes: the goodbye its advertisement sends, and the machine
    /// no longer answering at the address that advertisement named.
    ///
    /// What the phone has to do about it is keep everything and change one
    /// word. The agents are still the agents — the last thing this phone was
    /// told is still the last thing that was true — and the one line above
    /// them names what an account would do, which is find that machine from
    /// somewhere that is not its network.
    private func theMachineLeavesTheNetwork() throws {
        try control.ask(["Withdraw": ["daemon": runner.host]])
        try control.ask(["UdpBlocked": ["daemon": runner.host, "blocked": true]])
        let gone = waitUntil(within: 120) {
            (try? self.said(self.declared(self.runner, settling: false),
                            "home.row.\(self.runner.agent)")?.value)?
                .hasPrefix("host-offline") == true
        }
        let home = try declared(runner)
        record["agentWhenItLeft"] = said(home, "home.row.\(runner.agent)")?.value ?? ""
        record["agentSaysWhenItLeft"] = said(home, "home.row.\(runner.agent)")?.label ?? ""
        record["homeLineWhenItLeft"] = said(home, "home.exceptions")?.value ?? ""
        record["agentsWhenItLeft"] = try fleetOnScreen()
        XCTAssertTrue(gone, "the machine left and its agent reads "
                      + "\(said(home, "home.row.\(runner.agent)")?.value ?? "nothing")")
        XCTAssertEqual(said(home, "home.exceptions")?.value,
                       "Sign in to reach your agents from anywhere",
                       "the one line above the list reads "
                       + "\(said(home, "home.exceptions")?.value ?? "nothing")")
        photograph(app, "signed-out-offline")

        pressTab(app, "Hosts")
        let machines = try declared(runner)
        record["machineWhenItLeft"] = said(machines, "hosts.row.\(machine!)")?.value ?? ""
        XCTAssertEqual(said(machines, "hosts.row.\(machine!)")?.value, "offline",
                       "the machine that left reads "
                       + "\(said(machines, "hosts.row.\(machine!)")?.value ?? "nothing")")
        pressTab(app, "Agents")
    }

    // MARK: - And comes back

    /// The machine announces itself again and the phone reaches it, with
    /// nobody pressing anything — and with nothing on the home asking for an
    /// account, because there is nothing an account would add.
    private func andComesBack() throws {
        try control.ask(["UdpBlocked": ["daemon": runner.host, "blocked": false]])
        try announce()
        let back = waitUntil(within: 120) {
            (try? self.said(self.declared(self.runner, settling: false),
                            "home.row.\(self.runner.agent)")?.value)?
                .hasPrefix("host-offline") == false
        }
        let home = try declared(runner)
        record["agentWhenItCameBack"] = said(home, "home.row.\(runner.agent)")?.value ?? ""
        record["homeLineWhenItCameBack"] = said(home, "home.exceptions")?.value ?? ""
        record["accountsWhenItCameBack"] = try accountsOnThisPhone()
        XCTAssertTrue(back, "the machine came back and its agent still reads "
                      + "\(said(home, "home.row.\(runner.agent)")?.value ?? "nothing")")
        XCTAssertNil(said(home, "home.exceptions"),
                     "a phone that can reach everything it owns is still being told something: "
                     + "\(said(home, "home.exceptions")?.value ?? "")")
        XCTAssertEqual(try accountsOnThisPhone(), 0, "reaching the machine signed somebody in")
        photograph(app, "signed-out-reachable")
    }

    // MARK: - What each side says

    /// A code the machine has just printed.
    private func code() throws -> String {
        let answer = try control.ask(
            ["StartPinPairing": ["daemon": runner.host, "ttl_secs": 600]])
        return try XCTUnwrap((answer["Ack"] as? [String: Any])?["pin"] as? String,
                             "\(runner.host) printed no code")
    }

    private func bridge() throws -> [String: Any] {
        try XCTUnwrap(try door(runner, .init(kind: "bridge"))["bridge"] as? [String: Any],
                      "the door said nothing about the runtime")
    }

    private func started() throws -> Bool { try bridge()["started"] as? Bool ?? false }

    private func fleetOnScreen() throws -> [String] { try bridge()["agents"] as? [String] ?? [] }

    private func accountsOnThisPhone() throws -> Int {
        let answer = try door(runner, .init(kind: "accounts"))
        let accounts = (answer["known"] as? [String: Any])?["accounts"] as? [[String: Any]]
        return accounts?.count ?? 0
    }
}
