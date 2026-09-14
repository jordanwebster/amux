import Foundation
import XCTest

/// One phone, one machine on its network, and the accounts coming and going
/// over the top of it.
///
/// A machine you paired with is yours because you were standing next to it,
/// not because of who was signed in at the time. That is the rule this drives,
/// in all three directions: the first account adopts what the phone already
/// had, signing out leaves it exactly where it was, and a second account is a
/// second device as far as every machine is concerned — it starts with
/// nothing, and the first account is still there to go back to.
///
/// The machine is on this network and signed in to nobody, which is what makes
/// the story readable: nothing it does depends on an account, so everything
/// that changes on screen is the phone's own doing.
final class ProfilesTests: JourneyCase {
    /// The two accounts, as the runner minted them.
    private struct Cast {
        let personal: String
        let personalToken: String
        let work: String
        let workToken: String
        let workstation: String

        init(_ environment: [String: String]) throws {
            func required(_ name: String) throws -> String {
                try XCTUnwrap(environment[name], "the journey did not pass \(name)")
            }
            personal = try required("AMUX_USER")
            personalToken = try required("AMUX_TOKEN")
            work = try required("AMUX_WORK_USER")
            workToken = try required("AMUX_WORK_TOKEN")
            workstation = try required("AMUX_WORKSTATION")
        }
    }

    private var runner: Runner!
    private var cast: Cast!
    private var control: Lines!
    private var app: XCUIApplication!

    func testTheMachineThisPhonePairedWithSurvivesEveryAccountItHas() throws {
        runner = try Runner()
        cast = try Cast(ProcessInfo.processInfo.environment)
        control = try Lines(address: runner.control)
        defer { try? write("profiles.json") }

        try pairedWithNobodySignedIn()
        try theFirstAccountAdoptsWhatThePhoneHad()
        try signingOutLeavesTheMachineWhereItIs()
        try aSecondAccountIsASecondDevice()
    }

    // MARK: - Nobody signed in

    /// The phone pairs with the machine on its own network before it has ever
    /// had an account.
    private func pairedWithNobodySignedIn() throws {
        app = launch(runner, signedIn: false)
        XCTAssertTrue(waitUntil { (try? self.started()) == true },
                      "a phone with no account started no runtime of its own")
        try handOverWhatIsOnTheNetwork()
        try door(runner, .init(kind: "pairByCode", host: cast.workstation, pin: try code()))
        waitFor(app, "home.row.\(runner.agent)", "what the machine is running never arrived")
        record["accountsBeforeSigningIn"] = try accounts()
        let paired = try machineOnScreen()
        record["machineBeforeSigningIn"] = paired
        XCTAssertEqual(paired, "on-this-network",
                       "the machine this phone paired with reads \(paired)")
    }

    /// Puts the machine on this network and hands over what a browser would
    /// have resolved there. Said through the door because only the system may
    /// browse, and a simulator's browser looks at this Mac's network.
    private func handOverWhatIsOnTheNetwork() throws {
        let answer = try control.ask(["Announce": ["daemon": runner.host]])
        let advertised = try XCTUnwrap(
            (answer["Ack"] as? [String: Any])?["found"] as? [String: Any],
            "the runner announced nothing")
        try door(runner, .init(kind: "found", hosts: [advertised]))
    }

    // MARK: - The first account

    /// Signing in for the first time: the account takes over the profile this
    /// phone was already on, so the machine it paired with is still its
    /// machine and the key it paired with is still the key.
    private func theFirstAccountAdoptsWhatThePhoneHad() throws {
        try signIn(as: cast.personal, with: cast.personalToken)
        record["accountsAfterSigningIn"] = try accounts()
        // Signing in restarts the runtime on the profile the phone was
        // already on, so for a moment the machine is a name this phone has no
        // live route to and reads offline. Where it settles is the promise —
        // exactly where it was — and it is given the same room to get there
        // as signing back out is given below.
        XCTAssertTrue(waitUntil { (try? self.machineOnScreen()) == "on-this-network" },
                      "signing in lost the machine this phone had paired with: it reads "
                      + "\((try? machineOnScreen()) ?? "nothing")")
        let adopted = try machineOnScreen()
        record["machineAfterSigningIn"] = adopted
        waitFor(app, "home.row.\(runner.agent)", "signing in lost what the machine is running")
        photograph(app, "profile-adopted")
    }

