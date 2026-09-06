import Foundation
import SwiftUI

/// One block of an agent's prose.
///
/// Agents write markdown, so the transcript reads markdown: headings that are
/// headings, code that keeps its whitespace and its language, tables that line
/// up. The alternative — showing the source, or flattening it to one run of
/// text — either makes the reader do the parsing or throws away structure the
/// agent chose deliberately.
///
/// Inline styling is resolved here too, into `AttributedString`, because that
/// resolution is the expensive half and it must not happen while a finger is
/// on the screen. See ``MarkdownDocument``.
public enum MarkdownBlock: Equatable, Sendable, Identifiable {
    case heading(level: Int, text: AttributedString)
    case paragraph(AttributedString)
    /// A list. `ordered` decides whether the marker is a number or a dot, and
    /// `depth` how far each item is indented.
    case list(ordered: Bool, items: [Item])
    /// Fenced code, with whatever the fence named as its language. Never
    /// wrapped: code that wraps stops being code, so it scrolls sideways.
    case code(language: String?, text: String)
    case quote([AttributedString])
    case table(header: [AttributedString], rows: [[AttributedString]])
    case rule

    public struct Item: Equatable, Sendable, Identifiable {
        public let id: Int
        public let depth: Int
        public let marker: String
        public let text: AttributedString

        public init(id: Int, depth: Int, marker: String, text: AttributedString) {
            self.id = id
            self.depth = depth
            self.marker = marker
            self.text = text
        }
    }

    /// Position in the document. Two identical paragraphs are two blocks, so
    /// identity is where it is rather than what it says; the parser stamps it.
    public var id: Int {
        switch self {
        case .heading(_, let text): text.hashValue
        case .paragraph(let text): text.hashValue
        case .list(_, let items): items.first?.id ?? 0
        case .code(_, let text): text.hashValue
        case .quote(let lines): lines.first?.hashValue ?? 0
        case .table(let header, _): header.first?.hashValue ?? 0
        case .rule: 0
        }
    }
}

/// Prose, parsed.
///
/// A document is parsed away from the main thread and handed over whole:
/// ``MarkdownDocument/parse(_:)`` is `nonisolated` and returns a `Sendable`
/// value, so a view awaits it in a task and never blocks a frame on it. The
/// budget this exists for is the streaming one — a thousand rows arriving at
/// fifty a second — and inline attribute resolution is far too slow to do
/// while the list is being scrolled.
public struct MarkdownDocument: Equatable, Sendable {
    public let blocks: [MarkdownBlock]

    public init(blocks: [MarkdownBlock]) {
        self.blocks = blocks
    }

    /// Whether this is one short run of plain prose, which is most of what an
    /// agent writes and is far cheaper to draw as a single text view.
    public var isPlainParagraph: Bool {
        blocks.count == 1 && { if case .paragraph = blocks[0] { return true } else { return false } }()
    }

    public nonisolated static func parse(_ source: String) -> MarkdownDocument {
        var blocks: [MarkdownBlock] = []
        var lines = ArraySlice(source.components(separatedBy: "\n"))
        var counter = 0

        func next() -> Int { counter += 1; return counter }

        while let line = lines.first {
            let trimmed = line.trimmingCharacters(in: .whitespaces)

            if trimmed.isEmpty {
                lines = lines.dropFirst()
                continue
            }

            if let fence = Self.fence(trimmed) {
                lines = lines.dropFirst()
                var body: [String] = []
                while let line = lines.first, Self.fence(line.trimmingCharacters(in: .whitespaces)) == nil {
                    body.append(line)
                    lines = lines.dropFirst()
                }
                if !lines.isEmpty { lines = lines.dropFirst() }
                blocks.append(.code(
                    language: fence.isEmpty ? nil : fence,
                    text: body.joined(separator: "\n")))
                continue
            }

            if trimmed.hasPrefix("#"), let level = Self.headingLevel(trimmed) {
                lines = lines.dropFirst()
                let text = trimmed.dropFirst(level).trimmingCharacters(in: .whitespaces)
                blocks.append(.heading(level: level, text: Self.inline(text)))
                continue
            }

            if trimmed.allSatisfy({ $0 == "-" || $0 == "*" || $0 == "_" }), trimmed.count >= 3 {
                lines = lines.dropFirst()
                blocks.append(.rule)
                continue
            }

            if trimmed.hasPrefix("|"), Self.isTableRow(trimmed) {
                var rows: [[String]] = []
                while let line = lines.first,
                      Self.isTableRow(line.trimmingCharacters(in: .whitespaces)) {
                    rows.append(Self.cells(line.trimmingCharacters(in: .whitespaces)))
                    lines = lines.dropFirst()
                }
                // The second row of a markdown table is the alignment rule,
                // which is punctuation rather than content and is dropped.
                let body = rows.count > 1 && rows[1].allSatisfy(Self.isAlignmentCell)
                    ? Array(rows.dropFirst(2)) : Array(rows.dropFirst())
                blocks.append(.table(
                    header: (rows.first ?? []).map(Self.inline),
                    rows: body.map { $0.map(Self.inline) }))
                continue
            }

            if trimmed.hasPrefix(">") {
                var quoted: [AttributedString] = []
                while let line = lines.first,
                      line.trimmingCharacters(in: .whitespaces).hasPrefix(">") {
                    let text = line.trimmingCharacters(in: .whitespaces)
                        .dropFirst().trimmingCharacters(in: .whitespaces)
                    quoted.append(Self.inline(text))
                    lines = lines.dropFirst()
                }
                blocks.append(.quote(quoted))
                continue
            }

            if let first = Self.listMarker(line) {
                var items: [MarkdownBlock.Item] = []
                let ordered = first.ordered
                while let line = lines.first, let marker = Self.listMarker(line),
                      marker.ordered == ordered {
                    items.append(MarkdownBlock.Item(
                        id: next(), depth: marker.depth, marker: marker.marker,
                        text: Self.inline(marker.rest)))
                    lines = lines.dropFirst()
                }
                blocks.append(.list(ordered: ordered, items: items))
                continue
            }

            var paragraph: [String] = []
            while let line = lines.first {
                let text = line.trimmingCharacters(in: .whitespaces)
                guard !text.isEmpty, !text.hasPrefix("#"), !text.hasPrefix(">"),
                      !text.hasPrefix("|"), Self.fence(text) == nil,
                      Self.listMarker(line) == nil else { break }
                paragraph.append(text)
                lines = lines.dropFirst()
            }
            blocks.append(.paragraph(Self.inline(paragraph.joined(separator: " "))))
        }

        return MarkdownDocument(blocks: blocks)
    }

