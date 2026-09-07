import AmuxMobile
import Foundation

/// One thing attached to a message, as the text of the message spells it.
///
/// Mirrors the shared vocabulary exactly. Nothing here is parsed or formatted
/// on this side: the phone asks the shared library what a piece of message
/// text says and what a review should be sent as, and holds the answer.
public struct Mention: Codable, Sendable, Equatable {
    public var kind: MentionKind
    public var name: String
    public var size: UInt64?
    public var path: String?
}

/// The closed set of things a message can carry.
public enum MentionKind: Codable, Sendable, Equatable {
    case image(id: ArtifactId)
    case file(id: ArtifactId)
    case text(body: String, lines: UInt32)
    case review(header: ReviewHeader, comments: [ReviewComment])

    private enum Key: String, CodingKey {
        case kind, id, body, lines, header, comments
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .kind) {
        case "image":
            self = .image(id: try container.decode(ArtifactId.self, forKey: .id))
        case "file":
            self = .file(id: try container.decode(ArtifactId.self, forKey: .id))
        case "text":
            self = .text(
                body: try container.decode(String.self, forKey: .body),
                lines: try container.decode(UInt32.self, forKey: .lines))
        case "review":
            self = .review(
                header: try container.decode(ReviewHeader.self, forKey: .header),
                comments: try container.decode([ReviewComment].self, forKey: .comments))
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath, debugDescription: "unknown attachment \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .image(let id):
            try container.encode("image", forKey: .kind)
            try container.encode(id, forKey: .id)
        case .file(let id):
            try container.encode("file", forKey: .kind)
            try container.encode(id, forKey: .id)
        case .text(let body, let lines):
            try container.encode("text", forKey: .kind)
            try container.encode(body, forKey: .body)
            try container.encode(lines, forKey: .lines)
        case .review(let header, let comments):
            try container.encode("review", forKey: .kind)
            try container.encode(header, forKey: .header)
            try container.encode(comments, forKey: .comments)
        }
    }
}

/// What a review names about the patch it is a review of.
///
/// The blobs are the new-side git hashes of every changed path, so a review
/// says exactly what was read even after the working tree has moved on.
public struct ReviewHeader: Codable, Sendable, Equatable {
    public var diff: ArtifactId
    public var base: String
    public var head: String
    public var mergeBase: String?
    public var blobs: [[String]]

    private enum CodingKeys: String, CodingKey {
        case diff, base, head, blobs
        case mergeBase = "merge_base"
    }
}

/// A run of ordinary prose, or one well-formed attachment.
public enum Segment: Codable, Sendable, Equatable {
    case prose(String)
    case mention(Mention)

    private enum Key: String, CodingKey { case segment, value }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .segment) {
        case "prose": self = .prose(try container.decode(String.self, forKey: .value))
        case "mention": self = .mention(try container.decode(Mention.self, forKey: .value))
        case let other:
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath, debugDescription: "unknown segment \(other)"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .prose(let text):
            try container.encode("prose", forKey: .segment)
            try container.encode(text, forKey: .value)
        case .mention(let mention):
            try container.encode("mention", forKey: .segment)
            try container.encode(mention, forKey: .value)
        }
    }

    public var mention: Mention? {
        if case .mention(let mention) = self { return mention }
        return nil
    }
}

/// An artifact a message carries with it.
public struct DraftAttachment: Codable, Sendable, Equatable {
    public var id: ArtifactId
    public var kind: ArtifactKind
    public var name: String
    public var mime: String
    public var size: UInt64

    public init(id: ArtifactId, kind: ArtifactKind, name: String, mime: String, size: UInt64) {
        self.id = id
        self.kind = kind
        self.name = name
        self.mime = mime
        self.size = size
    }
}

public enum ArtifactKind: String, Codable, Sendable, Equatable {
    case image
    case file
    case diff
}

/// A written review on its way to the composer.
///
/// The element is the message text the review is sent as, formatted by the
/// shared library. The attachment carries no bytes: the patch is already
/// stored where it was produced, and this rides along only to keep it there
/// for whoever reads the review.
public struct ReviewToken: Sendable, Equatable {
    public let element: String
    public let attachment: DraftAttachment
    public let comments: Int

