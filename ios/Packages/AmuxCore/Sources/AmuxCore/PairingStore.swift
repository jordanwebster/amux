import Foundation
import Observation

/// Adding a machine, in the two phases the protocol has.
///
/// The first phase proves a secret and asks the machine who it is; the second
/// writes trust. Nothing here ever collapses them, because the whole reason
/// they are separate is that a person has to see the fingerprint of the key
/// they are about to trust before they trust it — a machine that authenticated
/// is a machine that knew the code, which is not the same as the machine the
/// person meant.
///
/// One attempt at a time. A phone is not a place where two pairings are in
/// flight at once, and holding only one is what lets a screen answer "which
/// attempt is this" without carrying an identifier around.
@MainActor
@Observable
public final class PairingStore {
    /// How far the one attempt has got.
    public enum Phase: Equatable, Sendable {
        /// Taking digits.
        case entering
        /// A secret or an answer is with the machine.
        case checking
        /// Authenticated, waiting for a person. Nothing has been trusted.
        case confirming(PendingPeer)
        /// Trust written, on this phone and on the machine.
        case trusted(String)
        /// It did not work. Which of the four ways is deliberately not said.
        case refused
    }

    /// How many digits a code is. The machine prints exactly this many, so an
    /// entry that has them all is an entry that is ready to send.
    public static let codeLength = 6

    /// How long a machine holds an offer open, as `amux pair` prints it.
    ///
    /// A constant rather than something observed: the offer is made before
    /// this phone is involved at all, so there is nothing to read it off.
    /// It is the protocol's own pairing window, and the screen says it because
    /// a code that has quietly gone stale is otherwise indistinguishable from
    /// a code that was mistyped.
    public static let offerWindow = "5 minutes"

    public private(set) var digits = ""
    public private(set) var phase = Phase.entering
    /// The machine a typed code is authenticated against.
    ///
    /// A six-digit code proves possession of one machine's offer, so there is
    /// always exactly one machine it can be tried against, and the screen that
    /// takes the digits knows which. Absent when the attempt came from a link,
    /// which names its own machine inside the payload.
    public private(set) var machine: HostEntry?
    /// The step this store is waiting on, so somebody else's result is not
    /// mistaken for its own.
    private var awaiting: OpId?
    private let clock: @MainActor () -> Date

    /// The clock this store reads, so a screen that puts a length of time on
    /// an offer asks the store rather than the system and a pinned clock pins
    /// the whole picture.
    public init(clock: @escaping @MainActor () -> Date = { Date() }) {
        self.clock = clock
    }

    public var now: Date { clock() }

    /// Starts an attempt over, for a machine or for a link.
    public func open(machine: HostEntry? = nil) {
        self.machine = machine
        digits = ""
        phase = .entering
        awaiting = nil
    }

    /// Whether this attempt is still taking digits. A code that is with the
    /// machine is not, because sending one twice spends two of the attempts
    /// the machine allows; a refused one is, because typing over a refusal is
    /// how somebody tries the next code.
    public var taking: Bool { phase == .entering || phase == .refused }

    /// The code as it now stands. Anything that is not a digit, and anything
    /// past the sixth, is not part of a code and is dropped. Typing after a
    /// refusal takes the refusal off the screen: the person has moved on.
    public func enter(_ typed: String) {
        guard taking else { return }
        digits = String(typed.filter(\.isNumber).prefix(Self.codeLength))
        if !digits.isEmpty { phase = .entering }
    }

    /// A step has been dispatched and this store is what answers it.
    public func awaits(_ op: OpId?) {
        awaiting = op
        // A step nobody could dispatch — no account, no runtime — is a step
        // that failed, and it fails in the same words every other failure uses.
        phase = op == nil ? .refused : .checking
        if op == nil { digits = "" }
    }

    public func apply(_ event: Event) {
        guard case .opResult(let result) = event, result.op == awaiting else { return }
        awaiting = nil
        switch result.outcome {
        case .pairingPending(let peer):
            phase = .confirming(peer)
        case .paired(_, let name):
            phase = .trusted(name)
        // Turned away by the person: back to an empty entry, because nothing
        // happened and the next thing to do is the same thing again.
        case .pairingAbandoned:
            digits = ""
            phase = .entering
        // Mistyped, expired, already used, never issued, an answer to an
        // attempt this phone no longer holds, or the machine refusing outright.
        // One state, one sentence, and the digits go — a code that failed is
        // never the code to try again.
        default:
            digits = ""
            phase = .refused
        }
    }

}
