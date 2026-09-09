import AmuxCore
import AmuxDesign
import AmuxFeatures
import AmuxShell
import Foundation
import Observation

/// Everything the app is made of, assembled in one place.
///
/// The screens know their stores, the shell knows where things lead, and this
/// knows which stores and which shell — so switching account swaps a set of
/// stores here and nothing above has to be told about accounts at all.
@MainActor
@Observable
final class Composition {
    let accounts = AccountRegistry()
    let router: Router
    /// The one sign-in this phone has in flight. It outlives the page that
    /// shows it, so a person who leaves the screen while the browser is up
    /// comes back to the attempt rather than to a fresh one.
    let signIn = SignInStore()
    /// What the App Store has to sell and how a purchase went. One per app:
    /// the store sells to an Apple Account, not to an amux one.
    let paywall = PaywallStore()
    /// The account somebody is in the middle of giving up. It outlives the
    /// question it is asked over: a deletion the billing system refuses sends
    /// the person out of the app to cancel a renewal, and coming back finds
    /// the same question with the same address typed.
    let deletion = DeletionStore()
    /// What the app is wearing. Nothing means whatever the phone is set to,
    /// which is what most people want and what the app starts as.
    var appearance: Appearance?
    #if AMUX_DEBUG_TOOLS
    let reports: ReportStore?
    let freezer: (any ReportFreezing)?
    #endif
    /// The account service. Every screen sees it as `CloudService` and none of
    /// them knows there is HTTP behind it. A debug build's driving door is
    /// handed the same one, so a launch driven against the real service is
    /// driven against the service the app itself is using.
    let cloud: any CloudService
    /// The App Store. The paywall sees it as `StoreFront` and does not know
    /// StoreKit is behind it.
    private let store: any StoreFront
    /// Where signing in happens: the system's own browser, which this app
    /// hands a URL and is told what came back from.
    private let webAuth: any WebAuthPresenter

    /// Where a fleet goes before anybody has signed in. The app runs signed
    /// out — it shows an empty home rather than a login wall — so there has to
    /// be somewhere for a cache to land that is not an account's.
    private let signedOut = StoreBundle(account: AccountId("signed-out"))

    var stores: StoreBundle { accounts.stores ?? signedOut }

    init() {
        // Held locally as well as stored, so what freezes a report can be
        // given the router without reaching back through an object that is
        // still being built.
        let router = Router()
        self.router = router
        // The real services, unless the launch says otherwise. A launch driven
        // by a test says otherwise: signing in must not open a browser at
        // amux.sh, buying must not reach the App Store, and deleting must not
        // delete anybody's account. The doubles are the same shape, so
        // everything above this line is the app either way.
        #if AMUX_DEBUG_TOOLS
        let scripted = ProcessInfo.processInfo.arguments
            .contains("-\(Door.scriptedCloudArgument)")
        cloud = scripted ? DoorHost.shared.cloud : AmuxCloudService()
        store = scripted ? DoorHost.shared.store : AppStoreFront()
        webAuth = scripted ? DoorHost.shared.webAuth : WebSignIn()
        #else
        cloud = AmuxCloudService()
        store = AppStoreFront()
        webAuth = WebSignIn()
        #endif
        #if AMUX_DEBUG_TOOLS
        reports = ReportStore()
        // The page the person is on goes into the report, so whoever opens the
        // bundle knows what they are looking at before they open the picture —
        // and so a picture taken on one page and written up on another says
        // which one it is of.
        freezer = ReportFreeze(
            route: { router.top?.name ?? router.tab.rawValue },
            screen: { Self.catalogueName(for: router) })
        #endif
        router.loads(with: self)
        rememberedFleet()
        settleOutstandingPurchases()
    }

    /// What the screen catalogue calls the page on show, where it has a name
    /// for it.
    ///
    /// A report's view-state recording is replayed against the catalogue, so
    /// this is the vocabulary that decides whether a bundle can be put back on
    /// the page its picture was taken on. Most pages are named the same in
    /// both, and a tab with nothing pushed on it is the screen at its root.
    /// A conversation, an agent's changes and one host have no catalogue name
    /// yet; a report taken there says so rather than naming a screen that
    /// would come back as the wrong thing.
    private static func catalogueName(for router: Router) -> String? {
        if let top = router.top { return Screen(rawValue: top.name)?.rawValue }
        return switch router.tab {
        case .agents: Screen.home.rawValue
        case .hosts: Screen.hosts.rawValue
        case .you: Screen.you.rawValue
        }
    }

