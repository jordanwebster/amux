import AmuxCore
import AmuxDesign
import SwiftUI

/// What happened, in order.
///
/// Everything an agent does hangs off one rail: a single hairline in the glyph
/// column that runs the length of a turn's work, so a screenful of activity
/// reads as one thing with a shape rather than as a stack of unrelated cards.
/// What is not work breaks the rail and takes the full width — the person's own
/// prompt, the agent's prose, and the rules that close a turn — because those
/// are the parts you read rather than scan.
///
/// The rows themselves come from ``TranscriptRow``, which reads each layer in
/// its own vocabulary. This file decides nothing about what a row means; it
/// only draws what the projection already named.
struct TranscriptFeed: View {
    @Environment(\.design) private var design
    let rows: [TranscriptRow]

    var body: some View {
        LazyVStack(alignment: .leading, spacing: 0) {
            ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                TranscriptRowView(
                    row: row,
                    railContinues: index + 1 < rows.count && rows[index + 1].onRail)
            }
        }
        .padding(.horizontal, design.metrics.gutter)
    }
}

/// One row, on the rail or breaking it.
private struct TranscriptRowView: View {
    @Environment(\.design) private var design
    let row: TranscriptRow
    let railContinues: Bool

    var body: some View {
        switch row.kind {
        case .prompt(let text):
            PromptSurface(text: text)
                .padding(.vertical, design.metrics.feedGap / 2)
        case .prose(let markdown, let open):
            Prose(markdown: markdown, open: open)
                .padding(.vertical, design.metrics.feedGap / 2)
        case .turnEnd(let meta):
            FeedRule(
                kind: "turn-end", glyph: nil,
                label: meta.map { "\($0) · turn ended" } ?? "turn ended")
                .padding(.vertical, design.metrics.feedGap / 2)
        case .compaction(let before, let after):
            FeedRule(
                kind: "compaction", glyph: "arrow.down.right.and.arrow.up.left",
                label: Self.compacted(before, after))
                .padding(.vertical, design.metrics.feedGap / 2)
        default:
            Rail(glyph: glyph, accented: accented, continues: railContinues) {
                content
            }
        }
    }

    /// "compacted · 148k → 22k", or just "compacted" when the layer did not
    /// say what it cost. A number nobody reported is not invented.
    private static func compacted(_ before: UInt64?, _ after: UInt64?) -> String {
        guard let before, let after else { return "compacted" }
        return "compacted · \(tokens(before)) \u{2192} \(tokens(after))"
    }

    private static func tokens(_ count: UInt64) -> String {
        count >= 1000 ? "\(count / 1000)k" : "\(count)"
    }

    @ViewBuilder
    private var content: some View {
        switch row.kind {
        case .exploration(let reads, let searches, let last, let inside):
            ExplorationRow(reads: reads, searches: searches, last: last, inside: inside)
        case .edit(let path, let added, let removed):
            EditRow(path: path, added: added, removed: removed)
        case .wrote(let path, let meta):
            ActivityRow(kind: "wrote", verb: "Wrote", subject: path, mono: true, meta: meta)
        case .ran(let command, let meta, let output):
            RanRow(command: command, meta: meta, output: output)
        case .tool(let name, let detail, let meta):
            ActivityRow(kind: "tool", verb: name, subject: detail, mono: true, meta: meta)
        case .denied(let label, let reason):
            ActivityRow(
                kind: "denied", verb: "Denied", subject: label, mono: true, meta: nil,
                note: reason)
        case .failed(let label, let message):
            ActivityRow(
                kind: "failed", verb: "Failed", subject: label, mono: true, meta: nil,
                note: message)
        case .interrupted(let toolUse):
            ActivityRow(
                kind: "interrupted", verb: "Interrupted",
                subject: toolUse ? "a tool it asked to run" : nil, mono: false, meta: nil)
        case .providerError(let message):
            ActivityRow(
                kind: "provider-error", verb: "Provider error", subject: nil, mono: false,
                meta: nil, note: message)
        case .thinking(let seconds, let redacted):
            ActivityRow(
                kind: "thinking", verb: seconds.map { "Thought for \($0)s" } ?? "Thought",
                subject: redacted ? "withheld" : nil, mono: false, meta: nil)
        case .subagent(let name, let kind, let state):
            ActivityRow(
                kind: "subagent", verb: state == nil ? "Started" : (state ?? "").capitalized,
                subject: [name, kind].compactMap { $0 }.joined(separator: " \u{00B7} "),
                mono: true, meta: nil)
        case .agentMessage(let from, let text, let outbound, let note):
            AgentMessageRow(from: from, text: text, outbound: outbound, note: note)
        case .exit(let text):
            ActivityRow(kind: "exit", verb: text, subject: nil, mono: false, meta: nil)
        case .unreadable(let label):
            ActivityRow(
                kind: "unreadable", verb: "Unreadable", subject: label, mono: true, meta: nil,
                note: "This build does not know this row; it is kept as it arrived.")
        case .prompt, .prose, .turnEnd, .compaction:
            EmptyView()
        }
    }

