import AmuxCore
import AmuxFeatures
import Foundation

/// Every named state this build can be put into.
///
/// One per screen the design describes, plus the states a screenshot of a good
/// morning never shows: a host lost mid-turn, an agent this build cannot read,
/// a send that was refused, an upload that failed, and the whole thing at an
/// accessibility type size.
public enum Fixtures {
    public static let all: [Fixture] = catalogue + states

    /// What the cloud says when it turns a sign-in away. Written once because
    /// two states read it: what the scripted cloud throws, and what the screen
    /// that has already been thrown at draws.
    public static let refusedSignIn = "that address is not recognised"

    /// What the App Store says when it will not take a payment. Written once
    /// for the same reason: the scripted store throws it and the screen that
    /// has already been thrown at draws it.
    public static let refusedPurchase = "your payment method was declined"

    public static func named(_ id: String) -> Fixture? {
        all.first { $0.id == id }
    }

    /// One state this build can be asked for: a screen and what fills it.
    public struct Built: Hashable, Sendable {
        public let screen: Screen
        public let state: String

        public init(_ screen: Screen, _ state: String) {
            self.screen = screen
            self.state = state
        }
    }

    /// Every state this build draws, in the order the catalogue describes
    /// them, so a sweep over the whole app reads the same list the door
    /// answers `open` from rather than a copy of it.
    public static var drawn: [Fixture] {
        all.filter { isBuilt($0.screen, state: $0.id) }
    }

    /// Whether this build draws that state.
    ///
    /// Built-ness belongs to the pair, not to the screen. The conversation
    /// screen draws a conversation, but the conversation whose host was lost
    /// mid-turn, the one stripped back to its rows and the one at an
    /// accessibility type size are separate states with separate baselines.
    /// Declared per screen, all of them became openable the moment the first
    /// one landed, and a check of "everything built so far still draws what it
    /// was locked as" started failing on work nobody had started.
    ///
    /// Asked before the state is looked up, so a state nobody has written yet
    /// answers "unimplemented" rather than "no state named": not having been
    /// written is exactly what being unbuilt means.
    public static func isBuilt(_ screen: Screen, state: String) -> Bool {
        built.contains(Built(screen, state))
    }

    /// Every state that is drawn and locked today. A state joins this list in
    /// the same commit that establishes its baseline.
    static let built: Set<Built> = [
        Built(.probe, "probe"),
        Built(.home, "home"),
        Built(.home, "home-accessibility"),
        Built(.run, "run-accessibility"),
        Built(.typing, "composer-accessibility"),
        Built(.home, "home-unreadable"),
        Built(.homeQuiet, "home-quiet"),
        Built(.drawer, "drawer"),
        Built(.run, "run"),
        Built(.run, "host-lost"),
        Built(.typing, "typing"),
        Built(.typing, "tokens"),
        Built(.slashTyping, "slash-typing"),
        Built(.plus, "plus"),
        Built(.settings, "settings"),
        Built(.settings, "permissions-claude"),
        Built(.settings, "permissions-codex"),
        Built(.overflow, "overflow"),
        Built(.overflow, "rename"),
        Built(.agentDelete, "agent-delete"),
        Built(.working, "working"),
        Built(.queued, "queued"),
        Built(.run, "strip"),
        Built(.runLive, "run-live"),
        Built(.working, "send-refused"),
        Built(.exited, "exited"),
        Built(.voices, "voices"),
        Built(.reviewCta, "review-cta"),
        Built(.reviewCta, "finished"),
        Built(.askPermission, "ask-permission"),
        Built(.askPermission, "ask-permission-codex"),
        Built(.askQuestion, "ask-question"),
        Built(.plan, "plan"),
        Built(.diff, "diff"),
        Built(.comment, "comment"),
        Built(.firstRun, "first-run"),
        Built(.firstRunPaid, "first-run-paid"),
        Built(.signIn, "sign-in"),
        Built(.signIn, "sign-in-failed"),
        Built(.profiles, "profiles"),
        Built(.you, "you"),
        Built(.delete, "delete"),
        Built(.delete, "delete-blocked"),
        Built(.paywall, "paywall"),
        Built(.paywall, "paywall-web"),
        Built(.paywall, "paywall-pending"),
        Built(.paywall, "paywall-failed"),
        Built(.hosts, "hosts"),
        Built(.hosts, "devices"),
        Built(.pin, "pin"),
        Built(.pairConfirm, "pair-confirm"),
        Built(.newAgent, "new-agent"),
        Built(.offline, "offline"),
        Built(.shake, "shake"),
        Built(.dump, "dump"),
        Built(.dump, "upload-failed"),
    ]

