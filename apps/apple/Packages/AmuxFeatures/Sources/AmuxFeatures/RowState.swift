import AmuxCore
import AmuxDesign
import SwiftUI

/// Everything a fleet row can say about itself, in one closed set.
///
/// It exists because the answer used to be spread across four places that
/// nothing reconciled: the agent's attention, whether this build can read its
/// provider, whether the machine that owns it has answered, and whether that
/// machine is online at all. The list row and the sentence VoiceOver reads
/// each recombined a different subset by hand, so no single
/// place said what a row could be — which is how two marks in the vocabulary
/// came to mean nothing without anybody noticing.
///
/// The marks are gone. A row draws one thing, the accent dot beside its age,
/// for the one state that is waiting on a person; everything else is said in
/// words on the third line, where `Idle` and `Finished · 4 files · +118 −40`
/// already were.
/// A word is more precise than a glyph and needs no vocabulary learnt first.
enum RowState: Equatable {
    /// Stopped and cannot continue without you. The only case with a mark.
    case needsYou
    case working
    case starting
    /// The machine that owns this agent is not answering.
    case hostOffline(String)
    /// Listed, and not reachable from here: the row is what this phone last
    /// held, and it stays on the list.
    case hostAway(String)
    /// A kind this build has no chat for.
    case unsupported
    /// The agent's process ended, and why where it said.
    case exited(String?)
    case idle

    /// Read once, in one order, so the order is a thing somebody can look at.
    ///
    /// Being unreadable comes first because it is a fact about this build
    /// rather than about the agent, and it outranks anything the agent might
    /// be doing — none of which can be acted on from here anyway. A dark
    /// machine comes next, and above `needsYou` deliberately: what an agent
    /// last asked for on a machine nobody can reach cannot be answered.
    init(row: AgentRow, host: HostView? = nil) {
        if !row.readable {
            self = .unsupported
            return
        }
        let machine = PlaceNames.host(host?.name ?? row.hostName)
        let reach = host?.reach
        if row.hostPresence == .offline || reach == .offline {
            self = .hostOffline(machine)
            return
        }
        if row.hostPresence == .away || reach == .away {
            self = .hostAway(machine)
            return
        }
        switch row.attention {
        case .needsYou: self = .needsYou
        case .working: self = .working
        case .starting: self = .starting
        case .exited: self = .exited(row.card.exitCause)
        case .idle: self = .idle
        }
    }

    /// The word the third line opens with. Needing you has none: the accent
    /// line under it says what for.
    var word: String? {
        switch self {
        case .needsYou: nil
        case .working: "Working"
        case .starting: "Starting"
        case .hostOffline(let machine): "\(machine) offline"
        case .hostAway(let machine): "\(machine) away"
        case .unsupported: "Unknown agent"
        case .exited: "Exited"
        case .idle: "Idle"
        }
    }

    /// What follows the word, where it says more than the place.
    var elaboration: String? {
        switch self {
        case .unsupported: "update amux to open it"
        case .hostAway: "not live"
        case .exited(let cause): cause.flatMap { $0.isEmpty ? nil : $0 }
        default: nil
        }
    }

    /// Whether the word already names the machine.
    var namesTheHost: Bool {
        switch self {
        case .hostOffline, .hostAway: true
        default: false
        }
    }

    var needsYou: Bool { self == .needsYou }

    /// The same, as VoiceOver says it.
    var spoken: String? {
        switch self {
        case .needsYou: "Needs you"
        case .working: "Working"
        case .starting: "Starting"
        case .hostOffline(let machine): "\(machine) is offline"
        case .hostAway(let machine): "\(machine) is away, not live"
        case .unsupported: "Unknown agent, update amux to open it"
        case .exited(let cause): ["Exited", cause].compactMap { $0 }.joined(separator: ", ")
        case .idle: "Idle"
        }
    }

    /// What a driver reads the row's state as.
    var name: String {
        switch self {
        case .needsYou: "needs-you"
        case .working: "working"
        case .starting: "starting"
        case .hostOffline: "host-offline"
        case .hostAway: "host-away"
        case .unsupported: "unsupported"
        case .exited: "exited"
        case .idle: "idle"
        }
    }
}