    /// What the token says on the button that attaches it and on the token
    /// itself: the number of remarks, which is the whole of what was written.
    public var label: String {
        comments == 1 ? "Review \u{00B7} 1 comment" : "Review \u{00B7} \(comments) comments"
    }
}

/// One thing attached to a message, as the composer holds it.
///
/// A token is a single object in the sentence: it is inserted where the caret
/// is, one backspace takes the whole of it, and it can be picked up and put
/// down elsewhere. That is only possible because it occupies exactly one
/// character of the draft — everything a reader sees on it is drawn from here
/// and nothing of it is typed.
///
/// The `element` is the canonical text this becomes when the message is sent,
/// and it is spelled by the shared library rather than here: an element that
/// escaped its own body wrongly would be prose on every other client.
public struct DraftToken: Sendable, Equatable {
    /// Which of amux's four closed attachment kinds this is, or the command
    /// a message can be. It decides the glyph and nothing else; what the token
    /// says is `label`.
    public enum Kind: String, Sendable, Equatable {
        case photo
        case file
        case text
        case review
        /// A provider command the message *is*, rather than something the
        /// message carries. It is a token for the same reason the others are —
        /// one object to the caret, one backspace to be rid of — but it spells
        /// no element into the text: it travels as its own segment of the
        /// draft, first and alone, which is the only shape the core accepts.
        case command
    }

    public let kind: Kind
    /// What the token reads on screen, in the composer and in the feed alike.
    public let label: String
    /// The message text this token is, spelled by the shared library.
    public let element: String
    /// The artifact that has to travel with the message, where there is one.
    /// Pasted text has none: it is carried inside the element itself.
    public let attachment: DraftAttachment?

    public init(kind: Kind, label: String, element: String, attachment: DraftAttachment?) {
        self.kind = kind
        self.label = label
        self.element = element
        self.attachment = attachment
    }

    /// The review, as one token in the sentence somebody is writing about it.
    public init(_ review: ReviewToken) {
        self.init(
            kind: .review, label: review.label, element: review.element,
            attachment: review.attachment)
    }
}

/// A message being written: prose, and the attachments standing in it.
///
/// The draft is one string. Each token holds one character of it — a private
/// character nobody can type — and what that character stands for is looked up
/// beside it. Everything the gestures have to do then falls out of ordinary
/// text editing: the picker inserts a character at the caret, one backspace
/// deletes a character, and moving a token is moving a character. Nothing has
/// to know that a token is several words wide, because in the draft it is not.
///
/// It lives on the conversation rather than in the composer's own view state,
/// so leaving to read the diff, attaching a review and coming back finds the
/// paragraph that was half written still there.
public struct MessageDraft: Sendable, Equatable {
    /// What is written, with one stand-in character per token.
    ///
    /// Set it and the tokens follow: a stand-in that is no longer in the text
    /// is a token that was deleted, whatever deleted it.
    public var body: String {
        get { written }
        set {
            written = newValue
            let live = Set(written)
            tokens = tokens.filter { live.contains($0.key) }
            caret = min(caret, written.count)
        }
    }

    /// Where the next thing written lands, counted in characters from the
    /// start. The field reports it; the picker and the paste read it.
    public private(set) var caret: Int = 0

    /// What each stand-in character stands for.
    public private(set) var tokens: [Character: DraftToken] = [:]

    private var written: String = ""

    public init(prose: String = "") {
        self.body = prose
    }