    private var glyph: String {
        switch row.kind {
        case .exploration: "magnifyingglass"
        case .edit: "plusminus"
        case .wrote: "square.and.pencil"
        case .ran: "chevron.left.forwardslash.chevron.right"
        case .tool: "wrench.adjustable"
        case .denied: "hand.raised"
        case .failed: "exclamationmark.triangle"
        case .interrupted: "xmark"
        case .providerError: "exclamationmark.triangle"
        case .thinking: "ellipsis"
        case .subagent: "arrow.triangle.branch"
        case .agentMessage(_, _, let outbound, _):
            outbound ? "arrow.turn.up.right" : "arrow.turn.down.left"
        case .exit: "power"
        case .unreadable: "questionmark.square.dashed"
        case .prompt, .prose, .turnEnd, .compaction: "circle"
        }
    }

    /// Only four row kinds carry the accent, and only on the glyph.
    ///
    /// A refusal, a failure, an interruption and a provider error are the
    /// moments a reader is scanning for, and the mark has to find them. It
    /// stops at the mark: colouring the words too would turn a page with three
    /// failures on it into a page that is mostly coloured, which is the same as
    /// a page with no colour on it at all.
    private var accented: Bool {
        switch row.kind {
        case .denied, .failed, .interrupted, .providerError: true
        default: false
        }
    }
}

/// The rail: the glyph column, the hairline that joins one row's work to the
/// next, and the row's own content beside it.
private struct Rail<Content: View>: View {
    @Environment(\.design) private var design
    let glyph: String
    let accented: Bool
    let continues: Bool
    @ViewBuilder let content: Content

    /// Wide enough for the widest glyph in the vocabulary and no wider: the
    /// column is a margin, and every point of it is width the prose beside it
    /// does not get.
    private let column: CGFloat = 26

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            VStack(spacing: 3) {
                Image(systemName: glyph)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(accented ? design.accent.color : design.inkFaint.color)
                    .frame(width: column, height: 20)
                // The line is drawn per row rather than once behind the whole
                // feed, so a lazy list that has not built the rows below still
                // draws a rail that stops where the work does.
                Rectangle()
                    .fill(design.hairline.color)
                    .frame(width: 1)
                    .frame(maxHeight: .infinity)
                    .opacity(continues ? 1 : 0)
            }
            .frame(width: column)
            content
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.bottom, 7)
        }
        .fixedSize(horizontal: false, vertical: true)
    }
}

/// What the person asked for.
///
/// A surface rather than a rail row, and set in from the leading edge so the
/// eye reads it as the other side of the conversation without needing a label
/// saying who wrote it.
private struct PromptSurface: View {
    @Environment(\.design) private var design
    let text: String

    var body: some View {
        HStack {
            Spacer(minLength: 36)
            Text(text)
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .background {
                    RoundedRectangle(cornerRadius: design.metrics.cardRadius, style: .continuous)
                        .fill(design.sunken.color)
                }
        }
        .accessibilityElement(children: .combine)
        .identified("transcript.prompt", label: text)
    }
}

