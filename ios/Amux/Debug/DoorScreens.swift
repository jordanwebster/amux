import AmuxCore
import AmuxDesign
import AmuxFeatures
import AmuxShell
import SwiftUI

/// Which screens the door can show, and how each one is built from the stores
/// a fixture filled.
///
/// A screen that is not here is not silently something else: opening it is a
/// typed refusal, so a golden run over the whole catalogue names every screen
/// still to be built instead of passing on a placeholder. Which states are
/// drawn is `Fixtures.isBuilt`, which the door asks before it ever gets here —
/// one screen can draw several states, and they are built one at a time.
enum DoorScreens {
    /// Which overlay a screen name means. A screen is not always one picture:
    /// the permissions sheet and the model sheet are both `settings`, opened
    /// on different rows, so the state a capture asks for says which.
    static func overlay(for screen: Screen) -> ConversationOverlay? {
        switch screen {
        case .plus: .plus
        case .settings: .settings
        case .overflow: .overflow
        case .agentDelete: .deleteAgent
        default: nil
        }
    }

    @MainActor
    @ViewBuilder
    static func view(for screen: Screen, host: DoorHost) -> some View {
        switch screen {
        case .probe: ProbeScreen()
        // The gated states are the same screen: what is empty and why is an
        // account fact the screen already reads, not a screen of its own.
        case .home, .homeQuiet, .firstRun, .firstRunPaid:
            AgentsHome(model: host.stores.fleet, accounts: host.accounts) { _ in }
        // The switcher is the home with its account list out. Drawn over the
        // real screen rather than on bare ground, because what it covers and
        // how the list behind it dims are facts about the screen underneath.
        case .profiles:
            AgentsHome(
                model: host.stores.fleet, accounts: host.accounts,
                accountsOpen: true) { _ in }
        // You. The accounts this phone knows, what the one on screen has, and
        // what belongs to the phone rather than to any account.
        case .you:
            YouScreen(
                // Nothing, meaning whatever the phone is set to. The
                // appearance the door is holding is the instrument taking the
                // photograph, not a choice anybody made on this screen, and
                // marking it as chosen would say the person picked the one the
                // capture happens to be in.
                accounts: host.accounts, appearance: nil,
                // This phone's own key, read off the machine store the way
                // the devices page reads it, so the row names the same
                // identity the machines were paired with.
                identity: host.stores.hosts.roster.map {
                    Fingerprint.short($0.identity.fingerprint)
                },
                debugTools: true) { _ in }
        // Giving up an account, over the page it was asked from. The You page
        // behind it is the real one, filled from the same accounts, because
        // how the page dims and how much of it the card covers are facts about
        // both at once — a card photographed on bare ground would be a picture
        // of neither.
        case .delete:
            DeleteAccountOverlay(
                entry: host.accounts.selectedAccount, model: host.deletion,
                actions: { _ in }
            ) {
                YouScreen(
                    accounts: host.accounts, appearance: nil,
                    identity: host.stores.hosts.roster.map {
                        Fingerprint.short($0.identity.fingerprint)
                    },
                    debugTools: true) { _ in }
            }
        // The drawer is drawn over the screen it was opened from, which is a
        // conversation. It is the real one, filled from the same state, rather
        // than a stand-in: what the panel dims, what its edge uncovers and how
        // far its shadow reaches are all facts about the screen underneath, and
        // a baseline photographed over bare ground would be a picture of none
        // of them.
        case .drawer:
            DrawerOverlay(
                open: .constant(true),
                drawer: AgentsDrawer(
                    model: host.stores.fleet, hosts: host.stores.hosts,
                    current: Scenario.focus) { _ in }
            ) {
                Conversation(
                    model: host.stores.conversation(Scenario.focus),
                    subject: ConversationSubject(
                        agent: Scenario.focus, in: host.stores.fleet)) { _ in }
            }
        // One screen, eight names. Whether a turn is still running, who else
        // has spoken in it, whether a message is waiting to go, whether the
        // layer will take one and whether the run has ended are all facts the
        // conversation reads off its own store rather than screens of their
        // own: `run-live`, `voices`, `review-cta`, `working`, `queued`,
        // `typing`, `exited` and `offline` are `run` with a different feed,
        // session, draft and fleet in it.
        //
        // The overlay is handed in here as well, because the strip above the
        // composer grows into the task list and a capture of the grown strip
        // has to be able to ask for it open.
        case .run, .runLive, .voices, .reviewCta, .working, .queued, .exited, .typing, .offline:
            Conversation(
                model: host.stores.conversation(Scenario.focus),
                subject: ConversationSubject(
                    agent: Scenario.focus, in: host.stores.fleet),
                naming: { host.stores.fleet.name(of: $0) },
                showing: host.overlay) { _ in }
        // The plus, opened. Which overlay a conversation is showing is a state
        // of the conversation and is handed in, the way the drawer's own
        // openness is, so what is photographed is the real screen with the
        // real card over it rather than the card on bare ground.
        // Typing a command raises rows over the box from the draft the
        // fixture wrote, so this is the ordinary conversation and nothing is
        // handed in: what is photographed is what somebody typing would see.
        case .plus, .settings, .overflow, .agentDelete, .slashTyping:
            let agent = host.stores.conversations.keys.contains(Scenario.focus)
                ? Scenario.focus : Scenario.agentId("spec-suite")
            Conversation(
                model: host.stores.conversation(agent),
                subject: ConversationSubject(agent: agent, in: host.stores.fleet),
                showing: host.overlay ?? DoorScreens.overlay(for: screen)) { _ in }
        // An ask is a state of the conversation rather than a screen beside
        // it: what replaces the composer is read off the session's own asks,
        // so permission, question and plan are one screen with a different
        // thing waiting on it. The Codex approval opens on a Codex agent,
        // which is why the agent is not always the same one.
        case .askPermission, .askQuestion, .plan:
            let agent = host.stores.conversations.keys.contains(Scenario.focus)
                ? Scenario.focus : Scenario.agentId("spec-suite")
            Conversation(
                model: host.stores.conversation(agent),
                subject: ConversationSubject(agent: agent, in: host.stores.fleet)) { _ in }
        // The review, and the review with a comment being written on it. One
        // page: what is selected and what is in the draft are the review's own
        // state, so the sheet is a state of this screen rather than a screen
        // beside it.
        case .diff, .comment:
            if let review = host.stores.review(Scenario.focus) {
                DiffPage(model: review, subject: "refactor-auth") { _ in }
            }
        // The machines. Captured outside the tab bar like every other screen,
        // because what the tab bar looks like is the shell's own journey.
        case .hosts:
            HostsTab(model: host.stores.hosts) { _ in }
        // The two pairing screens. Both are functions of the one pairing
        // attempt the account's stores hold, so a capture opens the screen and
        // the fixture decides which moment of the attempt it is.
        case .pin:
            PairByCode(model: host.stores.pairing) { _ in }
        case .pairConfirm:
            PairConfirmation(model: host.stores.pairing) { _ in }
        // Signing in. What the screen shows is where the one attempt this
        // phone has in flight stands — nothing yet, the browser up, refused,
        // or done — so the state decides the picture and this is the one arm
        // for all four.
        case .signIn:
            SignIn(model: host.signIn) { _ in }
        // Subscribing. What is on offer, what is chosen and how a purchase
        // went are all facts of the one attempt this phone has in flight, so
        // the state decides the picture and this is the one arm for all of
        // them — including the account that already pays.
        case .paywall:
            Paywall(model: host.paywall) { _ in }
        // Something looked wrong and the phone was photographed. The offer is
        // drawn over the real conversation rather than over bare ground,
        // because what is being reported is that screen: where the pill sits
        // relative to the composer, and what of the transcript it covers, are
        // facts about both at once.
        //
        // The system's own screenshot preview is not here and cannot be. It
        // belongs to the system, appears only after a real screenshot, and is
        // outside this app's window — which is the same reason the report's
        // frozen frame has no status bar in it.
        case .shake:
            Conversation(
                model: host.stores.conversation(Scenario.focus),
                subject: ConversationSubject(
                    agent: Scenario.focus, in: host.stores.fleet),
                naming: { host.stores.fleet.name(of: $0) }) { _ in }
                .reportOffer(true, take: {}, dismiss: {})
        // The report, on the frame a screenshot froze. Captured on its own
        // rather than over the screen it is about: the frame is inside the
        // report as a photograph, so the screen underneath is already in the
        // picture and drawing it twice would say something untrue about what
        // this screen covers.
        case .dump:
            ReportScreen(model: host.reports) { _ in }
        // Starting an agent. The chooser over it is a state of this screen
        // rather than a screen beside it, so the fixture decides whether it is
        // open and this is the one arm either way.
        case .newAgent:
            NewAgent(model: host.stores.newAgent, hosts: host.stores.hosts) { _ in }
        }
    }
}