    public var isEmpty: Bool {
        tokens.isEmpty && written.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// The tokens in the order they stand in the sentence.
    public var ordered: [DraftToken] { written.compactMap { tokens[$0] } }

    public mutating func place(caret offset: Int) {
        caret = max(0, min(offset, written.count))
    }

    public mutating func clear() {
        written = ""
        tokens = [:]
        caret = 0
    }

    /// Puts a token where the caret is, and leaves the caret after it.
    @discardableResult
    public mutating func insert(_ token: DraftToken) -> Character {
        let slot = freeSlot()
        tokens[slot] = token
        written.insert(slot, at: index(caret))
        caret += 1
        return slot
    }

    /// Puts ordinary text where the caret is.
    public mutating func insert(text: String) {
        written.insert(contentsOf: text, at: index(caret))
        caret += text.count
    }

    /// The review, attached from the page it was written on.
    public mutating func attach(_ review: ReviewToken) {
        insert(DraftToken(review))
    }

    /// One backspace. A token is one character, so this takes the whole of it
    /// or one letter, and never half of either.
    public mutating func backspace() {
        guard caret > 0 else { return }
        let removed = written.remove(at: index(caret - 1))
        caret -= 1
        if tokens[removed] != nil, !written.contains(removed) { tokens[removed] = nil }
    }

    /// Picks the character at `from` up and puts it down before what is at
    /// `to`, which is how a token is moved: the sentence closes behind it
    /// first, so `to` is read in the text without it.
    public mutating func move(from: Int, to: Int) {
        guard from >= 0, from < written.count else { return }
        let moved = written.remove(at: index(from))
        let landing = max(0, min(to, written.count))
        written.insert(moved, at: index(landing))
        caret = landing + 1
    }

    /// What a paste becomes, decided by the shared library: text short enough
    /// to read in place lands as characters, and anything long enough to bury
    /// the sentence around it becomes one token.
    public mutating func paste(_ text: String) {
        switch Bridge.pasted(text) {
        case .prose(let body): insert(text: body)
        case .token(let token): insert(token)
        }
    }

    /// The message as it will be sent: every stand-in replaced by the element
    /// it stands for.
    public var text: String {
        var sent = ""
        for character in written {
            if let token = tokens[character] {
                sent += token.element
            } else {
                sent.append(character)
            }
        }
        return sent
    }

    /// The artifacts that travel with the message, in the order they stand in
    /// it. A review's patch and an attached file are fetched by whoever reads
    /// the message, so they are named here as well as in the text.
    public var attachments: [DraftAttachment] { ordered.compactMap(\.attachment) }

    /// What this draft says, read back through the shared parser.
    ///
    /// A composer draws its own text the way a reader will read it rather than
    /// the way it was assembled, so a token can only be drawn if the element
    /// behind it is one the whole system accepts.
    public var segments: [Segment] { Bridge.attachments(in: text) }

    /// The next character no token is using. The private use area is where
    /// they come from: nothing in it can arrive from a keyboard, a paste or a
    /// dictation, so a stand-in can never collide with something written.
    private func freeSlot() -> Character {
        for code in Self.slots {
            guard let scalar = Unicode.Scalar(code) else { continue }
            let slot = Character(scalar)
            if tokens[slot] == nil { return slot }
        }
        return Character(Unicode.Scalar(Self.slots.lowerBound)!)
    }

    private static let slots: ClosedRange<UInt32> = 0xE000...0xF8FF

    private func index(_ offset: Int) -> String.Index {
        written.index(written.startIndex, offsetBy: max(0, min(offset, written.count)))
    }

    /// The shared send command this draft is, addressed to one agent.
    ///
    /// One text segment carrying the whole message: an attachment element is
    /// part of what was written, not a second thing sent beside it. The
    /// artifacts travel separately because the reader has to be able to fetch
    /// the bytes, not only read what was said about them.
    public func command(to agent: AgentId) -> BridgeCommand? {
        Wire(command: "send", action: nil, agent: agent, draft: wire).shared
    }

    /// The same draft, held instead of sent, because a turn is running.
    ///
    /// The core holds one message per agent and delivers it at the first turn
    /// end. Holding is not sending later from the phone: a phone that goes to
    /// sleep, loses its network or is put away must not take the message with
    /// it, and the machine the agent runs on is the only place that can be
    /// sure the turn ended exactly once.
    public func holdCommand(to agent: AgentId) -> BridgeCommand? {
        Wire(command: "queue", action: "hold", agent: agent, draft: wire).shared
    }

    /// The same draft, put in the place of one already held.
    ///
    /// A second hold is refused by the core outright — one message is queued,
    /// not a queue — so writing another one while one waits is a replacement
    /// and is spelled as one. The old text is gone because unqueueing is how
    /// you get it back, and this is the other gesture.
    public func replaceCommand(to agent: AgentId) -> BridgeCommand? {
        Wire(command: "queue", action: "replace", agent: agent, draft: wire).shared
    }

    /// Dropping the held message, which carries no draft: one is queued per
    /// agent and the core knows which. It is a static because it is not about
    /// any draft — the message being cancelled is the one the host is holding,
    /// not the one in front of you.
    public static func cancelQueue(_ agent: AgentId) -> BridgeCommand? {
        .shared(.object([
            "command": .string("queue"),
            "action": .string("cancel"),
            "agent": .string(agent.description),
        ]))
    }

    /// The command this draft is, where it is one.
    ///
    /// Only the token standing at the very front counts. The core takes a
    /// command token first and alone, so a `/compact` dropped into the middle
    /// of a sentence is not a command that got misplaced — it is prose, and
    /// the composer never makes one anywhere else.
    public var command: String? {
        guard let first = written.first, let token = tokens[first],
              token.kind == .command
        else { return nil }
        return token.label
    }

    private var wire: Wire.Body {
        guard let name = command else {
            return .init(segments: [.init(text: text)], attachments: attachments)
        }
        // The command names itself and the rest of the message is its
        // arguments, in that order, because that is the order the core reads
        // them in. The stand-in the command occupies spells nothing, so the
        // arguments are simply what is left.
        return .init(
            segments: [.command(name), .init(text: text)], attachments: attachments)
    }

    private struct Wire: Encodable {
        let command: String
        let action: String?
        let agent: AgentId
        let draft: Body

        struct Body: Encodable {
            let segments: [TextSegment]
            let attachments: [DraftAttachment]
        }

        /// One piece of the shared draft: what was written, or the command
        /// the message is. Written by hand for the same reason `Wire` is —
        /// each shape carries its own field and nothing else, and a null
        /// beside the tag is a field the core never wrote.
        enum TextSegment: Encodable {
            case text(String)
            case command(String)

            private enum Key: String, CodingKey { case segment, text, name }

            init(text: String) { self = .text(text) }

            func encode(to encoder: any Encoder) throws {
                var container = encoder.container(keyedBy: Key.self)
                switch self {
                case .text(let text):
                    try container.encode("text", forKey: .segment)
                    try container.encode(text, forKey: .text)
                case .command(let name):
                    try container.encode("command_token", forKey: .segment)
                    try container.encode(name, forKey: .name)
                }
            }
        }

        private enum Key: String, CodingKey { case command, action, agent, draft }

        /// Written by hand so that a send carries no `action` at all. The
        /// shared command is a tagged union and a null discriminant beside the
        /// tag is a field the core never wrote and would have to decide what
        /// to do with.
        func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: Key.self)
            try container.encode(command, forKey: .command)
            try container.encodeIfPresent(action, forKey: .action)
            try container.encode(agent, forKey: .agent)
            try container.encode(draft, forKey: .draft)
        }

