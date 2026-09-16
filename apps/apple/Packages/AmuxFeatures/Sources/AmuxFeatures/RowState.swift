import AmuxCore
import AmuxDesign
import SwiftUI

/// Everything a fleet row can say about itself, in one closed set.
///
/// It exists because the answer used to be spread across four places that
/// nothing reconciled: the agent's attention, whether this build can read its
/// provider, whether the machine that owns it has answered, and whether that
/// machine is online at all. The list row, the drawer row and the sentence
/// VoiceOver reads each recombined a different subset by hand, so no single
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
    case needsYou(Why)
    case working
    /// The machine that owns this agent is not answering.
    case hostOffline(String)
    /// The relay can see the machine that owns this agent and will not carry
    /// anything to it on this account. The row is the cache, and it stays on
    /// the list: the agent exists, it is simply not being watched.
    case hostAway(String)
    /// Amux no longer trusts its own guess that a turn is running, and says
    /// nothing rather than guessing again. See `word`.
    case unheard
    /// Run by a provider this build has no case for, under the name the host
    /// used for it.
    case unsupported(String)
    case finished(TurnOutcome?)
    case idle

    /// Read once, in one order, so the order is a thing somebody can look at.
    ///
    /// Being unreadable comes first because it is a fact about this build
    /// rather than about the agent, and it outranks anything the agent might
    /// be doing — none of which can be acted on from here anyway. A dark
    /// machine comes next, and above `needsYou` deliberately: the core has
    /// already made that call inside `effective_attention`, which degrades an
    /// offline host's agents to `unknown` whatever they last wanted. Reading
    /// the host directly rather than only through the attention is what also
    /// catches a remembered row, which keeps the attention it was cached with.
    init(row: AgentRow, host: HostEntry?, reach: HostReach? = nil) {
        if case .unknown(let provider) = row.card.agent.kind {
            self = .unsupported(provider)
            return
        }
        if let host, !host.online || reach == .offline {
            self = .hostOffline(PlaceNames.host(host.name))
            return
        }
        // Listed and not live. Nothing here is stale — the last thing this
        // phone was told is still the last thing that was true — but nothing
        // is arriving either, and a row that said "Working" about an agent no
        // stream is reaching would be this phone guessing.
        if let host, reach == .away {
            self = .hostAway(PlaceNames.host(host.name))
            return
        }
        switch row.attention {
        case .needsYou(let why):
            self = why == .finished ? .finished(row.outcome) : .needsYou(why)
        case .unknown:
            self = .unheard
        case .working:
            self = .working
        case .idle:
            self = .idle
        }
    }

    /// The word the third line leads with, or nothing where the row is better
    /// off saying nothing.
    ///
    /// `unheard` is the deliberate silence. All the core knows by then is that
    /// its own inference has expired: a Claude turn said it was working and no
    /// dated transcript row or prompt dispatch has been seen for ten minutes.
    /// The agent could be part-way through a long build and not writing rows,
    /// could have finished with the delivery lost, could be wedged, could be
    /// fine behind a wedged stream. Nothing here distinguishes those, so any
    /// word would be a diagnosis this app cannot support — and `Working` is
    /// doubly wrong, because the core deliberately stopped asserting it. The
    /// row already carries the honest fact: the age in its top corner.
    ///
    /// `needsYou` says nothing either: its row says what is wanted instead.
    var word: String? {
        switch self {
        case .needsYou, .unheard: nil
        case .working: "Working"
        case .hostOffline(let machine): "\(machine) offline"
        case .hostAway(let machine): "\(machine) away"
        case .unsupported(let provider): provider
        case .finished: "Finished"
        case .idle: "Idle"
        }
    }

    /// What follows the word, where the state has something of its own to say
    /// rather than leaving the row to fall back on where the agent runs.
    var elaboration: String? {
        switch self {
        // Why it cannot be opened, and what to do about it. The old wording
        // said "Cannot be read", which named this app's own limitation and
        // left a reader with nothing to do and no idea what the agent was.
        case .unsupported: "update amux to open it"
        // The one thing a reader needs from this row: what it says happened
        // is remembered rather than watched.
        case .hostAway: "not live"
        // Whatever the turn changed. An agent that has gone quiet since
        // finishing changed exactly what it changed, and the numbers are the
        // readable part; an absent count is not a zero and is never drawn as
        // one.
        case .finished(let outcome): outcome?.arithmetic
        default: nil
        }
    }

    /// Whether the word has already named the machine, so the row does not
    /// print it again on its trailing edge.
    var namesTheHost: Bool {
        switch self {
        case .hostOffline, .hostAway: true
        default: false
        }
    }

    /// Whether the row is waiting on a person, which is the one state a row
    /// draws in the accent colour.
    var needsYou: Bool {
        if case .needsYou = self { return true }
        return false
    }

    /// The state said aloud, for a reader who cannot see the row. Nothing
    /// where the row itself says nothing: the age is read out regardless, and
    /// it is the same fact a sighted reader is given.
    var spoken: String? {
        switch self {
        case .needsYou(let why): why.spoken
        case .working: "Working"
        case .hostOffline(let machine): "\(machine) is offline"
        case .hostAway(let machine): "\(machine) is away, not live"
        case .unheard: nil
        case .unsupported(let provider): "\(provider), update amux to open it"
        case .finished: "Finished"
        case .idle: "Idle"
        }
    }

    /// The word a capture and a door query agree on. Kept separate from what
    /// is drawn: a screen's wording is allowed to change without a journey
    /// that drives the screen having to be rewritten.
    var name: String {
        switch self {
        case .needsYou(let why): why.spoken
        case .working: "working"
        case .hostOffline: "host-offline"
        case .hostAway: "host-away"
        case .unheard: "unheard"
        case .unsupported: "unsupported"
        case .finished: "finished"
        case .idle: "idle"
        }
    }
}
