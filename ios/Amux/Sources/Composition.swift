import AmuxCore
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
    let router = Router()
    /// The one sign-in this phone has in flight. It outlives the page that
    /// shows it, so a person who leaves the screen while the browser is up
    /// comes back to the attempt rather than to a fresh one.
    let signIn = SignInStore()
    /// What the App Store has to sell and how a purchase went. One per app:
    /// the store sells to an Apple Account, not to an amux one.
    let paywall = PaywallStore()
    /// The real account service. Every screen sees it as `CloudService` and
    /// none of them knows there is HTTP behind it.
    private let cloud: any CloudService = AmuxCloudService()
    /// The real App Store. The paywall sees it as `StoreFront` and does not
    /// know StoreKit is behind it.
    private let store: any StoreFront = AppStoreFront()

    /// Where a fleet goes before anybody has signed in. The app runs signed
    /// out — it shows an empty home rather than a login wall — so there has to
    /// be somewhere for a cache to land that is not an account's.
    private let signedOut = StoreBundle(account: AccountId("signed-out"))

    var stores: StoreBundle { accounts.stores ?? signedOut }

    init() {
        router.loads(with: self)
        rememberedFleet()
    }

    /// Puts the fleet this phone saw last time on screen before anything has
    /// been reached.
    ///
    /// Read straight off disk by the shared library rather than by starting the
    /// runtime first: a launch has rows to draw long before it has a network,
    /// and a person opening the app to check on an agent should not watch an
    /// empty screen while a connection is negotiated. Every row arrives marked
    /// as remembered, and each one goes solid when the machine that owns it
    /// answers.
    private func rememberedFleet() {
        stores.apply(Bridge.cachedFleet(in: AppFiles.cache))
    }

    /// What the shell asks for that it cannot do itself.
    func handle(_ action: ShellAction) {
        switch action {
        case .selectAccount(let id):
            accounts.select(id)
        // Signing in is a page, pushed onto whichever stack asked for it so
        // going back leads where the person came from.
        case .signIn:
            router.open(.signIn(router.tab))
        // The hand-off itself. It leaves for a browser this app cannot read
        // and comes back with an account or with what went wrong; the store
        // holds which, and the screen draws it.
        case .handOffSignIn:
            Task { await signIn.signIn(with: cloud, presenting: WebSignIn(), into: accounts) }
        // Subscribing is a page too, and it asks the store what it has on the
        // way: the screen is useful before the answer arrives and nothing
        // waits on it.
        case .subscribe:
            paywall.entitled(accounts.selectedAccount?.entitlement ?? .none)
            router.open(.paywall(router.tab))
            Task { await paywall.load(from: store) }
        case .buySubscription:
            Task {
                guard await paywall.buy(store) == .bought else { return }
                // The store says this Apple Account paid; the account service
                // is where the subscription actually lives, so what this
                // account may do is read back from it rather than assumed.
                await refreshEntitlement()
            }
        case .restorePurchases:
            Task {
                guard await paywall.restore(store) == .bought else { return }
                await refreshEntitlement()
            }
        // Adding an account leaves the app for the web. Until that journey is
        // built there is nowhere to send somebody, and inventing a local one
        // the real one would have to undo would be worse than the button
        // doing nothing.
        case .addAccount:
            break
        }
    }
    /// Reads what the account service says this account may do, after
    /// something changed what that is.
    ///
    /// The App Store's receipt reaches the account service through its own
    /// webhook, so this is asked of the cloud rather than worked out from the
    /// purchase: a subscription bought on the web through the CLI has to be
    /// honoured by exactly the same read.
    private func refreshEntitlement() async {
        guard let id = accounts.selected else { return }
        guard let entitlement = try? await cloud.entitlement(id) else { return }
        guard let accepted = accounts.accept(entitlement, for: id) else { return }
        accounts.entitlement(accepted, for: id)
        paywall.entitled(accepted)
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
        case .newAgent, .pairByCode, .pairConfirmation, .host, .signIn, .paywall,
             .accounts, .appearance, .help:
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
             .paywall, .accounts, .appearance, .help:
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

    private static func directory(_ search: FileManager.SearchPathDirectory) -> URL {
        let manager = FileManager.default
        let root = manager.urls(for: search, in: .userDomainMask)[0]
            .appendingPathComponent("amux", isDirectory: true)
        try? manager.createDirectory(at: root, withIntermediateDirectories: true)
        return root
    }
}