    /// Inline markdown — emphasis, code spans, links — resolved once.
    ///
    /// Foundation's own parser is used rather than a hand-written one, and it
    /// is given the whole run at once. It refuses malformed source outright, so
    /// a failure falls back to the literal text: an agent writing a stray
    /// asterisk must not blank its own sentence.
    nonisolated static func inline(_ source: some StringProtocol) -> AttributedString {
        let text = String(source)
        let options = AttributedString.MarkdownParsingOptions(
            allowsExtendedAttributes: true,
            interpretedSyntax: .inlineOnlyPreservingWhitespace)
        var parsed = (try? AttributedString(markdown: text, options: options))
            ?? AttributedString(text)
        // A link is underlined rather than coloured. Every colour in this
        // design is either the neutral ramp or the one accent, and the accent
        // means "something is waiting for you"; a link is not that, and the
        // system's own blue is not a colour this app owns.
        for run in parsed.runs where run.link != nil {
            parsed[run.range].underlineStyle = .single
        }
        return parsed
    }

    private nonisolated static func fence(_ line: String) -> String? {
        guard line.hasPrefix("```") || line.hasPrefix("~~~") else { return nil }
        return String(line.dropFirst(3)).trimmingCharacters(in: .whitespaces)
    }

    private nonisolated static func headingLevel(_ line: String) -> Int? {
        let hashes = line.prefix { $0 == "#" }.count
        guard hashes >= 1, hashes <= 6, line.dropFirst(hashes).hasPrefix(" ") else { return nil }
        return hashes
    }

    private nonisolated static func isTableRow(_ line: String) -> Bool {
        line.hasPrefix("|") && line.dropFirst().contains("|")
    }

    private nonisolated static func isAlignmentCell(_ cell: String) -> Bool {
        !cell.isEmpty && cell.allSatisfy { $0 == "-" || $0 == ":" || $0 == " " }
    }

    private nonisolated static func cells(_ line: String) -> [String] {
        var text = line
        if text.hasPrefix("|") { text.removeFirst() }
        if text.hasSuffix("|") { text.removeLast() }
        return text.components(separatedBy: "|").map {
            $0.trimmingCharacters(in: .whitespaces)
        }
    }

    private nonisolated static func listMarker(
        _ line: String
    ) -> (ordered: Bool, depth: Int, marker: String, rest: String)? {
        let indent = line.prefix { $0 == " " || $0 == "\t" }.count
        let body = line.dropFirst(indent)
        let depth = min(2, indent / 2)
        if let first = body.first, first == "-" || first == "*" || first == "+",
           body.dropFirst().hasPrefix(" ") {
            return (false, depth, "\u{2022}",
                    body.dropFirst(2).trimmingCharacters(in: .whitespaces))
        }
        let digits = body.prefix(while: \.isNumber)
        if !digits.isEmpty, body.dropFirst(digits.count).hasPrefix(". ") {
            return (true, depth, "\(digits).",
                    body.dropFirst(digits.count + 2).trimmingCharacters(in: .whitespaces))
        }
        return nil
    }
}