/// What the agent said.
///
/// The markdown is parsed away from the main thread and the blocks arrive as a
/// finished value. Parsing on the main thread is what a streaming transcript
/// cannot afford: resolving inline attributes for a paragraph costs more than a
/// frame, and fifty rows a second is fifty of those.
private struct Prose: View {
    @Environment(\.design) private var design
    let markdown: String
    let open: Bool
    @State private var document: MarkdownDocument?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array((document?.blocks ?? []).enumerated()), id: \.offset) { _, block in
                MarkdownBlockView(block: block)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        // Links are underlined by the parser, so the tint only has to stop
        // them arriving in the system's blue, which is not a colour this
        // design owns.
        .tint(design.ink.color)
        .task(id: markdown) {
            let source = markdown
            document = await Task.detached(priority: .userInitiated) {
                MarkdownDocument.parse(source)
            }.value
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(markdown)
        .identified("transcript.prose", value: open ? "open" : "final")
    }
}

private struct MarkdownBlockView: View {
    @Environment(\.design) private var design
    let block: MarkdownBlock

    var body: some View {
        switch block {
        case .heading(let level, let text):
            Text(text)
                .designFont(level <= 2 ? .screenTitle : .bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
        case .paragraph(let text):
            Text(text)
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
        case .list(_, let items):
            VStack(alignment: .leading, spacing: 5) {
                ForEach(items) { item in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(item.marker)
                            .designFont(.body, design)
                            .foregroundStyle(design.inkFaint.color)
                        Text(item.text)
                            .designFont(.body, design)
                            .foregroundStyle(design.ink.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.leading, CGFloat(item.depth) * 16)
                }
            }
        case .code(let language, let text):
            CodeBlock(language: language, text: text)
        case .quote(let lines):
            HStack(alignment: .top, spacing: 10) {
                Rectangle()
                    .fill(design.hairline.color)
                    .frame(width: 2)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                        Text(line)
                            .designFont(.body, design)
                            .foregroundStyle(design.inkMuted.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            .fixedSize(horizontal: false, vertical: true)
        case .table(let header, let rows):
            TableBlock(header: header, rows: rows)
        case .rule:
            Rectangle()
                .fill(design.hairline.color)
                .frame(height: design.metrics.hairline)
        }
    }
}

/// Fenced code, scrolling sideways.
///
/// Code that wraps stops being code: an indented block whose lines fold reads
/// as prose with the structure taken out. So it keeps its own lines and the
/// reader travels along them, and the fence's language is stated on the
/// surface, because a block of unfamiliar syntax is much easier to read once
/// you know what it is.
private struct CodeBlock: View {
    @Environment(\.design) private var design
    let language: String?
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let language, !language.isEmpty {
                Text(language)
                    .designFont(.sectionTitle, design)
                    .foregroundStyle(design.inkFaint.color)
                    .padding(.horizontal, 12)
                    .padding(.top, 8)
            }
            ScrollView(.horizontal) {
                Text(text)
                    .designFont(.mono, design)
                    .foregroundStyle(design.ink.color)
                    .textSelection(.enabled)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
            }
            .scrollIndicators(.hidden)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                .fill(design.sunken.color)
        }
        .identified("transcript.code", value: language ?? "plain")
    }
}

/// A table, kept as a table. It scrolls sideways for the same reason code
/// does: columns that wrap stop lining up, and a table that does not line up
/// is a list with extra punctuation.
private struct TableBlock: View {
    @Environment(\.design) private var design
    let header: [AttributedString]
    let rows: [[AttributedString]]

    var body: some View {
        ScrollView(.horizontal) {
            VStack(alignment: .leading, spacing: 6) {
                line(header, emphasis: true)
                Rectangle()
                    .fill(design.hairline.color)
                    .frame(height: design.metrics.hairline)
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    line(row, emphasis: false)
                }
            }
        }
        .scrollIndicators(.hidden)
    }

    private func line(_ cells: [AttributedString], emphasis: Bool) -> some View {
        HStack(alignment: .top, spacing: 18) {
            ForEach(Array(cells.enumerated()), id: \.offset) { _, cell in
                Text(cell)
                    .designFont(emphasis ? .bodyEmphasis : .body, design)
                    .foregroundStyle(emphasis ? design.ink.color : design.inkMuted.color)
                    .frame(minWidth: 60, alignment: .leading)
            }
        }
    }
}