    /// The credential an account service would have handed over, put to the
    /// app the way a sign-in's is.
    private func signIn(as user: String, with token: String) throws {
        try door(runner, .init(kind: "connect", relay: runner.relay, token: token, user: user))
        XCTAssertTrue(waitUntil { (try? self.selected()) == user },
                      "\(user) never came to be the account on screen")
        try handOverWhatIsOnTheNetwork()
    }

    // MARK: - Signing out

    /// Leaving the account: the phone keeps everything. The account stays
    /// listed with Sign In beside it, and the machine on this network is still
    /// on this network — nothing about reaching it was ever the account's.
    private func signingOutLeavesTheMachineWhereItIs() throws {
        pressTab(app, "You")
        waitFor(app, "you.signOut", "the account offered no way out of itself")
        press(app, "you.signOut")
        XCTAssertTrue(waitUntil { (try? self.signedIn()) == [] },
                      "signing out left \(String(describing: try? self.signedIn())) signed in")
        record["accountsAfterSigningOut"] = try accounts()
        pressTab(app, "Hosts")
        XCTAssertTrue(waitUntil { (try? self.machineOnScreen()) == "on-this-network" },
                      "signing out lost the machine: it reads "
                      + "\((try? machineOnScreen()) ?? "nothing")")
        record["machineAfterSigningOut"] = try machineOnScreen()
        pressTab(app, "Agents")
        waitFor(app, "home.row.\(runner.agent)", "signing out lost what the machine is running")
        photograph(app, "signed-out-keeps-the-machine")
    }

    // MARK: - A second account

    /// Another account on the same phone is another device: its own profile,
    /// its own key, and no machines at all until somebody pairs it with one.
    /// The first account is still listed, and still has what it had.
    private func aSecondAccountIsASecondDevice() throws {
        try signIn(as: cast.work, with: cast.workToken)
        pressTab(app, "Hosts")
        // The machine is still on this network and the browser still finds it,
        // so it is on this screen — as something this account could pair with
        // and nothing else. What it is not is one of this account's machines:
        // the key the first account wrote is the first account's.
        waitFor(app, "hosts.offer.\(cast.workstation)",
                "the second account was not even offered the machine on its network")
        XCTAssertTrue(waitUntil { (try? self.machineOnScreen()) == "" },
                      "the second account inherited the machine: it reads "
                      + "\((try? machineOnScreen()) ?? "nothing")")
        record["accountsOnTheSecondAccount"] = try accounts()
        let inherited = try machineOnScreen()
        record["machineOnTheSecondAccount"] = inherited
        record["offeredOnTheSecondAccount"] =
            said(try declared(runner), "hosts.offer.\(cast.workstation)")?.value ?? ""
        XCTAssertEqual(inherited, "",
                       "the second account can see the first account's machine")
        pressTab(app, "Agents")
        waitFor(app, "home.empty.firstRun", "the second account inherited agents")
        photograph(app, "second-profile")

        let known = try accounts()
        XCTAssertEqual(known.map { $0["id"] as? String ?? "" }.sorted(),
                       [cast.personal, cast.work].sorted(),
                       "the phone lists \(known.map { $0["id"] as? String ?? "" })")
        let signedInNow = try signedIn()
        XCTAssertEqual(signedInNow, [cast.work], "the phone has \(signedInNow) signed in")
    }

    // MARK: - What each side says

    private func code() throws -> String {
        let answer = try control.ask(
            ["StartPinPairing": ["daemon": runner.host, "ttl_secs": 600]])
        return try XCTUnwrap((answer["Ack"] as? [String: Any])?["pin"] as? String,
                             "\(runner.host) printed no code")
    }

    /// Where the phone says it can reach the machine from, or nothing at all
    /// where this profile has never heard of it.
    private func machineOnScreen() throws -> String {
        said(try declared(runner, settling: false), "hosts.row.\(cast.workstation)")?.value ?? ""
    }

    private func known() throws -> [String: Any] {
        try XCTUnwrap(try door(runner, .init(kind: "accounts"))["known"] as? [String: Any],
                      "the door said nothing about the accounts")
    }

    private func accounts() throws -> [[String: Any]] {
        try known()["accounts"] as? [[String: Any]] ?? []
    }

    private func selected() throws -> String {
        try known()["selected"] as? String ?? ""
    }

    private func signedIn() throws -> [String] {
        try accounts().filter { $0["signedIn"] as? Bool == true }
            .map { $0["id"] as? String ?? "" }
    }

    private func started() throws -> Bool {
        let bridge = try door(runner, .init(kind: "bridge"))["bridge"] as? [String: Any]
        return bridge?["started"] as? Bool ?? false
    }
}