    /// Puts the fleet the account on screen saw last time in front of it,
    /// before anything has been reached.
    ///
    /// Read straight off disk by the shared library rather than by starting the
    /// runtime first: a launch has rows to draw long before it has a network,
    /// and a person opening the app to check on an agent should not watch an
    /// empty screen while a connection is negotiated. Every row arrives marked
    /// as remembered, and each one goes solid when the machine that owns it
    /// answers.
    ///
    /// What is remembered belongs to an account, so nobody signed in has
    /// nothing to remember, and changing which account is on screen reads that
    /// account's own rows rather than leaving the last one's up.
    private func rememberedFleet() {
        guard let account = accounts.selected else { return }
        stores.apply(Bridge.cachedFleet(in: AppFiles.cache, for: account))
    }

    /// What the shell asks for that it cannot do itself.
    func handle(_ action: ShellAction) {
        switch action {
        // Changing which account is on screen empties every stack behind it.
        // A page pushed under the account just left is about that account's
        // machines and that account's agents, and coming back to a tab must
        // not find one of them still standing under another account's name.
        case .selectAccount(let id):
            guard accounts.selected != id else { break }
            accounts.select(id)
            rememberedFleet()
            for tab in Tab.allCases { router.setPath([], for: tab) }
        // Signing in is a page, pushed onto whichever stack asked for it so
        // going back leads where the person came from. Adding an account is
        // the same page: this app has no idea who is about to sign in, and
        // whoever comes back is either an account this phone already knows or
        // a new one.
        case .signIn, .addAccount:
            // Opened afresh, it asks afresh. What came back last time was
            // about whoever signed in then, and leaving it on screen would
            // offer somebody a Done button for an account they are not
            // signing in as. A browser still up is the exception: that
            // attempt is this page's and is still running.
            if !signIn.working { signIn.again() }
            router.open(.signIn(router.tab))
        // The hand-off itself. It leaves for a browser this app cannot read
        // and comes back with an account or with what went wrong; the store
        // holds which, and the screen draws it.
        case .handOffSignIn:
            Task { await signIn.signIn(with: cloud, presenting: webAuth, into: accounts) }
        // Subscribing is a page too, and it asks the store what it has on the
        // way: the screen is useful before the answer arrives and nothing
        // waits on it.
        case .subscribe:
            paywall.entitled(accounts.selectedAccount?.entitlement ?? .none)
            router.open(.paywall(router.tab))
            Task { await paywall.load(from: store) }
        case .buySubscription:
            Task {
                guard case .bought(let purchase)? = await paywall.buy(store) else { return }
                await confirm(purchase)
            }
        case .restorePurchases:
            Task {
                guard case .bought(let purchase)? = await paywall.restore(store) else { return }
                await confirm(purchase)
            }
        // A purchase that went through and has not been confirmed, offered to
        // amux.sh again. Where there is nothing left to send — the cloud took
        // it and it was reading the entitlement back that failed — asking
        // again is asking what this account may now do.
        case .retryPurchase:
            Task {
                if let held = paywall.holding {
                    await confirm(held)
                } else if !(await refreshEntitlement()) {
                    paywall.unconfirmed(.unreachable)
                }
            }
        // Leaving an account. It stays listed with Sign In beside it: the
        // address is the one thing a person recognises, and forgetting it
        // would make signing back in look like adding a stranger.
        case .signOutAccount(let id):
            accounts.signOut(id)
        case .wear(let wanted):
            appearance = wanted
        // Giving up an account for good. What it costs is asked first, over
        // the page it was asked from; nothing leaves this phone until the
        // address has been typed and Delete pressed.
        case .deleteAccount(let id):
            deletion.ask(id)
        case .cancelDeletion:
            deletion.dismiss()
        // The account service is what deletes an account, and it refuses while
        // a subscription is still set to renew. Both answers land in the store
        // the question is drawn from, and a deletion that went through takes
        // the account off this phone with it.
        case .confirmDeletion:
            Task { await deletion.delete(with: cloud, from: accounts) }
        }
    }
    /// Reads what the account service says this account may do, after
    /// something changed what that is.
    ///
    /// The App Store's receipt reaches the account service through its own
    /// webhook, so this is asked of the cloud rather than worked out from the
    /// purchase: a subscription bought on the web through the CLI has to be
    /// honoured by exactly the same read.
    @discardableResult
    private func refreshEntitlement() async -> Bool {
        guard let id = accounts.selected else { return false }
        guard let entitlement = try? await cloud.entitlement(id) else { return false }
        guard let accepted = accounts.accept(entitlement, for: id) else { return false }
        accounts.entitlement(accepted, for: id)
        paywall.entitled(accepted)
        return true
    }