/// A rule across the feed, with what it is about written into it.
private struct FeedRule: View {
    @Environment(\.design) private var design
    /// Which rule this is, in the name the screen goes by. A turn ending and
    /// history being compacted away are different events and are told apart
    /// by whoever reads the screen, not only by the words on them.
    let kind: String
    let glyph: String?
    let label: String

    var body: some View {
        HStack(spacing: 7) {
            if let glyph {
                Image(systemName: glyph)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(design.inkFaint.color)
            }
            Text(label)
                .designFont(.caption, design)
                .foregroundStyle(design.inkFaint.color)
                .lineLimit(1)
            Rectangle()
                .fill(design.hairline.color)
                .frame(height: design.metrics.hairline)
        }
        .accessibilityElement(children: .combine)
        .identified("transcript.\(kind)", label: label)
    }
}

/// A run of reads and searches, folded to its counts.
///
/// The counts are the point: what a reader wants from six looks in a row is
/// "it looked around, here is the last place it landed", not six lines. It
/// opens, because the paths matter once you are asking a question about them.
private struct ExplorationRow: View {
    @Environment(\.design) private var design
    let reads: Int
    let searches: Int
    let last: String
    let inside: [TranscriptRow.Detail]
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button { open.toggle() } label: {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(counts)
                        .designFont(.body, design)
                        .foregroundStyle(design.ink.color)
                        .fixedSize()
                    Text(last)
                        .designFont(.mono, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                        .truncationMode(.head)
                    Spacer(minLength: 4)
                    Image(systemName: open ? "chevron.up" : "chevron.down")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(design.inkFaint.color)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if open {
                VStack(alignment: .leading, spacing: 3) {
                    ForEach(inside) { detail in
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Text(detail.verb)
                                .designFont(.detail, design)
                                .foregroundStyle(design.inkMuted.color)
                            Text(detail.subject)
                                .designFont(.monoSmall, design)
                                .foregroundStyle(design.inkFaint.color)
                                .lineLimit(1)
                                .truncationMode(.head)
                        }
                    }
                }
            }
        }
        .accessibilityElement(children: .contain)
        .identified("transcript.exploration", label: "\(counts), \(last)", value: open ? "open" : "folded")
    }

    /// "4 reads · 2 searches", and only the halves that happened.
    private var counts: String {
        [reads > 0 ? "\(reads) read\(reads == 1 ? "" : "s")" : nil,
         searches > 0 ? "\(searches) search\(searches == 1 ? "" : "es")" : nil]
            .compactMap { $0 }.joined(separator: " \u{00B7} ")
    }
}

/// A file that changed: its path, and what it cost.
private struct EditRow: View {
    @Environment(\.design) private var design
    let path: String
    let added: Int
    let removed: Int

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(path)
                .designFont(.mono, design)
                .foregroundStyle(design.ink.color)
                .lineLimit(1)
                .truncationMode(.head)
            Spacer(minLength: 6)
            Text("+\(added)")
                .designFont(.mono, design)
                .foregroundStyle(design.added.color)
            Text("\u{2212}\(removed)")
                .designFont(.mono, design)
                .foregroundStyle(design.removed.color)
        }
        .accessibilityElement(children: .combine)
        .identified("transcript.edit", label: "\(path), \(added) added, \(removed) removed")
    }
}

/// A command, and what it printed.
private struct RanRow: View {
    @Environment(\.design) private var design
    let command: String
    let meta: String?
    let output: TranscriptRow.Output?

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            ActivityRow(kind: "ran", verb: "Ran", subject: command, mono: true, meta: meta)
            if let output { OutputPreview(output: output) }
        }
    }
}

