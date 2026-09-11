import AmuxCore
import AmuxDesign
import Foundation

/// A place in the app, as a recording of what somebody was looking at names
/// it.
///
/// The app's own vocabulary rather than the screen catalogue's: the catalogue
/// names pictures a capture run can take, and most of them are states of a
/// screen rather than somewhere a person can be. A recording has to say where
/// the person was — which tab, which conversation, whose changes — because
/// that is what a replay has to put them back into, inside the shell they were
/// in rather than on a screen photographed on its own.
///
/// `screen` is the exception and is the catalogue's: a capture run drives the
/// app to pictures that are not places, and a report taken during one says so
/// in the catalogue's words.
public enum Place: Sendable, Equatable {
    /// The Agents tab, showing its own root.
    case home
    /// The Hosts tab, showing its own root.
    case hosts
    /// The You tab, showing its own root: this phone's settings.
    case settings
    case conversation(AgentId)
    /// The changes one agent has made, read as a review.
    case review(AgentId)
    /// A picture from the screen catalogue, for a report taken while a capture
    /// run was driving the app.
    case screen(String)

    /// What this place is called where one is named in a line of prose or a
    /// line of a replay's answer.
    public var described: String {
        switch self {
        case .home: "home"
        case .hosts: "hosts"
        case .settings: "settings"
        case .conversation(let agent): "conversation \(agent)"
        case .review(let agent): "review \(agent)"
        case .screen(let name): name
        }
    }
}

/// What the person using the app did to the view, recorded beside the shared
/// runtime's own messages.
///
/// A report holds two recordings that answer different questions. The
/// runtime's `msgs.jsonl` says what the fleet and its conversations were; this
/// says what was being looked at — which place, which sheet, how far down a
/// transcript, in which appearance and at which reader's type size, whose
/// account was signed in and what the clock on it read. Neither alone
/// reproduces the screen somebody was complaining about, so replay folds the
/// first and then applies the second.
///
/// The composer's draft is deliberately absent. The plan names a `draft` case,
/// but the draft type it carries belongs to the composer, which is not built
/// yet; a case carrying a stand-in would freeze the wrong shape into recorded
/// bundles. It is added when the composer lands.
public enum TraceEvent: Sendable, Equatable {
    /// Where the person went. Written as they navigate, so a recording is the
    /// trail through the app rather than a single destination.
    case route(Place)
    /// The sheet over that place, or nothing when it was dismissed.
    case sheet(String?)
    /// How far down one agent's transcript the reader had scrolled, in points.
    case scroll(AgentId, Double)
    case appearance(Appearance)
    /// The reader's type size, spelled the way a door request spells it.
    case dynamicType(String)
    /// When the screen was frozen, and when the fleet on it was last put in
    /// order.
    ///
    /// Two instants because they answer different questions and are rarely the
    /// same. `at` is when the report happened, which is what dates it. Every
    /// "11s ago" on the screen was measured from `ordered`, which is when the
    /// list was last arranged — so a replay that read the clock from `at`
    /// would rebuild the same rows with every age on them wrong.
    case frozen(at: Date, ordered: Date)
    /// Who was signed in and what they could reach, or nothing when nobody
    /// was.
    ///
    /// The screen says whether this phone can reach anything at all, and
    /// whether the account has a subscription behind it, so a replay without
    /// this rebuilds somebody else's home. No credential of any kind is in
    /// it: the entry is the address, the name, whether the session is still
    /// good and what the account service last said the account may do.
    case account(AccountEntry?)
}

extension TraceEvent: Codable {
    private enum Key: String, CodingKey {
        case kind, place, screen, agent, sheet, offset, appearance, size, at, ordered, account
    }

