import Foundation
import XCTest

/// Everything a person does about the account this phone reaches its machines
/// with: signing in, paying, signing in again as somebody else, moving between
/// the two, leaving one and giving one up for good.
///
/// It is one launch from beginning to end, because that is what it is about.
/// The accounts a phone knows are held while the app is running, so a test
/// that relaunched between acts would be asking a fresh phone each question —
/// and the questions here are about what the phone remembers from the last
/// answer.
///
/// Nothing reaches a real account service or a real App Store. Both are behind
/// a boundary the app is handed at launch, and the doubles it gets instead
/// answer whatever this test has last said they answer, including the failures
/// and the waiting. That is the only way a purchase sheet — another process,
/// with somebody's money behind it — can be a state a finger reaches. The
/// relay under the two accounts is real, and so are the two machines and the
/// agent that asks for something: what an account that is not on screen has
/// waiting is a number only a live connection can produce.
final class AccountsTests: JourneyCase {
    /// The machines, the accounts and the agent, as the journey named them.
    private struct Cast {
        let laptop: String
        let studio: String
        let workToken: String
        let workAgent: String

        init(_ environment: [String: String]) throws {
            func required(_ name: String) throws -> String {
                try XCTUnwrap(environment[name], "the journey did not pass \(name)")
            }
            laptop = try required("AMUX_LAPTOP")
            studio = try required("AMUX_STUDIO")
            workToken = try required("AMUX_WORK_TOKEN")
            workAgent = try required("AMUX_WORK_AGENT")
        }
    }

    /// Who this phone signs in as. The identifier is the account service's and
    /// is also the name the relay knows that account by, because a credential
    /// is minted for an account and the runtime asks for it by name.
    private enum Who {
        static let personal = (id: "personal", email: "ada@example.com", name: "Ada")
        static let work = (id: "work", email: "team@acme.example", name: "Acme")
    }

    private var runner: Runner!
    private var cast: Cast!
    private var control: Lines!
    /// The one launch every act works in.
    private var app: XCUIApplication!