/// Command output, kept to its head.
///
/// Two hundred lines of build log is not what anyone opened the conversation
/// for, but the first line of it usually is. So the head is shown and the rest
/// is counted rather than dropped: a hidden count is a promise that nothing was
/// thrown away, which is the difference between a summary and a lie.
private struct OutputPreview: View {
    @Environment(\.design) private var design
    let output: TranscriptRow.Output

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(output.head)
                .designFont(.mono, design)
                .foregroundStyle(design.inkMuted.color)
                .lineLimit(2)
            if output.hidden != 0 {
                HStack(spacing: 6) {
                    Image(systemName: "ellipsis")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(design.inkFaint.color)
                    Text(hiddenLabel)
                        .designFont(.mono, design)
                        .foregroundStyle(design.inkFaint.color)
                }
            }
        }
        .accessibilityElement(children: .combine)
        .identified("transcript.output", value: hiddenLabel)
    }

    /// A negative count is the projection saying the head was clipped before
    /// it could be counted, so the row says there is more without claiming a
    /// number it does not have.
    private var hiddenLabel: String {
        guard output.hidden > 0 else { return "more lines" }
        return "\(output.hidden) more line\(output.hidden == 1 ? "" : "s")"
    }
}

/// A message between two agents, collapsed to its first line until opened.
///
/// Collapsed because it is somebody else's conversation: what matters at a
/// glance is that it happened and who with. It opens in place, in mono, so the
/// quoted voice is visibly not this agent's prose.
private struct AgentMessageRow: View {
    @Environment(\.design) private var design
    let from: String
    let text: String
    let outbound: Bool
    let note: String?
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Button { open.toggle() } label: {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(from)
                        .designFont(.identifier, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(1)
                    if let note {
                        Text(note)
                            .designFont(.mono, design)
                            .foregroundStyle(design.inkFaint.color)
                    }
                    Spacer(minLength: 4)
                    Image(systemName: open ? "chevron.up" : "chevron.down")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(design.inkFaint.color)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            Text(text)
                .designFont(.mono, design)
                .foregroundStyle(design.inkMuted.color)
                .lineLimit(open ? nil : 1)
                .fixedSize(horizontal: false, vertical: open)
        }
        .accessibilityElement(children: .contain)
        .identified(
            "transcript.agent-message", label: "\(from): \(text)",
            value: open ? "open" : "collapsed")
    }
}

/// The shape every rail row that is not special shares: a verb, the one thing
/// it acted on, whatever the layer said about it on the trailing edge, and a
/// second line when there is a reason worth stating.
private struct ActivityRow: View {
    @Environment(\.design) private var design
    /// Which kind of row this is, in the name the screen goes by. The shape is
    /// shared; a refusal, a failure and a file written are not, and anybody
    /// reading the screen — a journey, a person with VoiceOver on — has to be
    /// able to tell which of them is on it.
    let kind: String
    let verb: String
    let subject: String?
    let mono: Bool
    let meta: String?
    var note: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(verb)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                    .fixedSize()
                if let subject {
                    Text(subject)
                        .designFont(mono ? .mono : .body, design)
                        .foregroundStyle(mono ? design.inkMuted.color : design.ink.color)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer(minLength: 4)
                if let meta {
                    Text(meta)
                        .designFont(.mono, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                        .layoutPriority(1)
                }
            }
            if let note {
                Text(note)
                    .designFont(.mono, design)
                    .foregroundStyle(design.inkFaint.color)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityElement(children: .combine)
        .identified(
            "transcript.\(kind)",
            label: [verb, subject, meta, note].compactMap { $0 }.joined(separator: ", "))
    }
}

/// A layer this build cannot read, said plainly.
///
/// The Claude SDK layer reports itself unsupported in this checkout. That is a
/// typed fact from the core, not a failure to load, so the transcript states it
/// as one: the conversation exists, the app knows what it is, and it says what
/// it cannot do rather than showing an empty feed that looks like a bug.
struct UnsupportedLayer: View {
    @Environment(\.design) private var design
    let layer: String

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: "questionmark.square.dashed")
                    .font(.system(size: 15, weight: .medium))
                    .foregroundStyle(design.inkFaint.color)
                Text("This build cannot read \(layer)")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
            }
            Explain(
                "The agent is running and its machine can reach it. What it says "
                + "arrives in a shape this app does not know yet, so nothing is "
                + "shown rather than something guessed at.")
        }
        .padding(design.metrics.rowPadding)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            RoundedRectangle(cornerRadius: design.metrics.cardRadius, style: .continuous)
                .fill(design.sunken.color)
        }
        .padding(.horizontal, design.metrics.gutter)
        .accessibilityElement(children: .combine)
        .identified("transcript.unsupported", label: "This build cannot read \(layer)")
    }
}