    /// The screens the design catalogue describes, in its own order.
    public static let catalogue: [Fixture] = [
        // The harness's own target, which is not a screen of the app.
        Fixture(id: "probe", screen: .probe),

        // 1 · Opening the app
        Fixture(id: "home", screen: .home) { bundle in
            States.open(bundle)
        },
        Fixture(id: "home-quiet", screen: .homeQuiet) { bundle in
            // Nothing blocked and nothing unread: the exceptions line appears
            // only because one machine is actually unreachable.
            States.open(bundle, agents: Scenario.settledAgents, unread: Scenario.allRead)
        },
        Fixture(id: "drawer", screen: .drawer) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },

        // 2 · A conversation
        Fixture(id: "run", screen: .run) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
        Fixture(id: "run-live", screen: .runLive) { bundle in
            States.open(bundle, entries: Transcript.live,
                        session: Sessions.claude(gate: .working, phase: "running"))
        },
        Fixture(id: "voices", screen: .voices) { bundle in
            States.open(
                bundle, entries: Transcript.everyKind,
                session: Sessions.claude(family: Sessions.family))
        },
        Fixture(id: "review-cta", screen: .reviewCta) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(), changes: Transcript.changes)
        },

        // 3 · When it needs you
        Fixture(id: "ask-permission", screen: .askPermission) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(gate: .needsYou, asks: [Sessions.claudePermission]))
        },
        Fixture(id: "ask-question", screen: .askQuestion) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(gate: .needsYou, asks: [Sessions.claudeQuestion]))
        },
        Fixture(id: "plan", screen: .plan) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(gate: .needsYou, asks: [Sessions.claudePlan]))
        },
        // A review part-way through being written: two files folded away, two
        // things already said. The comments are added through the store rather
        // than handed to it, so the state a screenshot is taken of is a state
        // the app can actually reach.
        Fixture(id: "diff", screen: .diff) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(), changes: Transcript.review)
            States.reviewed(bundle)
        },
        // The same review with a range held and a remark half written.
        Fixture(id: "comment", screen: .comment) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(), changes: Transcript.review)
            States.reviewed(bundle)
            guard let review = bundle.review(Scenario.focus) else { return }
            review.select(LineRange(file: 3, from: 9, to: 10))
            review.draft = """
                The catch-all swallows Code::Internal too, which isn't a \
                pairing failure. Match the three explicitly and let the rest \
                bubble.
                """
        },

        // 4 · Writing to it
        // A message part-way through being written. The draft is put into the
        // conversation's own store rather than into the view, so the state
        // photographed here is one the app reaches by somebody typing.
        Fixture(id: "typing", screen: .typing) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
            bundle.conversation(Scenario.focus).draft.body = """
                Before you squash it, check that the relay's reconnect path \
                doesn't read INVALID_PIN by name \u{2014} I think it might, and if it \
                does this whole change needs a different shape.
                """
        },
        // A message carrying one of each thing a message can carry, with
        // ordinary words between them. Every token is made the way the app
        // makes one — the elements are the shared library's, and the review is
        // written on the review store — so what is photographed is a draft the
        // app could actually be holding.
        //
        // The agent has attached something too, in the message above the box,
        // so the two are in one picture: an attachment an agent sent and an
        // attachment you are writing are the same chip, because they are the
        // same element in the same text read by the same parser.
        Fixture(id: "tokens", screen: .typing) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy(attaching: Scenario.checkOutput),
                        session: Sessions.claude(), changes: Transcript.review)
            States.reviewed(bundle)
            let conversation = bundle.conversation(Scenario.focus)
            conversation.draft.insert(text: "Same failure as ")
            if let photo = Bridge.token(for: Scenario.screenshot) {
                conversation.draft.insert(photo)
            }
            conversation.draft.insert(text: ", trace in ")
            if let file = Bridge.token(for: Scenario.trace) {
                conversation.draft.insert(file)
            }
            conversation.draft.insert(text: ". The log around it:\n")
            conversation.draft.paste(Scenario.longPaste)
            conversation.draft.insert(text: "\n")
            if let review = bundle.review(Scenario.focus)?.token {
                conversation.draft.attach(review)
            }
        },
        Fixture(id: "plus", screen: .plus) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
        Fixture(id: "settings", screen: .settings, overlay: .settings) { bundle in
            // Model and effort on a Codex session, which is the layer this
            // build can actually change them on.
            States.open(
                bundle, entries: Transcript.codexTurn, agent: Scenario.agentId("spec-suite"),
                session: Sessions.codex())
        },
        // A command being typed. It is photographed on a Codex agent because
        // a command is only a command on a layer that takes one: the core
        // refuses a command token on a PTY Claude session outright, so that
        // session is offered nothing and there is no picture of it to take.
        Fixture(id: "slash-typing", screen: .slashTyping) { bundle in
            States.open(
                bundle, entries: Transcript.codexTurn, agent: Scenario.agentId("spec-suite"),
                session: Sessions.codex())
            bundle.conversation(Scenario.agentId("spec-suite")).draft.body = "/co"
        },
        // A turn in flight: the command is still running, the fleet says so
        // and says when it started, and the composer names both.
        Fixture(id: "working", screen: .working) { bundle in
            States.open(
                bundle, agents: Scenario.working, entries: Transcript.live,
                session: Sessions.claude(
                    gate: .working, phase: "running",
                    provider: Sessions.claudeProvider(running: Sessions.todos)))
        },
        // A message waiting for the turn to end, with everything else that is
        // true about the turn beside it: the task being worked on and its
        // count, and the three agents this one started, one of which cannot
        // continue. All four facts in one picture, which is how the design
        // draws the strip.
        Fixture(id: "queued", screen: .queued) { bundle in
            States.open(
                bundle, agents: Scenario.startedWork, entries: Transcript.live,
                session: Sessions.claude(
                    gate: .working, phase: "running",
                    provider: Sessions.claudeProvider(running: Sessions.todos),
                    queue: Sessions.heldMessage, family: Sessions.started))
        },
        Fixture(id: "overflow", screen: .overflow) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
        Fixture(id: "rename", screen: .overflow, overlay: .rename) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
        Fixture(id: "agent-delete", screen: .agentDelete) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },

        // 5 · Machines and agents
        // The phone was watching when air stopped answering: that is why the
        // row can say how long it has been gone. A machine that was already
        // away the first time the phone heard of it says only that it is.
        Fixture(id: "hosts", screen: .hosts) { bundle in
            States.open(bundle, hosts: Scenario.reachableHosts)
            States.lostHost(bundle, Scenario.air, minutesAgo: 8)
            States.trusted(bundle)
        },
        // The same screen with the keys read rather than counted: whole
        // fingerprints, this phone's first, and the one thing there is to do
        // about a machine that should not have one any more.
        Fixture(id: "devices", screen: .hosts) { bundle in
            States.open(bundle, hosts: Scenario.reachableHosts)
            States.lostHost(bundle, Scenario.air, minutesAgo: 8)
            States.trusted(bundle)
            bundle.hosts.readDevices()
        },
        // Half a code typed against the machine that printed it. The next box
        // is outlined and nothing blinks: there is no keyboard coming, so a
        // caret would be a promise the screen cannot keep.
        Fixture(id: "pin", screen: .pin) { bundle in
            States.open(bundle)
            States.offering(bundle)
            bundle.pairing.open(machine: Scenario.unpaired)
            bundle.pairing.enter("419")
        },
        // Starting an agent on the machine an agent last ran on. The Codex
        // session is here because the models a layer offers only ever arrive
        // with a running session — an account with one Codex agent is an
        // account whose Codex card has a list to open, and one without is not.
        Fixture(id: "new-agent", screen: .newAgent) { bundle in
            States.open(
                bundle, agent: Scenario.agentId("spec-suite"),
                session: Sessions.codex())
            States.offers(bundle)
        },
        // A conversation whose machine went away mid-turn. Both things are
        // true at once and both are said: the feed is the last thing that was
        // true and stays readable, and the composer — the one control that
        // would lie by staying usable — becomes where the failure is reported.
        Fixture(id: "offline", screen: .offline) { bundle in
            States.hostLost(bundle)
        },
        Fixture(id: "exited", screen: .exited) { bundle in
            var ended = Scenario.agents
            ended[0].phase = .exited(exitCode: 1)
            ended[0].attention = .unknown
            States.open(
                bundle, agents: ended, entries: Transcript.pairingCopy,
                session: Sessions.claude(gate: .exited, stream: .closed(
                    reason: .object(["reason": .string("agent_exited")]))))
        },

        // 6 · You
        // The switcher out over the home it hangs from: three accounts, one
        // selected, one signed out and offering to sign back in.
        Fixture(id: "profiles", screen: .profiles, accounts: Fixture.several) { bundle in
            States.open(bundle)
        },
        // The whole page: the accounts, what the one on screen has bought,
        // and what belongs to the phone. The device roster is filled because
        // this phone's own key is one of the rows.
        Fixture(id: "you", screen: .you, accounts: Fixture.several) { bundle in
            States.open(bundle)
            States.trusted(bundle)
        },
        // Giving up an account, asked over the page it was asked from. The
        // address is already typed, because what the button does once it is
        // typed is the whole point of the screen; the subscription renews, so
        // the third consequence is the one that has to be honest about
        // billing.
        Fixture(id: "delete", screen: .delete, accounts: renewing,
                deletion: Fixture.Deleting(
                    account: ScriptedCloudState.ada.id,
                    typed: ScriptedCloudState.ada.email)) { bundle in
            States.open(bundle)
            States.trusted(bundle)
        },
        // The same question, refused: the account service will not delete an
        // account whose subscription is still set to renew, and only the App
        // Store can stop an App Store one.
        Fixture(id: "delete-blocked", screen: .delete, cloud: ScriptedCloudState(
            deletion: .blockedByRenewal(source: .appStore, manageURL: appStoreSubscriptions)),
                accounts: renewing,
                deletion: Fixture.Deleting(
                    account: ScriptedCloudState.ada.id,
                    typed: ScriptedCloudState.ada.email,
                    phase: .blocked(source: .appStore, manageURL: appStoreSubscriptions))
        ) { bundle in
            States.open(bundle)
            States.trusted(bundle)
        },
        Fixture(id: "first-run", screen: .firstRun, cloud: .firstRun, accounts: []),
        Fixture(id: "sign-in", screen: .signIn, cloud: .firstRun, accounts: []),
        Fixture(id: "first-run-paid", screen: .firstRunPaid, cloud: .unsubscribed,
                accounts: [Fixture.unsubscribed]),
        // Nothing bought: the two plans and the price of each. The account
        // is signed in and unsubscribed, because an account that already pays
        // is not shown a paywall at all.
        Fixture(id: "paywall", screen: .paywall, cloud: .unsubscribed,
                accounts: [Fixture.unsubscribed]),

        // 7 · When it goes wrong
        Fixture(id: "shake", screen: .shake) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
        // The report, on the frame the screenshot froze: one box drawn around
        // the row that is wrong, its own note under the picture, and one note
        // about the whole thing. The rectangle is in the frame's own points,
        // which is what a reader on a Mac puts back.
        Fixture(id: "dump", screen: .dump, report: Fixture.Reporting(
            // The note is written on two lines rather than left to wrap.
            // A vertical text field settles a few points wider or narrower
            // depending on how much of the page is scrollable, and this state
            // is almost exactly one screen tall, so a note left to find its
            // own wrap point broke on a different word on about one run in
            // two. Where a person's note wraps is the field's business; where
            // this one wraps is the fixture's.
            note: "Queued message stays on screen\nafter sending",
            marks: [ReportMark(
                x: 24, y: 236, width: 354, height: 30,
                note: "this row never leaves once the\nmessage has gone")])
        ) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
    ]

    /// Where the App Store keeps a person's own subscriptions. The system's
    /// page, not one amux owns, which is the whole reason a blocked deletion
    /// has to send somebody out of the app.
    static let appStoreSubscriptions = URL(
        string: "https://apps.apple.com/account/subscriptions")!

    /// The accounts of the two deletion states: the same phone the You page
    /// shows, with the account being given up paying a subscription that is
    /// still set to renew. A renewal is what the account service refuses to
    /// delete around, so a state about deleting has to have one.
    static let renewing: [AccountEntry] = {
        var accounts = Fixture.several
        accounts[0].entitlement = .active(
            source: .appStore, renews: Scenario.now.addingTimeInterval(11 * 24 * 60 * 60))
        return accounts
    }()

    /// States a screenshot of a good morning never shows.
    public static let states: [Fixture] = [
        // Both permission vocabularies. Codex asks in its own words and offers
        // its own choices; flattening the two into one would put words in a
        // provider's mouth.
        Fixture(id: "ask-permission-codex", screen: .askPermission) { bundle in
            States.open(
                bundle, entries: Transcript.codexTurn, agent: Scenario.agentId("spec-suite"),
                session: Sessions.codex(gate: .needsYou, asks: [Sessions.codexPermission]))
        },
        // Both permission vocabularies, on the layer that speaks each. Claude
        // runs under a mode and reports which; Codex runs under a preset over
        // two axes and both are named. Flattening them into one invented set
        // would put words in a provider's mouth.
        Fixture(id: "permissions-claude", screen: .settings, overlay: .permissions) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
        Fixture(id: "permissions-codex", screen: .settings, overlay: .permissions) { bundle in
            States.open(
                bundle, entries: Transcript.codexTurn, agent: Scenario.agentId("spec-suite"),
                session: Sessions.codex())
        },
        // The same finished turn once the fleet says nobody has read it: the
        // panel takes the composer's place and offers the page the chip in the
        // chrome opens.
        Fixture(id: "finished", screen: .reviewCta) { bundle in
            var read = Scenario.agents
            read[0].attention = .needsYou(why: .finished)
            read[0].outcome = TurnOutcome(files: 2, insertions: 3, deletions: 6)
            States.open(
                bundle, agents: read, entries: Transcript.pairingCopy,
                session: Sessions.claude(), changes: Transcript.changes)
        },
        // Where a pairing link lands. Reaching it means a secret authenticated
        // and means nothing else: no trust has been written on this phone or
        // on the machine, and leaving writes none.
        Fixture(id: "pair-confirm", screen: .pairConfirm) { bundle in
            States.open(bundle)
            States.offering(bundle)
            States.offered(bundle)
        },
        // The host went away mid-turn. The feed stays readable and says so.
        // The same state the `offline` screen is photographed in: one is the
        // design's screen and one is the state list's, and they are one
        // picture because they are one thing that happened.
        Fixture(id: "host-lost", screen: .run) { bundle in
            States.hostLost(bundle)
        },
        // An agent this build cannot read. It is not offered to open.
        Fixture(id: "unreadable", screen: .run) { bundle in
            States.open(
                bundle, entries: [], agent: Scenario.agentId("legacy-port"),
                session: Sessions.unreadable())
        },
        // The same strip grown: the provider's whole list above the line that
        // summarises it, with the count, the started agents and the queued
        // message still where they were. The design pictures the folded strip
        // and not this, so what the list looks like open is this build's
        // answer.
        Fixture(id: "strip", screen: .run, overlay: .tasks) { bundle in
            States.open(
                bundle, agents: Scenario.startedWork, entries: Transcript.live,
                session: Sessions.claude(
                    gate: .working, phase: "running",
                    provider: Sessions.claudeProvider(running: Sessions.todos),
                    queue: Sessions.heldMessage, family: Sessions.started))
        },
        // A send the layer refused, with the reason visible and no input
        // reaching the host. It is a state of the screen the composer lives
        // on, which is where a refusal to send is read.
        Fixture(id: "send-refused", screen: .working) { bundle in
            States.open(
                bundle, entries: Transcript.pairingCopy,
                session: Sessions.claude(gate: .replaying, phase: "replaying", stream: .replaying),
                extra: [.opResult(OpResult(
                    op: OpId(UUID(uuidString: "00000000-0000-0000-0000-00000000FA11")!),
                    outcome: .failed(refusal)))])
        },
        // The report could not be sent. The draft is not lost.
        Fixture(id: "upload-failed", screen: .dump,
                cloud: ScriptedCloudState(upload: .offline),
                report: Fixture.Reporting(
                    // The note is written on two lines rather than left to wrap.
            // A vertical text field settles a few points wider or narrower
            // depending on how much of the page is scrollable, and this state
            // is almost exactly one screen tall, so a note left to find its
            // own wrap point broke on a different word on about one run in
            // two. Where a person's note wraps is the field's business; where
            // this one wraps is the fixture's.
            note: "Queued message stays on screen\nafter sending",
                    marks: [ReportMark(
                        x: 24, y: 236, width: 354, height: 30,
                        note: "this row never leaves once the\nmessage has gone")],
                    sending: .failed("offline"))) { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
        },
        // Signing in, having pressed the button once and been turned away.
        // The screen says what the cloud said, so the words on it and the
        // words the scripted cloud would throw are one string.
        // A subscription bought on the web, through the CLI, honoured here.
        // The screen says where it came from instead of selling a second one.
        Fixture(id: "paywall-web", screen: .paywall),
        // The store has taken the purchase and cannot finish it: a child
        // waiting on a parent, or a bank asking for a second factor. Nothing
        // is charged and nothing is bought.
        Fixture(id: "paywall-pending", screen: .paywall, accounts: [Fixture.unsubscribed],
                paywall: .awaitingApproval),
        // The store refused it. What it said is what the screen says.
        Fixture(id: "paywall-failed", screen: .paywall, accounts: [Fixture.unsubscribed],
                store: ScriptedStoreState(purchase: .fails(Fixtures.refusedPurchase)),
                paywall: .failed(Fixtures.refusedPurchase)),
        Fixture(id: "sign-in-failed", screen: .signIn,
                cloud: ScriptedCloudState(signIn: .refused(Fixtures.refusedSignIn),
                                          entitlement: .none, token: nil),
                signIn: .failed(Fixtures.refusedSignIn)),
        // A machine on the account running a newer amux than the phone: one of
        // its agents arrives under a provider name this build has never heard
        // of. It is listed under that name, said to be unreadable, and the
        // only row on the screen that cannot be opened.
        Fixture(id: "home-unreadable", screen: .home) { bundle in
            States.open(bundle, agents: [Scenario.unreadableAgent] + Scenario.agents)
        },
        // Nothing yet: one action, and no list pretending to be loading.
        Fixture(id: "home-empty", screen: .home) { bundle in
            States.open(bundle, agents: [], hosts: [], unread: UnreadWeights())
        },
        // The cache before the network answers: rows are shown and marked
        // unconfirmed rather than replaced by a spinner.
        Fixture(id: "home-cached", screen: .home) { bundle in
            States.open(bundle, agents: Scenario.remembered, reconciled: false)
        },
        // The same screens at the largest text size the system offers, which
        // is the one worth locking: anything that survives it survives every
        // size below it. Nothing is dropped here; it wraps, and where a line
        // cannot wrap it is allowed to shorten rather than be cut off.
        Fixture(id: "home-accessibility", screen: .home, typeSize: "accessibility5") { bundle in
            States.open(bundle)
        },
        Fixture(id: "run-accessibility", screen: .run, typeSize: "accessibility5") { bundle in
            States.open(bundle, entries: Transcript.pairingCopy,
                        session: Sessions.claude(gate: .needsYou, asks: [Sessions.claudePermission]))
        },
        // The box with a message half-written in it, at the same size: the
        // composer is the one surface that grows under the reader's thumb, so
        // it is photographed separately from the conversation behind it.
        Fixture(id: "composer-accessibility", screen: .typing, typeSize: "accessibility5") { bundle in
            States.open(bundle, entries: Transcript.pairingCopy, session: Sessions.claude())
            bundle.conversation(Scenario.focus).draft.body =
                "Check the reconnect path before you squash it."
        },
    ]

    private static let refusal: OpFailure = {
        let json = Data("""
            {"error":"general","message":"the session is replaying history",\
            "auth_required":false,"subscription_required":false}
            """.utf8)
        // The refusal is the core's own sentence, decoded rather than retyped.
        return try! AmuxJSON.decoder.decode(OpFailure.self, from: json)
    }()
}
