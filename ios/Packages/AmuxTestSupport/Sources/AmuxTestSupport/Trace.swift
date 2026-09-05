import AmuxCore
import AmuxDesign
import Foundation

/// What the person using the app did to the view, recorded beside the shared
/// runtime's own messages.
///
/// A report holds two recordings that answer different questions. The
/// runtime's `msgs.jsonl` says what the fleet and its conversations were; this
/// says what was being looked at — which screen, which sheet, how far down a
/// transcript, in which appearance and at which reader's type size. Neither
/// alone reproduces the screen somebody was complaining about, so replay folds
/// the first and then applies the second.
///
/// The composer's draft is deliberately absent. The plan names a `draft` case,
/// but the draft type it carries belongs to the composer, which is not built
/// yet; a case carrying a stand-in would freeze the wrong shape into recorded
/// bundles. It is added when the composer lands.
public enum TraceEvent: Sendable, Equatable {
    /// The screen being shown, by the id the catalogue gives it.
    case route(String)
    /// The sheet over that screen, or nothing when it was dismissed.
    case sheet(String?)
    /// How far down one agent's transcript the reader had scrolled, in points.
    case scroll(AgentId, Double)
    case appearance(Appearance)
    /// The reader's type size, spelled the way a door request spells it.
    case dynamicType(String)
}

extension TraceEvent: Codable {
    private enum Key: String, CodingKey {
        case kind, screen, sheet, agent, offset, appearance, size
    }

    public init(from decoder: any Decoder) throws {
        let fields = try decoder.container(keyedBy: Key.self)
        let kind = try fields.decode(String.self, forKey: .kind)
        switch kind {
        case "route": self = .route(try fields.decode(String.self, forKey: .screen))
        case "sheet": self = .sheet(try fields.decodeIfPresent(String.self, forKey: .sheet))
        case "scroll":
            self = .scroll(
                try fields.decode(AgentId.self, forKey: .agent),
                try fields.decode(Double.self, forKey: .offset))
        case "appearance":
            self = .appearance(try fields.decode(Appearance.self, forKey: .appearance))
        case "dynamicType": self = .dynamicType(try fields.decode(String.self, forKey: .size))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: fields, debugDescription: "no trace event named \(kind)")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: Key.self)
        switch self {
        case .route(let screen):
            try fields.encode("route", forKey: .kind)
            try fields.encode(screen, forKey: .screen)
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
    /// What the screen looked like when the bundle was written. A replay of
    /// the bundle is compared with this.
    public static let screenFile = "screen.png"

    public static func lines(_ events: [TraceEvent]) throws -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try events
            .map { String(decoding: try encoder.encode($0), as: UTF8.self) }
            .joined(separator: "\n") + "\n"
    }

    public static func events(_ lines: String) throws -> [TraceEvent] {
        let decoder = JSONDecoder()
        return try lines
            .split(separator: "\n")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
            .map { try decoder.decode(TraceEvent.self, from: Data($0.utf8)) }
    }
}