/// The root a debug build draws: whatever the door has been asked to show,
/// under the appearance and type size it was asked for, and the app itself
/// when nothing has asked for anything.
struct DrivenRoot<Content: View>: View {
    @State private var host = DoorHost.shared
    private let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        Group {
            if let replayed = host.replayed {
                // The app itself, from a recording. Drawn before the
                // catalogue is asked anything, because a replay is not a
                // picture of one screen: it is the shell with the rebuilt
                // stores in it, and the tab bar under the page is part of
                // what the report was of.
                Shell(
                    router: replayed.router, accounts: replayed.accounts,
                    stores: replayed.stores, signIn: host.signIn,
                    paywall: host.paywall, deletion: host.deletion,
                    appearance: host.appearance, report: nil,
                    recording: replayed.recording,
                    actions: { _ in })
                    .preferredColorScheme(host.appearance == .dark ? .dark : .light)
            } else if let screen = host.screen {
                DoorScreens.view(for: screen, host: host)
                    // Opening a fixture replaces its stores and must also
                    // discard view-local state, such as the previous
                    // conversation's open panel, even for the same screen.
                    .id(ObjectIdentifier(host.stores))
                    // The hosting controller owns system status-bar style.
                    // Give it the same preference as the window's traits;
                    // overriding only the window can leave white status text
                    // on the next light fixture.
                    .preferredColorScheme(host.appearance == .dark ? .dark : .light)
            } else {
                content
            }
        }
        .environment(\.design, host.design)
        // A screen the door is showing is being photographed, not used: what
        // blinks on a timer of its own draws its resting state so two runs
        // take the same picture.
        .environment(\.photographed, host.screen != nil || host.replayed != nil)
        // Built afresh on every appearance request rather than moved into the
        // new one: a material already on screen cross-fades over a length of
        // time nobody publishes, and a still of that fade is a picture of
        // neither appearance.
        .id(host.appearances)
        .dynamicTypeSize(host.typeSize)
        // A state may turn the assistive settings on and never off: the
        // device's own answer is already in the environment by the time this
        // runs, and a driven screen has no business telling a reader who asked
        // for less motion that they did not.
        .transformEnvironment(\.reducesMotion) { $0 = $0 || host.reduceMotion }
        .transformEnvironment(\.reducesTransparency) { $0 = $0 || host.reduceTransparency }
        .onPreferenceChange(IdentifiedElements.self) { declared in
            Task { @MainActor in DoorHost.shared.declared = declared }
        }
    }
}