    /// One act of this journey: what a person does in it, and the cheapest way
    /// to leave behind what doing it leaves behind.
    ///
    /// The journey is the whole of them in order, and that is the only run its
    /// claim is made from. A run told which acts to drive drives those and
    /// takes the shortcut for every act before them, which is how somebody
    /// reproduces one failing act without paying for the whole story again.
    private struct Act {
        let name: String
        let perform: () throws -> Void
        /// What the act leaves behind, reached the short way: the same presses
        /// with none of the outcomes the act exists to show.
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
            // The launch itself is what this act is about, so there is
            // nothing for it to leave behind.
            Act("unsigned-launch", aPhoneNobodyHasSignedInOn),
            Act("sign-in", signingInAndTheTwoWaysItDoesNot,
                shortcut: { try self.signIn(as: Who.personal, entitlement: "none") }),
            Act("subscribe", buyingTheSubscription,
                shortcut: { try self.subscribeTheShortWay() }),
            Act("second-account", aSecondAccountOnTheSamePhone,
                shortcut: {
                    try self.signIn(as: Who.work, entitlement: "active", source: "web")
                    try self.selectAccount(Who.work)
                }),
            Act("switching", movingBetweenTwoAccountsWithMachinesUnderThem,
                shortcut: { try self.selectAccount(Who.personal) }),
            Act("signed-out-account", leavingAnAccountAndComingBackToIt,
                shortcut: { try self.selectAccount(Who.work) }),
            Act("delete", givingAnAccountUpForGood,
                shortcut: { try self.selectAccount(Who.personal) }),
            Act("help-and-appearance", theThingsThatBelongToThePhone),
        ]
    }

    func testAPhoneSignsInSubscribesSwitchesAndGivesAnAccountUp() throws {
        runner = try Runner()
        cast = try Cast(ProcessInfo.processInfo.environment)
        control = try Lines(address: runner.control)
        // Written whatever happens: what the phone showed is how a failure
        // here is understood afterwards, and a run that stopped at the first
        // bad assertion has the most to explain.
        defer { try? write("accounts.json") }

        // The launch nobody has signed in on. Every act happens in it.
        app = launch(runner, signedIn: false, scripted: true)

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
        // Said out loud in the record, so a run which took shortcuts can never
        // be read afterwards as the journey itself.
        var performed: [String] = []
        var shortcut: [String] = []
        var seconds: [String: Int] = [:]
        record["actsPerformed"] = performed
        record["actsShortcut"] = shortcut
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

        // What the two doubles were asked, in order. A screen that read the
        // account service back after a purchase rather than believing the
        // store is only provable here.
        record["cloudCalls"] = try cloudCalls()
        record["storeCalls"] = try storeCalls()
        // The same two lists on their own, so the order they are in is
        // readable without hunting through everything else the phone said.
        try write("scripted-calls.json", [
            "cloud": try cloudCalls(), "store": try storeCalls(),
        ])
    }

    // MARK: - The acts

    /// A phone nobody has signed in on.
    private func aPhoneNobodyHasSignedInOn() throws {
        waitFor(app, "home", "the home never appeared")
        record["gateAtLaunch"] = try says("home")
        record["homeOffersAtLaunch"] = try called("home.empty.action")
        record["homeSaysAtLaunch"] = try says("home.empty.title")

        pressTab(app, "You")
        waitFor(app, "you", "the You page never appeared")
        record["youAtLaunch"] = try says("you")
        record["accountsAtLaunch"] = identifiers(app, startingWith: "account.")
        // Nothing to subscribe to, sign out of or delete: those belong to an
        // account, and there is none.
        record["accountRowsAtLaunch"] = ["you.subscription", "you.signOut", "you.delete"]
            .filter { element(app, $0).exists }
        photograph(app, "first-run")
        pressTab(app, "Agents")
    }

    /// Signing in, and the two ways it does not finish.
    private func signingInAndTheTwoWaysItDoesNot() throws {
        press(app, "home.empty.action")
        waitFor(app, "sign-in", "Sign In did not lead to the sign-in page")
        record["signInOpens"] = try says("sign-in.opens")
        record["signInOffers"] = try called("sign-in.continue")

        // Refused, in the cloud's own words rather than in words this app
        // invented for it.
        let refusal = "amux.sh has no account for this sign-in"
        try scriptCloud(["signIn": "refused", "reason": refusal])
        press(app, "sign-in.continue")
        waitFor(app, "sign-in.failed", "a refused sign-in said nothing")
        record["refusalSaid"] = try says("sign-in.failed")

        // Coming back from the browser without finishing is a decision, not a
        // failure: it leaves nothing on screen to dismiss.
        try scriptCloud(["signIn": "cancelled"])
        press(app, "sign-in.continue")
        XCTAssertTrue(
            waitUntil(within: 15) { !self.element(self.app, "sign-in.failed").exists },
            "cancelling left a failure on screen")
        record["afterCancelling"] = try says("sign-in")

        // And the account, with nothing bought.
        try signIn(as: Who.personal, entitlement: "none")
        waitFor(app, "home", "signing in did not come back to the home")
        record["gateAfterSigningIn"] = try waitForValue(runner, "home", "unsubscribed")
        record["homeOffersAfterSigningIn"] = try called("home.empty.action")
        record["accountsAfterSigningIn"] = try accountsKnown()
    }

    /// Buying the subscription: what it costs, the three ways it does not go
    /// through, and the one that does.
    private func buyingTheSubscription() throws {
        // The paywall is reached from the home, which is where the one thing
        // left to do is drawn.
        pressTab(app, "Agents")
        press(app, "home.empty.action")
        waitFor(app, "paywall", "Subscribe did not lead to the paywall")
        let offered = try declared(runner)
        record["paywallOffers"] = ["paywall.monthly", "paywall.yearly"].compactMap { id in
            said(offered, id).map { "\($0.label) \($0.value)" }
        }
        record["paywallTerms"] = said(offered, "paywall.terms")?.label
        photograph(app, "paywall")

        // The store's sheet, closed without buying. The screen is left exactly
        // where it was, with the same plan chosen.
        try scriptStore(["purchase": "cancelled"])
        press(app, "paywall.buy")
        // Ready, with the same plan still chosen: the screen's own word for
        // where it is is the plan it is offering.
        record["afterCancellingThePurchase"] = try waitForValue(runner, "paywall", "yearly")
        record["stillOffersAfterCancelling"] = try called("paywall.buy")

        // Refused by the store, in the store's own words.
        let refusal = "your payment method was declined"
        try scriptStore(["purchase": "fails", "reason": refusal])
        press(app, "paywall.buy")
        waitFor(app, "paywall.failed", "a refused purchase said nothing")
        record["purchaseRefusalSaid"] = try says("paywall.failed")

        // Taken and not finished — a child's purchase waiting on a parent, or
        // a bank asking again. Nothing is owed, and the screen stops offering
        // to buy so nobody pays twice for the same month.
        try scriptStore(["purchase": "pending"])
        press(app, "paywall.buy")
        waitFor(app, "paywall.pending", "a pending purchase said nothing")
        record["pendingSaid"] = try says("paywall.pending")
        record["offersWhilePending"] = try called("paywall.buy")

        // Restore, with nothing on this Apple Account to restore. An answer,
        // not a failure, and said as the answer to the question the button
        // asks.
        try scriptStore(["restore": "nothingToRestore"])
        press(app, "paywall.restore")
        record["restoreSaid"] = try waitForValue(
            runner, "paywall.failed", "there is nothing on this Apple Account to restore")

        // The plan chosen by hand, which is what the rest of this act buys.
        press(app, "paywall.monthly")
        record["chosenPlan"] = try says("paywall.monthly")

        // Bought, and the account service never heard about it. The purchase
        // is kept — the App Store is still holding the transaction, which is
        // what a finish would throw away — and the screen says so and offers
        // to send it again.
        try scriptStore(["purchase": "bought"])
        try scriptCloud(["recordPurchase": "network"])
        press(app, "paywall.buy")
        record["afterAPostThatNeverArrived"] = try waitForValue(
            runner, "paywall", "unconfirmed unreachable")
        record["unreachableSaid"] = try says("paywall.unconfirmed")
        record["unreachableExplained"] = try called("paywall.unconfirmed")
        record["offersWhileUnconfirmed"] = try called("paywall.buy")
        record["storeCallsWhileUnconfirmed"] = try storeCalls()

        // The account service refusing reads differently from a phone that
        // could not get through: one will not change by waiting and the other
        // will.
        let refusedPost = "that transaction belongs to another account"
        try scriptCloud(["recordPurchase": "refused", "purchaseReason": refusedPost])
        press(app, "paywall.buy")
        record["afterAPostThatWasRefused"] = try waitForValue(
            runner, "paywall", "unconfirmed refused")
        record["refusedPostSaid"] = try says("paywall.unconfirmed")
        record["refusedPostExplained"] = try called("paywall.unconfirmed")

        // Sent again and taken, and the account it was bought for still has
        // nothing: the App Store's receipt reaches amux.sh through its own
        // webhook, so the purchase and the access can be minutes apart. The
        // screen says which of the two is still happening and stays somewhere
        // a person can press.
        try scriptCloud(["recordPurchase": "accepted", "entitlement": "none"])
        press(app, "paywall.buy")
        record["afterAPostTakenWithoutAccessYet"] = try waitForValue(
            runner, "paywall", "unconfirmed switching on")
        record["switchingOnSaid"] = try says("paywall.unconfirmed")
        record["switchingOnExplained"] = try called("paywall.unconfirmed")
        record["offersWhileSwitchingOn"] = try called("paywall.buy")
        record["storeCallsWhileSwitchingOn"] = try storeCalls()

        // Asked again while it is still switching on. The transaction is the
        // account service's now, so all that is left is reading the
        // entitlement back — nothing sells a second subscription and nothing
        // posts the same purchase twice.
        let beforeAskingAgain = try cloudCalls()
        press(app, "paywall.buy")
        XCTAssertTrue(
            waitUntil { ((try? self.cloudCalls()) ?? []).count > beforeAskingAgain.count },
            "asking again while the subscription was switching on asked amux.sh nothing")
        record["afterAskingAgain"] = try waitForValue(
            runner, "paywall", "unconfirmed switching on")
        record["callsAddedByAskingAgain"] = Array(
            (try cloudCalls()).dropFirst(beforeAskingAgain.count))

        // And asked once more, with the access switched on. What the account
        // may do is read back from the account service, which is where a
        // subscription actually lives — the store's word for it is never
        // enough.
        try scriptCloud([
            "recordPurchase": "accepted", "entitlement": "active", "source": "appStore",
        ])
        press(app, "paywall.buy")
        waitFor(app, "paywall.subscribed", "retrying the purchase said nothing")
        record["subscribedSource"] = try says("paywall.subscribed")
        record["offersAfterBuying"] = try called("paywall.buy")
        record["cloudCallsAfterBuying"] = try cloudCalls()
        record["storeCallsAfterBuying"] = try storeCalls()
        press(app, "paywall.buy")
        record["gateAfterBuying"] = try waitForValue(runner, "home", "ready")
        record["entitlementAfterBuying"] = try accountsKnown()

        // The same account on a phone with nothing bought, which is what
        // Restore Purchases is for. The account service, asked afresh, says
        // nothing is bought; the App Store says otherwise; and putting it back
        // goes through the account service rather than around it.
        try signIn(as: Who.personal, entitlement: "none")
        pressTab(app, "Agents")
        press(app, "home.empty.action")
        waitFor(app, "paywall", "Subscribe did not lead to the paywall")
        try scriptStore(["restore": "bought"])
        try scriptCloud([
            "recordPurchase": "accepted", "entitlement": "active", "source": "appStore",
        ])
        press(app, "paywall.restore")
        waitFor(app, "paywall.subscribed", "a restored subscription said nothing")
        record["restoredSource"] = try says("paywall.subscribed")
        record["cloudCallsAfterRestoring"] = try cloudCalls()
        record["storeCallsAfterRestoring"] = try storeCalls()
        press(app, "paywall.buy")
        waitFor(app, "home", "Done did not come back from the paywall")

        // And one the store approves after the fact — a parent answering, a
        // bank's second factor. Nothing here presses anything: the call the
        // phone makes of the account service is the whole proof.
        let beforeApproval = try cloudCalls()
        try scriptStore(["approve": true])
        XCTAssertTrue(
            waitUntil { ((try? self.cloudCalls()) ?? []).count > beforeApproval.count },
            "a purchase the store approved never reached the account service")
        record["cloudCallsAfterApproval"] = try cloudCalls()
        record["callsAddedByTheApproval"] = Array(
            (try cloudCalls()).dropFirst(beforeApproval.count))
    }

    /// A second account on the same phone, subscribed somewhere else entirely.
    private func aSecondAccountOnTheSamePhone() throws {
        pressTab(app, "You")
        press(app, "you.add")
        waitFor(app, "sign-in", "Add Account did not lead to the sign-in page")
        try signIn(as: Who.work, entitlement: "active", source: "web")
        record["accountsAfterAdding"] = try accountsKnown()
        record["rowsAfterAdding"] = identifiers(app, startingWith: "account.")

        // Signing in somewhere else does not move anybody: what is on screen
        // is still what was on screen.
        record["selectedAfterAdding"] = try selectedAccount()
        try selectAccount(Who.work)
        record["workSubscriptionRow"] = try says("you.subscription")

        // A subscription bought on the web through the CLI is honoured here,
        // and the screen says where it came from instead of selling a second
        // one for the same thing.
        press(app, "you.subscription")
        waitFor(app, "paywall", "the subscription row did not lead to the paywall")
        record["workPaywallSource"] = try says("paywall.subscribed")
        record["sellsToTheWebSubscriber"] = element(app, "paywall.restore").exists
        press(app, "paywall.buy")
        waitFor(app, "you", "Done did not come back from the paywall")
    }

    /// Two accounts with machines under them, moved between.
    private func movingBetweenTwoAccountsWithMachinesUnderThem() throws {
        // The relay, and a credential for each account, beginning with the one
        // on screen. The runtime takes its accounts when it starts, so the
        // second one starts it again with both and leaves the same account
        // being read.
        try door(runner, .init(kind: "connect", relay: runner.relay, token: cast.workToken,
                               user: Who.work.id))
        try door(runner, .init(kind: "addAccount", token: runner.token, user: Who.personal.id))

        // The account on screen is the work one, so this is its machine.
        try pair(with: "studio", cast.studio)
        pressTab(app, "Agents")
        record["workFleet"] = try fleetOnScreen()

        // Somebody has to be reading a conversation for what an agent needs to
        // be known at all: nothing runs on a phone.
        waitFor(app, "home.row.\(cast.workAgent)",
                "the work account's machine never answered for its agent")
        press(app, "home.row.\(cast.workAgent)")
        waitFor(app, "conversation", "the work account's agent did not open")
        try control.ask([
            "AgentRaiseAsk": [
                "agent": "ship-the-release",
                "ask": ["Permission": [
                    "tool": "Bash",
                    "invocation": ["tool": "bash", "command": "./ship.sh"],
                    "scoped_directories": ["/work"],
                ]],
            ],
        ])
        waitFor(app, "ask.allow", "the agent's question never reached the phone")
        record["waitingWhileOnScreen"] = try called("ask.allow")
        // Out of the conversation the way the app offers: the fleet over it,
        // and the foot of that leads everywhere else.
        press(app, "conversation.drawer")
        press(app, "drawer.you")

        // Now the other account is on screen, and the work one is not.
        try selectAccount(Who.personal)
        try pair(with: "laptop", cast.laptop)
        record["personalFleet"] = try fleetOnScreen()

        // What the account nobody is looking at has waiting, from its own live
        // subscription. It is a number nothing on this phone could invent.
        pressTab(app, "Agents")
        press(app, "home.title")
        waitFor(app, "accounts.switcher", "the title did not open the switcher")
        let waiting = "account.\(Who.work.email).waiting"
        let reported = waitUntil { ((try? self.says(waiting)) ?? "" ?? "") != "" }
        let row = try called("account.\(Who.work.email)") ?? "nothing"
        XCTAssertTrue(
            reported,
            "the account off screen never reported what it had waiting; its row says \(row)")
        record["inactiveAccountRow"] = row
        record["inactiveAccountWaiting"] = try called(waiting)
        record["accountsWhileSwitched"] = try accountsKnown()
        photograph(app, "profiles")

        // An answer the work account's connection had already produced,
        // arriving after the switch. It is about an account nobody is looking
        // at, and it is refused: nothing of it reaches the screen.
        let before = try fleetOnScreen()
        let late = try door(runner, .init(kind: "late", account: Who.work.id))
        record["droppedLateResults"] = (late["known"] as? [String: Any])?["dropped"] as? Int
        record["fleetAfterTheLateResult"] = try fleetOnScreen()
        record["fleetBeforeTheLateResult"] = before
        // The title put the panel up and the title puts it away.
        press(app, "home.title")
    }

    /// Leaving an account, and coming back to it.
    private func leavingAnAccountAndComingBackToIt() throws {
        try selectAccount(Who.work)
        press(app, "you.signOut")
        record["accountsAfterSigningOut"] = try accountsKnown()
        // Seen from the other account, which is where an account somebody has
        // just left is looked at from: still listed, and offering to sign back
        // in rather than to be switched to.
        try selectAccount(Who.personal)
        record["signedOutRowSays"] = try says("account.\(Who.work.email)")

        // Signing back in: the account was never forgotten, so this is not
        // adding a stranger. It comes back with a subscription that has since
        // ended, which the row says rather than pretending it never existed.
        press(app, "account.\(Who.work.email)")
        waitFor(app, "sign-in", "the signed-out account did not lead to the sign-in page")
        try signIn(as: Who.work, entitlement: "lapsed", source: "web")
        record["accountsAfterSigningBackIn"] = try accountsKnown()
        try selectAccount(Who.work)
        record["lapsedSubscriptionRow"] = try says("you.subscription")
    }

    /// Giving an account up for good.
    private func givingAnAccountUpForGood() throws {
        try selectAccount(Who.work)
        press(app, "you.delete")
        waitFor(app, "delete", "Delete Account did not ask anything")
        record["deletionAsks"] = try says("delete")
        record["deleteBeforeTyping"] = element(app, "delete.confirm").isEnabled

        // The wrong address is not this account's address, and the one press
        // that cannot be undone stays unavailable.
        try type("someone@example.com", into: "delete.email")
        record["deleteAfterTheWrongAddress"] = element(app, "delete.confirm").isEnabled
        try door(runner, .init(kind: "clear", identifier: "delete.email"))
        try type(Who.work.email, into: "delete.email")
        record["deleteAfterTheRightAddress"] = element(app, "delete.confirm").isEnabled

        // The account service will not delete an account while its
        // subscription is still set to renew, and only where it was bought can
        // that be stopped.
        try scriptCloud(["deletion": "blockedByRenewal", "source": "appStore"])
        press(app, "delete.confirm")
        waitFor(app, "delete.blocked", "a refused deletion said nothing")
        record["blockedBy"] = try says("delete.blocked")
        record["blockedLeadsTo"] = try called("delete.manage")
        photograph(app, "delete")

        // Leaving for the billing and coming back finds the same question with
        // the same address still typed: the question outlives the trip.
        press(app, "delete.manage")
        app.activate()
        waitFor(app, "delete", "the question was gone after leaving for the billing")
        record["typedAfterComingBack"] = try says("delete.email")

        try scriptCloud(["deletion": "deleted"])
        press(app, "delete.confirm")
        waitForNo(app, "delete", "the account was deleted and the question stayed on screen")
        record["accountsAfterDeleting"] = try accountsKnown()
        record["selectedAfterDeleting"] = try selectedAccount()
    }

    /// The things that belong to the phone rather than to any account.
    private func theThingsThatBelongToThePhone() throws {
        pressTab(app, "You")
        waitFor(app, "you.appearance", "the You page has no appearance row")
        var appearances: [String] = []
        var pictures: [String: Data] = [:]
        for wanted in ["dark", "light", "system"] {
            press(app, "you.appearance.\(wanted)")
            appearances.append(try waitForValue(runner, "you.appearance", wanted))
            pictures[wanted] = XCUIScreen.main.screenshot().pngRepresentation
            if wanted != "system" { photograph(app, "appearance-\(wanted)") }
        }
        record["appearances"] = appearances
        // Applied to the app as a whole, live, under the thumb that pressed
        // it: the same page in the two appearances is not the same picture.
        record["appearanceRedrewTheScreen"] = pictures["dark"] != pictures["light"]

        // Reaching a person happens on the web, where the people are.
        press(app, "you.support")
        let safari = XCUIApplication(bundleIdentifier: "com.apple.mobilesafari")
        record["supportLeftTheApp"] = safari.wait(for: .runningForeground, timeout: 20)
        record["supportOpened"] = safari.textFields.firstMatch.value as? String
            ?? safari.otherElements["URL"].value as? String ?? ""
        app.activate()
        waitFor(app, "you", "the app did not come back from the support address")
    }

    // MARK: - The presses the acts are made of

    /// Signs in on the sign-in page that is already open, as somebody, with
    /// whatever the account service says they may do.
    private func signIn(
        as who: (id: String, email: String, name: String),
        entitlement: String, source: String = "appStore"
    ) throws {
        if !element(app, "sign-in").exists {
            pressTab(app, "You")
            press(app, element(app, "you.add").exists ? "you.add" : "home.empty.action")
        }
        waitFor(app, "sign-in", "there was no sign-in page to sign in on")
        try scriptCloud([
            "signIn": "succeeds", "account": who.id, "email": who.email,
            "displayName": who.name, "entitlement": entitlement, "source": source,
        ])
        press(app, "sign-in.continue")
        waitFor(app, "sign-in.signed-in", "signing in never came back with an account")
        record["lastSignedInAs"] = try says("sign-in.signed-in")
        // Done, which is the only thing left to do on a page about somewhere
        // else.
        press(app, "sign-in.continue")
        waitForNo(app, "sign-in", "the sign-in page stayed after it had finished")
    }

    /// Buys the subscription with none of the outcomes the act it stands in
    /// for exists to show.
    private func subscribeTheShortWay() throws {
        pressTab(app, "Agents")
        press(app, "home.empty.action")
        waitFor(app, "paywall", "Subscribe did not lead to the paywall")
        try scriptStore(["purchase": "bought"])
        try scriptCloud(["entitlement": "active", "source": "appStore"])
        press(app, "paywall.buy")
        waitFor(app, "paywall.subscribed", "the purchase did not go through")
        press(app, "paywall.buy")
        _ = try waitForValue(runner, "home", "ready")
    }

    /// Puts an account on screen, from wherever the app is.
    private func selectAccount(_ who: (id: String, email: String, name: String)) throws {
        pressTab(app, "You")
        waitFor(app, "you", "the You page never appeared")
        guard try selectedAccount() != who.id else { return }
        press(app, "account.\(who.email)")
        let arrived = waitUntil { (try? self.selectedAccount()) == who.id }
        let known = (try? accountsKnown()) ?? []
        XCTAssertTrue(arrived, "\(who.email) never came on screen; this phone knows \(known)")
    }

    /// Trusts one machine by the code it printed, through the door rather than
    /// through the keypad.
    ///
    /// The screens that read a code belong to the journey about pairing. What
    /// this journey is about is which account the trust is written under, and
    /// the door takes the same two steps the keypad takes against the same
    /// machine over the same relay.
    private func pair(with machine: String, _ identity: String) throws {
        let answer = try control.ask(
            ["StartPinPairing": ["daemon": machine, "ttl_secs": 600]])
        let pin = try XCTUnwrap((answer["Ack"] as? [String: Any])?["pin"] as? String,
                                "\(machine) printed no code")
        try door(runner, .init(kind: "pairByCode", host: identity, pin: pin))
        XCTAssertTrue(
            waitUntil { ((try? self.machinesOnScreen()) ?? []).contains(machine) },
            "\(machine) never joined the machines this account reaches")
    }

    /// Types into a field the way a person does, through the field itself.
    private func type(_ text: String, into identifier: String) throws {
        let field = app.textFields.matching(identifier: identifier).firstMatch
        guard field.waitForExistence(timeout: waiting) else {
            return XCTFail("there is no field named \(identifier)")
        }
        field.tap()
        field.typeText(text)
        _ = try waitForValue(runner, identifier, text)
    }

    // MARK: - Reading

    /// What one named thing on screen says its value is.
    ///
    /// Asked of the app's own door rather than read off the system's tree:
    /// a name declared on a screen reaches that tree as an identifier and
    /// nothing else, and everything the screen said beside it — the value, the
    /// label — is reported up the view tree instead.
    private func says(_ identifier: String) throws -> String? {
        said(try declared(runner), identifier)?.value
    }

    /// What the screen calls one named thing, which is what VoiceOver reads.
    private func called(_ identifier: String) throws -> String? {
        said(try declared(runner), identifier)?.label
    }

    /// Every call the phone has made of the scripted account service, in
    /// order. What a screen believed is only provable here: a purchase read
    /// back from the account service and one taken on the store's word look
    /// identical on screen.
    private func cloudCalls() throws -> [String] {
        try door(runner, .init(kind: "calls"))["cloud"] as? [String] ?? []
    }

    private func storeCalls() throws -> [String] {
        try door(runner, .init(kind: "calls"))["store"] as? [String] ?? []
    }

    /// What the account service will answer from here on. Everything unsaid
    /// keeps the answer it had.
    private func scriptCloud(_ said: [String: Any]) throws {
        try door(runner, .init(kind: "cloud", cloud: said))
    }

    private func scriptStore(_ said: [String: Any]) throws {
        try door(runner, .init(kind: "store", store: said))
    }

    /// The accounts this phone knows, as the registry every screen reads has
    /// them: who, whether they are signed in, and what each may do.
    private func accountsKnown() throws -> [String] {
        let answer = try door(runner, .init(kind: "accounts"))
        let known = (answer["known"] as? [String: Any])?["accounts"] as? [[String: Any]] ?? []
        return known.map { account in
            let email = account["email"] as? String ?? "?"
            let signedIn = (account["signedIn"] as? Bool ?? false) ? "signed in" : "signed out"
            return "\(email): \(signedIn), \(account["entitlement"] as? String ?? "?")"
        }
    }

    private func selectedAccount() throws -> String? {
        let answer = try door(runner, .init(kind: "accounts"))
        return (answer["known"] as? [String: Any])?["selected"] as? String
    }

    private func bridge() throws -> [String: Any] {
        try XCTUnwrap(try door(runner, .init(kind: "bridge"))["bridge"] as? [String: Any],
                      "the door said nothing about the runtime")
    }

    /// The agents the account on screen believes in, by name.
    private func fleetOnScreen() throws -> [String] {
        try bridge()["agents"] as? [String] ?? []
    }

    private func machinesOnScreen() throws -> [String] {
        try bridge()["hosts"] as? [String] ?? []
    }
}