        var shared: BridgeCommand? {
            guard let data = try? AmuxJSON.encoder.encode(self),
                  let body = try? AmuxJSON.decoder.decode(JSONValue.self, from: data)
            else { return nil }
            return .shared(body)
        }
    }
}

extension Bridge {
    /// Formats a written review as the token a composer holds.
    ///
    /// The element is spelled by the shared library, not here: the review body
    /// frames each remark by its length in bytes and escapes what would close
    /// the element early, and a second spelling of those rules would be a
    /// second thing to keep right.
    public static func reviewToken(
        diff: ArtifactId, document: ReviewDocument, comments: [ReviewComment]
    ) -> ReviewToken? {
        struct Request: Encodable {
            let diff: ArtifactId
            let document: ReviewDocument
            let comments: [ReviewComment]
        }
        guard let request = try? AmuxJSON.encoder.encode(
            Request(diff: diff, document: document, comments: comments)),
            let reply = String(data: request, encoding: .utf8)
                .flatMap({ amux_mobile_review_element($0) })
        else { return nil }
        defer { amux_mobile_free(reply) }
        struct Reply: Decodable {
            let element: String
            let attachment: DraftAttachment
        }
        guard let formatted = try? AmuxJSON.decoder.decode(
            Reply.self, from: Data(String(cString: reply).utf8)) else { return nil }
        return ReviewToken(
            element: formatted.element, attachment: formatted.attachment,
            comments: comments.count)
    }

