import Foundation
import Observation

/// The account somebody is being asked about taking off this phone.
///
/// Its own store rather than view state, so the question can be declared in a
/// state and photographed, and so it survives the page redrawing under it.
/// There is nothing to wait for: removing an account asks amux.sh nothing, so
/// the question is either on screen or it is not.
@MainActor
@Observable
public final class RemovalStore {
    /// Who is being asked about, or nothing when nobody is.
    public private(set) var account: AccountId?

    public init(asking account: AccountId? = nil) {
        self.account = account
    }

    public func ask(_ id: AccountId) {
        account = id
    }

    public func dismiss() {
        account = nil
    }
}
