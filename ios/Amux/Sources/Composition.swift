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

    /// Where a fleet goes before anybody has signed in. The app runs signed
    /// out — it shows an empty home rather than a login wall — so there has to
    /// be somewhere for a cache to land that is not an account's.
    private let signedOut = StoreBundle(account: AccountId("signed-out"))

    var stores: StoreBundle { accounts.stores ?? signedOut }

    init() {
        router.loads(with: self)
    }

    /// What the shell asks for that it cannot do itself.
    func handle(_ action: ShellAction) {
        switch action {
        case .selectAccount(let id):
            accounts.select(id)
        // Signing in, adding an account and subscribing all leave the app for
        // the web or the store. Until those journeys are built there is
        // nowhere to send somebody, and inventing a local sign-in that the
        // real one would have to undo would be worse than the button doing
        // nothing.
        case .addAccount, .signIn, .subscribe:
            break
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
            stores.conversation(agent)
        case .newAgent, .pairByCode, .pairConfirmation, .host, .accounts, .appearance, .help:
            break
        }
    }
}