    /// Tells amux.sh about a purchase and reads back what this account may now
    /// do.
    ///
    /// Both halves matter. The post is what makes the subscription the
    /// account's; the read is what the screen believes, and it is the same
    /// read a subscription bought on the web arrives through — so a purchase
    /// the cloud took but could not be read back afterwards says so rather
    /// than showing somebody a subscription the home screen cannot use.
    private func confirm(_ purchase: SignedPurchase) async {
        guard let id = accounts.selected else {
            // Nobody to record it against. The purchase is kept and the
            // transaction unfinished, so signing in and opening the app again
            // sends it.
            paywall.unconfirmed(.unreachable)
            return
        }
        guard await paywall.confirm(purchase, with: cloud, as: id, finishing: store) else { return }
        if !(await refreshEntitlement()) { paywall.unconfirmed(.unreachable) }
    }

    /// Sends anything the App Store is still holding, and goes on listening
    /// for purchases that are approved later.
    ///
    /// This is what makes an unconfirmed purchase temporary. A phone that lost
    /// its network mid-purchase, an app killed before the post finished, a
    /// child's purchase a parent approves tomorrow: each one reaches amux.sh
    /// without anybody pressing anything, because the transaction was never
    /// finished with the store.
    private func settleOutstandingPurchases() {
        Task {
            guard let id = accounts.selected else { return }
            if await paywall.confirmOutstanding(in: store, with: cloud, as: id) {
                await refreshEntitlement()
            }
        }
        Task {
            for await purchase in store.approvals() {
                await confirm(purchase)
            }
        }
    }
}

extension Composition: RouteLoader {
    /// Fills a page that is already on screen.
    ///
    /// Opening a conversation opens its store, which is what tells the runtime
    /// this client is watching that agent. The transcript arrives afterwards
    /// and lands in the page the tap already pushed.
    func load(_ route: Route) {
        switch route {
        case .conversation(let agent), .changes(let agent):
            stores.openConversation(agent)
        case .newAgent, .pairByCode, .pairConfirmation, .host, .signIn, .paywall, .accounts:
            break
        }
    }

    /// Leaving a conversation stops the machine streaming it.
    ///
    /// What was read stays read — coming back finds the transcript where it
    /// was left — but a phone that went on streaming every conversation
    /// somebody had ever opened would be reading its machines on behalf of
    /// nobody. The changes page is the same conversation seen differently, so
    /// leaving it for the conversation above it changes nothing.
    func left(_ route: Route) {
        switch route {
        case .conversation(let agent):
            stores.releaseStream(agent)
        case .changes, .newAgent, .pairByCode, .pairConfirmation, .host, .signIn,
             .paywall, .accounts:
            break
        }
    }
}

/// Where this app keeps things between launches.
///
/// The fleet is a cache: losing it costs one launch its remembered rows and
/// nothing else, so it lives where the system is allowed to reclaim it. The
/// shared runtime is handed the same two directories, so what a launch reads
/// and what a connection writes are one file.
enum AppFiles {
    static let support = directory(.applicationSupportDirectory)
    static let cache = directory(.cachesDirectory)

    /// What this build calls itself in a report, the way the terminal's own
    /// reports name theirs: the thing that wrote it, then its version.
    static var build: String {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString")
        return "amux-ios/\(version as? String ?? "0")"
    }

    private static func directory(_ search: FileManager.SearchPathDirectory) -> URL {
        let manager = FileManager.default
        let root = manager.urls(for: search, in: .userDomainMask)[0]
            .appendingPathComponent("amux", isDirectory: true)
        try? manager.createDirectory(at: root, withIntermediateDirectories: true)
        return root
    }
}