    public init(from decoder: any Decoder) throws {
        let fields = try decoder.container(keyedBy: Key.self)
        let kind = try fields.decode(String.self, forKey: .kind)
        switch kind {
        case "route": self = .route(try Self.place(from: fields))
        case "sheet": self = .sheet(try fields.decodeIfPresent(String.self, forKey: .sheet))
        case "scroll":
            self = .scroll(
                try fields.decode(AgentId.self, forKey: .agent),
                try fields.decode(Double.self, forKey: .offset))
        case "appearance":
            self = .appearance(try fields.decode(Appearance.self, forKey: .appearance))
        case "dynamicType": self = .dynamicType(try fields.decode(String.self, forKey: .size))
        case "frozen":
            self = .frozen(
                at: try fields.decode(Date.self, forKey: .at),
                ordered: try fields.decode(Date.self, forKey: .ordered))
        case "account":
            self = .account(try fields.decodeIfPresent(AccountEntry.self, forKey: .account))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: fields, debugDescription: "no trace event named \(kind)")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: Key.self)
        switch self {
        case .route(let place):
            try fields.encode("route", forKey: .kind)
            try Self.encode(place, into: &fields)
        case .sheet(let sheet):
            try fields.encode("sheet", forKey: .kind)
            try fields.encodeIfPresent(sheet, forKey: .sheet)
        case .scroll(let agent, let offset):
            try fields.encode("scroll", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(offset, forKey: .offset)
        case .appearance(let appearance):
            try fields.encode("appearance", forKey: .kind)
            try fields.encode(appearance, forKey: .appearance)
        case .dynamicType(let size):
            try fields.encode("dynamicType", forKey: .kind)
            try fields.encode(size, forKey: .size)
        case .frozen(let at, let ordered):
            try fields.encode("frozen", forKey: .kind)
            try fields.encode(at, forKey: .at)
            try fields.encode(ordered, forKey: .ordered)
        case .account(let entry):
            try fields.encode("account", forKey: .kind)
            try fields.encodeIfPresent(entry, forKey: .account)
        }
    }

    /// A place is spelled as the kind of place it is and, where the kind needs
    /// one, the thing it is a place about. An agent id rather than a name:
    /// names are the fleet's to change and the recording has to survive one.
    private static func place(from fields: KeyedDecodingContainer<Key>) throws -> Place {
        let named = try fields.decode(String.self, forKey: .place)
        switch named {
        case "home": return .home
        case "hosts": return .hosts
        case "settings": return .settings
        case "conversation": return .conversation(try fields.decode(AgentId.self, forKey: .agent))
        case "review": return .review(try fields.decode(AgentId.self, forKey: .agent))
        case "screen": return .screen(try fields.decode(String.self, forKey: .screen))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .place, in: fields, debugDescription: "no place named \(named)")
        }
    }

    private static func encode(_ place: Place, into fields: inout KeyedEncodingContainer<Key>) throws {
        switch place {
        case .home: try fields.encode("home", forKey: .place)
        case .hosts: try fields.encode("hosts", forKey: .place)
        case .settings: try fields.encode("settings", forKey: .place)
        case .conversation(let agent):
            try fields.encode("conversation", forKey: .place)
            try fields.encode(agent, forKey: .agent)
        case .review(let agent):
            try fields.encode("review", forKey: .place)
            try fields.encode(agent, forKey: .agent)
        case .screen(let name):
            try fields.encode("screen", forKey: .place)
            try fields.encode(name, forKey: .screen)
        }
    }
}

/// The parts of a report bundle this side of the flight reads and writes, and
/// how the view-state trace is spelled on disk.
///
/// One JSON object per line, like the runtime's own recording beside it, so a
/// truncated bundle loses its last event rather than all of them and a person
/// can read either file with the same eyes.
public enum Trace {
    /// The shared runtime's recording: a checkpoint header, then its messages.
    public static let messagesFile = "msgs.jsonl"
    /// The view-state recording written beside it.
    public static let traceFile = "trace.jsonl"
    /// Instants are written the way every other timestamp that leaves this app
    /// is written — RFC 3339, to the fraction — so a person reading the file
    /// can see when the report was frozen without decoding anything.
    public static func lines(_ events: [TraceEvent]) throws -> String {
        let encoder = AmuxJSON.encoder
        encoder.outputFormatting = [.sortedKeys]
        return try events
            .map { String(decoding: try encoder.encode($0), as: UTF8.self) }
            .joined(separator: "\n") + "\n"
    }

    public static func events(_ lines: String) throws -> [TraceEvent] {
        let decoder = AmuxJSON.decoder
        return try lines
            .split(separator: "\n")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
            .map { try decoder.decode(TraceEvent.self, from: Data($0.utf8)) }
    }
}