    /// The token an artifact already stored becomes in the message.
    ///
    /// Photo and File differ only in the glyph a reader sees and the kind the
    /// element names; both are one stored blob referred to by its identity, so
    /// nothing of the bytes rides in the draft.
    public static func token(for attachment: DraftAttachment) -> DraftToken? {
        struct Request: Encodable {
            let id: ArtifactId
            let kind: ArtifactKind
            let name: String
            let size: UInt64
        }
        let request = Request(
            id: attachment.id, kind: attachment.kind, name: attachment.name,
            size: attachment.size)
        guard let element = element(
            from: request, calling: { amux_mobile_attachment_element($0) })
        else { return nil }
        return DraftToken(
            kind: attachment.kind == .image ? .photo : .file,
            label: attachment.name, element: element, attachment: attachment)
    }

    /// What a paste becomes.
    public enum Pasted: Sendable, Equatable {
        /// Short enough to read in place: it stays what was written.
        case prose(String)
        /// Long enough to bury the sentence around it: named and counted
        /// rather than shown, because drawing eighteen kilobytes of log in a
        /// composer is the app disagreeing with its own model.
        case token(DraftToken)
    }

    /// Routes a paste by size. Where the line sits is the shared library's
    /// answer and not the phone's, so a paragraph that becomes a token in the
    /// terminal becomes one here.
    public static func pasted(_ text: String) -> Pasted {
        guard let reply = amux_mobile_paste(text) else { return .prose(text) }
        defer { amux_mobile_free(reply) }
        struct Reply: Decodable {
            let prose: String?
            let element: String?
            let lines: UInt32?
            let name: String?
        }
        guard let answer = try? AmuxJSON.decoder.decode(
            Reply.self, from: Data(String(cString: reply).utf8)) else { return .prose(text) }
        if let prose = answer.prose { return .prose(prose) }
        guard let element = answer.element, let lines = answer.lines else {
            return .prose(text)
        }
        let name = (answer.name ?? "Pasted text").capitalizedFirst
        return .token(DraftToken(
            kind: .text, label: "\(name) \u{00B7} \(lines) lines", element: element,
            attachment: nil))
    }

    /// One reply from the shared library that is only an element.
    private struct Element: Decodable { let element: String }

    private static func element(
        from request: some Encodable, calling ffi: (String) -> UnsafeMutablePointer<CChar>?
    ) -> String? {
        guard let data = try? AmuxJSON.encoder.encode(request),
              let json = String(data: data, encoding: .utf8),
              let reply = ffi(json)
        else { return nil }
        defer { amux_mobile_free(reply) }
        return try? AmuxJSON.decoder.decode(
            Element.self, from: Data(String(cString: reply).utf8)).element
    }

    /// What a piece of message text says: prose and the attachments in it.
    ///
    /// Anything the shared parser does not accept stays prose, byte for byte,
    /// which is what a reader on any other client will see too.
    public static func attachments(in text: String) -> [Segment] {
        guard let json = amux_mobile_attachments(text) else { return [] }
        defer { amux_mobile_free(json) }
        let data = Data(String(cString: json).utf8)
        return (try? AmuxJSON.decoder.decode([Segment].self, from: data)) ?? []
    }
}

extension String {
    /// The same words with the first letter raised. A name the shared library
    /// spells for a log line ("pasted text") is a label here, and a label in
    /// this app starts with a capital.
    public var capitalizedFirst: String {
        guard let first else { return self }
        return String(first).uppercased() + dropFirst()
    }
}
