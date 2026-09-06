import Foundation
import Observation

/// A stable address into a frozen review document: which file, and which of
/// that file's rows.
///
/// Positions in the document rather than line numbers, because a patch's rows
/// carry two independent numberings and only one of them exists for any given
/// row. The numbering is what a comment is finally addressed by; getting there
/// is what ``Anchor`` does.
public struct RowRef: Hashable, Sendable, Comparable {
    public let file: Int
    public let row: Int

    public init(file: Int, row: Int) {
        self.file = file
        self.row = row
    }

    public static func < (lhs: RowRef, rhs: RowRef) -> Bool {
        (lhs.file, lhs.row) < (rhs.file, rhs.row)
    }
}

/// A run of rows inside one file, as a finger drew it.
///
/// Always within one file: a range spanning two files is not a thing a patch
/// can be commented on, and the selection refuses to grow past the file it
/// started in rather than silently anchoring somewhere else.
public struct LineRange: Hashable, Sendable {
    public let file: Int
    public let from: Int
    public let to: Int

    /// Ordered on the way in, so a range drawn upwards is the same range as
    /// one drawn downwards.
    public init(file: Int, from: Int, to: Int) {
        self.file = file
        self.from = min(from, to)
        self.to = max(from, to)
    }

    public init(_ row: RowRef) {
        self.init(file: row.file, from: row.row, to: row.row)
    }

    public var rows: ClosedRange<Int> { from...to }

    public func contains(_ row: RowRef) -> Bool {
        row.file == file && rows.contains(row.row)
    }
}

/// The side of a unified diff an endpoint refers to. A removed row exists only
/// in the old file and an added one only in the new, so a comment has to say
/// which numbering it means.
public enum Side: String, Codable, Sendable, Equatable {
    case old
    case new
}

/// Where a comment lands, and the rows it was written about.
///
/// The quoted rows travel with it. A comment addressed only by line number
/// would be unreadable the moment the file moved on; carrying the text means
/// what was said and what it was said about arrive together.
public struct Anchor: Codable, Sendable, Equatable {
    public var path: String
    public var startSide: Side
    public var startLine: UInt32
    public var side: Side
    public var line: UInt32
    public var quoted: [String]

    public init(
        path: String, startSide: Side, startLine: UInt32, side: Side, line: UInt32,
        quoted: [String]
    ) {
        self.path = path
        self.startSide = startSide
        self.startLine = startLine
        self.side = side
        self.line = line
        self.quoted = quoted
    }
}

/// One thing somebody wrote about a range of lines.
public struct ReviewComment: Codable, Sendable, Equatable, Identifiable {
    public var path: String
    public var startSide: Side
    public var startLine: UInt32
    public var side: Side
    public var line: UInt32
    public var quoted: [String]
    public var text: String

    private enum CodingKeys: String, CodingKey {
        case path, side, line, quoted, text
        case startSide = "start_side"
        case startLine = "start_line"
    }

    public init(anchor: Anchor, text: String) {
        self.path = anchor.path
        self.startSide = anchor.startSide
        self.startLine = anchor.startLine
        self.side = anchor.side
        self.line = anchor.line
        self.quoted = anchor.quoted
        self.text = text
    }

    /// A comment is one remark at one place, so where it is is what it is.
    public var id: String { "\(path):\(side.rawValue):\(line):\(startLine)" }

    /// "122–123", or one number where the range is one line. An en dash, not a
    /// hyphen: it is a range and reads as one.
    public var lines: String {
        startLine == line ? "\(line)" : "\(startLine)\u{2013}\(line)"
    }
}

/// One review being written: the frozen patch, what is folded away, what a
/// finger has hold of, and everything said so far.
///
/// The document never changes. It was frozen when the turn ended and a review
/// is about that patch and no other — which is why the comments address rows
/// by the numbering the patch itself carries and why the identity travels
/// with it. Everything else here is what the reader is doing.
@MainActor
@Observable
public final class ReviewStore {
    /// The artifact this patch is, so a review sent later names the diff that
    /// was actually read.
    public let diff: ArtifactId
    public let document: ReviewDocument
    /// The document's files in the order the page reads them: alphabetical,
    /// by the ordering a person alphabetises with rather than by byte value,
    /// so `PROTOCOL.md` sorts among the paths and not before all of them.
    ///
    /// A patch's own order is the order git walked it, which is neither
    /// alphabetical nor stable between two runs over the same tree. Every
    /// address into this review — a row reference, a range, a comment's
    /// position — is an index into this order and no other.
    public let files: [ReviewFile]
    /// Files folded away, by path. Nothing is folded to begin with: a review
    /// opens showing what changed.
    public private(set) var collapsed: Set<String> = []
    /// The rows a finger has hold of, or that the sheet is open about.
    public private(set) var selection: LineRange?
    /// The comment being written. Held here rather than in the sheet so that
    /// what somebody typed survives the sheet being redrawn.
    public var draft: String = ""
    /// Everything said, in the order the document reads.
    public private(set) var comments: [ReviewComment] = []

