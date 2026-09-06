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
        comments == 1 ? "Review · 1 comment" : "Review · \(comments) comments"
    }
}

/// A message being written: a review, if one has been attached, and whatever
/// was said beside it.
///
/// A review is not a verdict. It carries remarks at places in a patch and
/// nothing else, so the general thing somebody wants to say about it is
/// ordinary prose in the same message rather than a field of the review.
///
/// The composer is not built yet. What is here is what a composer will hold
/// and send; until there is one, a review attached from the diff page waits
/// here for it.
public struct MessageDraft: Sendable, Equatable {
    /// What was said beside the review.
    public var prose: String = ""
    public private(set) var review: ReviewToken?

    public init(prose: String = "", review: ReviewToken? = nil) {
        self.prose = prose
        self.review = review
    }

    public var isEmpty: Bool {
        review == nil && prose.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    public mutating func attach(_ review: ReviewToken) {
        self.review = review
    }

    public mutating func clear() {
        prose = ""
        review = nil
    }

    /// The message as it will be sent: the review's element, then the remark.
    public var text: String {
        guard let review else { return prose }
        return prose.isEmpty ? review.element : "\(review.element)\n\n\(prose)"
    }

    public var attachments: [DraftAttachment] {
        review.map { [$0.attachment] } ?? []
    }

    /// What this draft says, read back through the shared parser.
    ///
    /// A composer draws its own text the way a reader will read it rather than
    /// the way it was assembled, so a token can only be drawn if the element
    /// behind it is one the whole system accepts.
    public var segments: [Segment] { Bridge.attachments(in: text) }

    /// The shared send command this draft is, addressed to one agent.
    ///
    /// One text segment carrying the whole message: a review element is part
    /// of what was written, not a second thing sent beside it. The artifact
    /// travels separately because the reader has to be able to fetch the
    /// patch, not only read what was said about it.
    public func command(to agent: AgentId) -> BridgeCommand? {
        Wire(agent: agent, draft: .init(
            segments: [.init(text: text)], attachments: attachments)).shared
    }

    private struct Wire: Encodable {
        let command = "send"
        let agent: AgentId
        let draft: Body

        struct Body: Encodable {
            let segments: [TextSegment]
            let attachments: [DraftAttachment]
        }

        struct TextSegment: Encodable {
            let segment = "text"
            let text: String
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