    public init(diff: ArtifactId, document: ReviewDocument) {
        self.diff = diff
        self.document = document
        self.files = document.files.sorted {
            $0.path.localizedStandardCompare($1.path) == .orderedAscending
        }
    }

    public func isCollapsed(_ path: String) -> Bool { collapsed.contains(path) }

    public func toggle(file path: String) {
        if collapsed.contains(path) { collapsed.remove(path) } else { collapsed.insert(path) }
    }

    /// How many comments are on one file, which is what its heading counts.
    public func comments(in path: String) -> Int {
        comments.filter { $0.path == path }.count
    }

    /// The comments drawn under one row of the document, in the order they
    /// were written there.
    public func comments(under row: RowRef) -> [ReviewComment] {
        guard let file = files[safe: row.file],
              let fact = file.rows[safe: row.row] else { return [] }
        return comments.filter { comment in
            comment.path == file.path && Self.ends(comment, at: fact)
        }
    }

    /// Takes hold of a range. A range that cannot be anchored — one that ends
    /// on the break between two hunks, say — is not taken: a selection the
    /// document cannot address would open a sheet that could not be sent.
    public func select(_ range: LineRange) {
        guard anchor(range) != nil else { return }
        selection = range
    }

    /// Lets go without saying anything. The draft goes too: an unfinished
    /// comment is unfinished about a particular range, and keeping the words
    /// while losing the lines would put them somewhere nobody chose.
    public func cancel() {
        selection = nil
        draft = ""
    }

    /// Says something about a range. Nothing is added for a remark with no
    /// words in it.
    @discardableResult
    public func comment(_ range: LineRange, _ text: String) -> Bool {
        let said = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !said.isEmpty, let anchor = anchor(range) else { return false }
        let comment = ReviewComment(anchor: anchor, text: said)
        // Kept in document order rather than in the order somebody wrote them,
        // so the review reads down the patch the way the patch reads.
        let at = comments.firstIndex { existing in
            (row(of: existing) ?? Self.last) > (row(of: comment) ?? Self.last)
        }
        comments.insert(comment, at: at ?? comments.count)
        selection = nil
        draft = ""
        return true
    }

    /// Where a comment sits in the document, or nothing when the document no
    /// longer holds the row it was written about.
    public func row(of comment: ReviewComment) -> RowRef? {
        guard let file = files.firstIndex(where: { $0.path == comment.path })
        else { return nil }
        guard let row = files[file].rows.firstIndex(where: {
            Self.ends(comment, at: $0)
        }) else { return nil }
        return RowRef(file: file, row: row)
    }

    /// Resolves a range into the coordinates a comment is finally addressed
    /// by, or nothing where the range has no addressable endpoint.
    ///
    /// The rule is the shared core's: a removed row is addressed in the old
    /// file's numbering, an added or context row in the new file's, and the
    /// break between two hunks is addressed in neither.
    public func anchor(_ range: LineRange) -> Anchor? {
        guard let file = files[safe: range.file],
              let first = file.rows[safe: range.from],
              let last = file.rows[safe: range.to],
              let start = Self.endpoint(first), let end = Self.endpoint(last)
        else { return nil }
        return Anchor(
            path: file.path, startSide: start.side, startLine: start.line,
            side: end.side, line: end.line,
            quoted: file.rows[range.from...range.to].map(\.text))
    }

    /// What a range says of itself while it is being commented on: "2 lines in
    /// src/pairing.rs". The count is rows of the patch, which is what was
    /// selected, rather than lines of either file.
    public func describe(_ range: LineRange) -> String? {
        guard let file = files[safe: range.file] else { return nil }
        let count = range.to - range.from + 1
        return "\(count) line\(count == 1 ? "" : "s") in \(file.path)"
    }

    private static let last = RowRef(file: .max, row: .max)

    private static func endpoint(_ row: DiffRow) -> (side: Side, line: UInt32)? {
        switch row.kind {
        case .removed: row.old.map { (.old, $0) }
        case .added, .context: row.new.map { (.new, $0) }
        case .boundary, .note: nil
        }
    }

    private static func ends(_ comment: ReviewComment, at row: DiffRow) -> Bool {
        switch comment.side {
        case .old: row.old == comment.line
        case .new: row.new == comment.line
        }
    }
}

extension Array {
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}
