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
    @Environment(\.transcriptTops) private var tops
    let rows: [TranscriptRow]

    var body: some View {
        LazyVStack(alignment: .leading, spacing: 0) {
            ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                TranscriptRowView(
                    row: row,
                    railContinues: index + 1 < rows.count && rows[index + 1].onRail)
                    // Where this entry begins, measured against the feed
                    // rather than against the page. A row does not move
                    // within the feed when the feed is scrolled, so this is
                    // answered once per entry when it is laid out and never
                    // again while somebody reads — which is what makes asking
                    // every entry affordable.
                    .onGeometryChange(for: CGFloat.self) {
                        $0.frame(in: .named(TranscriptTops.space)).minY
                    } action: { tops?.begins(row.id, at: $0) }
            }
        }
        .padding(.horizontal, design.metrics.gutter)
    }
}

/// Where each entry of a transcript begins, kept beside the feed rather than
/// in it.
///
/// A reference rather than view state on purpose. These are answers about the
/// layout that arrive during layout, and writing one into view state would
/// invalidate the feed that was in the middle of being laid out. Nothing draws
/// from this; it is read at the two moments somebody asks where in a
/// transcript the reader is, and told where to put them back.
@MainActor
final class TranscriptTops {
    /// The name the feed answers geometry questions in. Scrolling does not
    /// move a row within the feed, so a position measured in it is stable.
    nonisolated static let space = "transcript.feed"

    private var tops: [String: CGFloat] = [:]

    func begins(_ entry: String, at top: CGFloat) {
        tops[entry] = top
    }

    func top(of entry: String) -> CGFloat? { tops[entry] }

    /// The entry the top of the page is inside: the last one to begin at or
    /// above it. Nothing when the feed has not been laid out yet, or when the
    /// page is above the first entry it has measured.
    func resting(at top: CGFloat) -> TranscriptResting? {
        guard let found = tops
            .filter({ $0.value <= top + 0.5 })
            .max(by: { $0.value < $1.value })
        else { return nil }
        return TranscriptResting(entry: found.key, into: Double(max(0, top - found.value)))
    }
}

private struct TranscriptTopsKey: EnvironmentKey {
    static let defaultValue: TranscriptTops? = nil
}

extension EnvironmentValues {
    var transcriptTops: TranscriptTops? {
        get { self[TranscriptTopsKey.self] }
        set { self[TranscriptTopsKey.self] = newValue }
    }
}

/// The scroll view a transcript lives in.
///
/// A conversation opens at its latest row and follows the tail while a turn
/// streams, which is what a chat does: what just happened is what you are
/// looking at, and a row arriving while you read the tail brings you with it.
/// Only where the list starts and how it reacts to growing are anchored — its
/// alignment is not — so a transcript shorter than the screen stays at the top
/// where it began instead of being pushed down against the composer. A
/// two-row conversation must never open with empty ground above its first row.
///
/// Markdown and lazy rows acquire their heights after the scroll view first
/// lays out. The composer can also change the viewport as its measured height
/// and the safe-area insets arrive. Keep the opening tail attached to those
/// layout changes until the reader takes control. Waiting for every lazy row
/// to report that it finished measuring can wait forever on an offscreen row.
struct TranscriptContainer<Content: View>: View {
    @Environment(\.design) private var design
    /// Where a recording left the reader, to be put back instead of the tail.
    /// Nothing is the ordinary case and the one the app itself always passes:
    /// open at the latest entry and follow it.
    var resting: TranscriptResting?
    /// Told where the reader has come to rest, once they have taken the feed
    /// off its tail. Nothing in the shipping app listens.
    var moved: ((TranscriptResting) -> Void)?
    @ViewBuilder let content: Content
    @State private var position = ScrollPosition()
    @State private var readerMoved = false
    @State private var tops = TranscriptTops()
    @State private var page = TranscriptPage()

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                content
            }
            // One gap under the last row and no more. The composer inset
            // already holds the feed clear of the box, so anything further
            // would open every conversation on a band of empty ground where
            // the newest row should be.
            .padding(.bottom, design.metrics.feedGap)
            .frame(maxWidth: .infinity, alignment: .leading)
            .coordinateSpace(.named(TranscriptTops.space))
        }
        .environment(\.transcriptTops, tops)
        .scrollIndicators(.hidden)
        // A transcript opens at its latest entry and stays there as it grows.
        // One put back where a recording left the reader must not: what is
        // above them is what they were reading, and an entry measuring itself
        // below them is not a reason to be taken to the bottom of the feed.
        .defaultScrollAnchor(resting == nil ? .bottom : .top, for: .initialOffset)
        .defaultScrollAnchor(resting == nil ? .bottom : .top, for: .sizeChanges)
        .scrollPosition($position)
        .onScrollGeometryChange(for: TranscriptLayout.self) { geometry in
            TranscriptLayout(geometry)
        } action: { _, layout in
            // A geometry callback runs while lazy measurements are being
            // applied. Ask after that layout has finished, so the scroll uses
            // the new heights and insets.
            Task { @MainActor in
                guard layout.containerSize.height > 0 else { return }
                if resting != nil { restore() } else if !readerMoved {
                    position.scrollTo(edge: .bottom)
                }
            }
        }
        // Where the page has reached, which the layout above deliberately
        // does not notice. Two questions need it and neither is a reason to
        // move the feed: which entry the reader has stopped on, and how far a
        // restored position still has to go.
        .onScrollGeometryChange(for: TranscriptReach.self) { geometry in
            TranscriptReach(geometry)
        } action: { _, reach in
            page.top = reach.top
            page.offset = reach.offset
            guard resting != nil else { return }
            Task { @MainActor in restore() }
        }
        .onScrollPhaseChange { _, phase in
            if phase == .tracking || phase == .interacting {
                readerMoved = true
            }
            // Where the reader stopped, rather than everywhere they passed
            // through. A transcript is scrolled in one gesture over hundreds
            // of entries, and a recording of every one of them would say
            // nothing a recording of the last one does not.
            if phase == .idle, readerMoved, resting == nil,
               let stopped = tops.resting(at: page.top) {
                moved?(stopped)
            }
        }
        .onChange(of: position.isPositionedByUser) { _, byReader in
            if byReader { readerMoved = true }
        }
    }

    /// Puts the reader back where a recording left them.
    ///
    /// Written as a correction repeated until it lands rather than as one
    /// scroll, because the entry it aims at may not have been laid out yet:
    /// a feed opens at its tail, so an entry the reader had scrolled back to
    /// is often not built at all until something asks for it. Asking for it
    /// by identity builds it; once it has reported where it begins, the
    /// remainder is the difference between where the page is and where it
    /// should be, applied to the last position asked for so that it converges
    /// whatever the scroll view counts a position in.
    ///
    /// The ceiling is what stops a transcript that never settles — one whose
    /// entries keep re-measuring — from correcting itself forever.
    private func restore() {
        guard let resting, page.corrections < TranscriptPage.corrections else { return }
        guard let begins = tops.top(of: resting.entry) else {
            page.corrections += 1
            position.scrollTo(id: resting.entry, anchor: .top)
            return
        }
        let error = begins + resting.into - page.top
        guard abs(error) > 0.5 else { return }
        page.corrections += 1
        page.asked = (page.asked ?? page.offset) + error
        position.scrollTo(y: page.asked ?? 0)
    }
}

/// What the page is doing, kept beside the scroll view rather than in view
/// state.
///
/// Every field here is written from a geometry callback, which runs while the
/// feed is being laid out. Writing one into view state would invalidate the
/// view in the middle of its own layout, and none of these is drawn from.
@MainActor
private final class TranscriptPage {
    /// How many corrections a restored position may take before it is left
    /// where it is. Generous, because the first several are spent asking for
    /// an entry that has not been laid out yet and buy no movement at all.
    static let corrections = 64

    /// Where the top of the page has reached, measured in the feed.
    var top: CGFloat = 0
    /// What the scroll view says its own offset is, which is not the same
    /// number and not in the same units as the feed's.
    var offset: CGFloat = 0
    /// The last position asked for, so a correction adds to what was asked
    /// rather than to what was measured.
    var asked: CGFloat?
    var corrections = 0
}

/// Offset changes are deliberately excluded: moving to the tail must not
/// itself request another scroll, and a reader's movement is not a resize.
private struct TranscriptLayout: Equatable {
    let contentSize: CGSize
    let containerSize: CGSize
    let insets: EdgeInsets

    init(_ geometry: ScrollGeometry) {
        contentSize = geometry.contentSize
        containerSize = geometry.containerSize
        insets = geometry.contentInsets
    }
}

/// How far the page has travelled, in both of the counts that matter.
///
/// `top` is where the top of the visible page has reached measured in the feed,
/// which is the count entries report themselves in, so the two can be compared.
/// `offset` is the scroll view's own, which is neither the same number nor
/// necessarily counted from the same place — it is only ever added to.
private struct TranscriptReach: Equatable {
    let top: CGFloat
    let offset: CGFloat

    init(_ geometry: ScrollGeometry) {
        offset = geometry.contentOffset.y
        top = geometry.contentOffset.y + geometry.contentInsets.top
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
            // An agent can attach things too, through its `attach` tool, and
            // they are elements in the message text exactly as yours are — so
            // they are read out of it the same way and drawn as the same chip.
            AttachedText(text: markdown) { said in
                Prose(markdown: said, open: open)
            }
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
            ActivityRow(
                kind: "wrote", verb: "Wrote", subject: path, mono: true, meta: meta,
                truncation: .head)
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
        case .planVerdict(let verdict):
            PlanVerdictRow(verdict: verdict)
        case .agentMessage(let from, let text, let outbound, let note):
            AgentMessageRow(from: from, text: text, outbound: outbound, note: note)
        case .exit(let text):
            ActivityRow(kind: "exit", verb: text, subject: nil, mono: false, meta: nil)
        case .unreadable(let label):
            ActivityRow(
                kind: "unreadable", verb: "Unreadable", subject: label, mono: true, meta: nil,
                note: "This build cannot read this row.")
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
        case .planVerdict: "list.bullet.rectangle"
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
            AttachedText(text: text) { said in
                Text(said)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
            }
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
/// The agent's markdown, rendered.
///
/// Shared with the ask panel, where a plan is the same markdown asking to be
/// judged rather than reporting what happened.
struct Prose: View {
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
                .thumbTarget(y: 13)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(counts), \(last)")
            .identified(
                "transcript.exploration", label: "\(counts), \(last)",
                value: open ? "open" : "folded")
            .reclaimingThumbTarget(y: 13)
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
    }

    /// "4 reads · 2 searches", and only the halves that happened.
    private var counts: String {
        [reads > 0 ? "\(reads) read\(reads == 1 ? "" : "s")" : nil,
         searches > 0 ? "\(searches) search\(searches == 1 ? "" : "es")" : nil]
            .compactMap { $0 }.joined(separator: " \u{00B7} ")
    }
}

/// A plan that was judged, and the plan itself when somebody asks for it.
///
/// The verdict is one line because that is what a reader scrolling past a
/// finished decision wants; the document folds out underneath because a
/// decision is only reviewable if what was decided about is still there. It is
/// the row's own markdown rather than the file on disk, so reopening an old
/// approval shows the plan as it was approved and not as it has since been
/// rewritten.
///
/// No accent: the accent is this app's one word for something waiting on you,
/// and a plan already answered is the opposite of that. A plan sent back was
/// sent back by the person reading this, which is not news to them.
private struct PlanVerdictRow: View {
    @Environment(\.design) private var design
    let verdict: TranscriptRow.PlanVerdict
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button { open.toggle() } label: {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(headline)
                        .designFont(.body, design)
                        .foregroundStyle(design.ink.color)
                        .fixedSize(horizontal: false, vertical: true)
                    if let note {
                        Text(note)
                            .designFont(.detail, design)
                            .foregroundStyle(design.inkFaint.color)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 4)
                    if verdict.markdown != nil {
                        Image(systemName: open ? "chevron.up" : "chevron.down")
                            .font(.system(size: 12, weight: .medium))
                            .foregroundStyle(design.inkFaint.color)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(verdict.markdown == nil)
            if open, let markdown = verdict.markdown {
                Prose(markdown: markdown, open: false)
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background {
                        RoundedRectangle(
                            cornerRadius: design.metrics.controlRadius, style: .continuous)
                            .fill(design.sunken.color)
                    }
            }
            if let path = verdict.path {
                Text(path)
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
                    .lineLimit(1)
                    .truncationMode(.head)
            }
        }
        .accessibilityElement(children: .contain)
        .identified(
            "transcript.plan", label: headline,
            value: verdict.markdown == nil ? "no document" : (open ? "open" : "folded"))
    }

    private var headline: String {
        switch verdict.decision {
        case .approved: "Plan approved"
        case .sentBack: "Plan sent back"
        }
    }

    /// The layer's own word for why, where it stated one. Never invented: a
    /// plan sent back without a stated reason says nothing rather than
    /// guessing at the person's mind.
    private var note: String? {
        guard case .sentBack(let reason) = verdict.decision else { return nil }
        return reason
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
                .thumbTarget(y: 13)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(from): \(text)")
            .identified(
                "transcript.agent-message", label: "\(from): \(text)",
                value: open ? "open" : "collapsed")
            .reclaimingThumbTarget(y: 13)
            Text(text)
                .designFont(.mono, design)
                .foregroundStyle(design.inkMuted.color)
                .lineLimit(open ? nil : 1)
                .fixedSize(horizontal: false, vertical: open)
        }
        .accessibilityElement(children: .contain)
    }
}

/// One line of a rail row: a verb, the thing it acted on, and what the layer
/// said about that on the trailing edge.
///
/// A stack's own answer to a line that does not fit is a layout priority, and
/// whichever text carries it keeps the width it wants while the other is
/// squeezed to an ellipsis. Neither text here can afford to be the one that
/// loses everything. The meta is whatever the tool printed — a write comes
/// back as a whole sentence, "File created successfully at: …" — and with the
/// width going to it first the path the row exists to name disappears. Giving
/// the path the width instead breaks the other direction: a long command would
/// push off the "exit 1" that says how it went.
///
/// So the subject is served first, and the meta gives up its width until it is
/// down to a third of the contested line; below that the two truncate
/// together. A short meta is therefore never squeezed by a long subject, and a
/// long meta never costs the subject more than two thirds of the line.
struct RowLine: Layout {
    /// Which of the line's three texts a subview is. The line is not a list of
    /// interchangeable views: what each one is decides what happens to it when
    /// the width runs out.
    enum Role: Int {
        case verb, subject, meta
    }

    struct RoleKey: LayoutValueKey {
        static let defaultValue = Role.verb
    }

    /// The most of a contested line the trailing meta may hold.
    static let metaShare: CGFloat = 1.0 / 3.0
    /// Between the verb and the subject, matching the stack this replaced.
    private let spacing: CGFloat = 8
    /// The clear space between the subject and the meta, so that two texts
    /// that both run long still read as two texts.
    private let gap: CGFloat = 20
    /// The clear space a line with nothing on its trailing edge keeps there
    /// anyway, so a subject that runs long stops short of the margin rather
    /// than against it.
    private let trailing: CGFloat = 12

    /// How a line too narrow for both divides between them.
    ///
    /// `content` is what is left once the verb and the gap have been taken
    /// out. Both returned widths are what the text is given to draw in, which
    /// is what decides whether it truncates.
    static func split(
        content: CGFloat, subject: CGFloat, meta: CGFloat
    ) -> (subject: CGFloat, meta: CGFloat) {
        guard content > 0 else { return (0, 0) }
        guard subject + meta > content else { return (subject, meta) }
        let allowed = max(content - subject, content * metaShare)
        let meta = min(meta, allowed)
        return (min(subject, content - meta), meta)
    }

    func sizeThatFits(
        proposal: ProposedViewSize, subviews: Subviews, cache: inout ()
    ) -> CGSize {
        let widths = widths(subviews, within: proposal.width)
        let line = widths.values.reduce(0, +) + fixed(subviews)
        let heights = heights(subviews, widths: widths)
        return CGSize(
            width: proposal.width.map { $0.isFinite ? $0 : line } ?? line,
            height: heights.ascent + heights.descent)
    }

    func placeSubviews(
        in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()
    ) {
        let widths = widths(subviews, within: bounds.width)
        let heights = heights(subviews, widths: widths)
        var leading = bounds.minX
        for (index, subview) in subviews.enumerated() {
            guard let width = widths[index] else { continue }
            let role = subview[RoleKey.self]
            // The meta hangs off the trailing edge the way the stack's spacer
            // used to put it there; everything else runs from the leading one.
            let x = role == .meta ? bounds.maxX - width : leading
            if role != .meta { leading = x + width + spacing }
            let size = ProposedViewSize(width: width, height: nil)
            let baseline = subview.dimensions(in: size)[.firstTextBaseline]
            subview.place(
                at: CGPoint(x: x, y: bounds.minY + heights.ascent - baseline),
                anchor: .topLeading, proposal: size)
        }
    }

    /// What each subview is given to draw in, by its index.
    private func widths(_ subviews: Subviews, within available: CGFloat?) -> [Int: CGFloat] {
        var ideal: [Role: (index: Int, width: CGFloat)] = [:]
        for (index, subview) in subviews.enumerated() {
            ideal[subview[RoleKey.self]] = (index, subview.sizeThatFits(.unspecified).width)
        }
        var widths = ideal.values.reduce(into: [Int: CGFloat]()) { $0[$1.index] = $1.width }
        guard let available, available.isFinite else { return widths }
        let verb = ideal[.verb]?.width ?? 0
        switch (ideal[.subject], ideal[.meta]) {
        case let (.some(subject), .some(meta)):
            let content = max(0, available - verb - spacing - gap)
            let share = Self.split(content: content, subject: subject.width, meta: meta.width)
            widths[subject.index] = share.subject
            widths[meta.index] = share.meta
        case let (.some(subject), .none):
            widths[subject.index] = min(
                subject.width, max(0, available - verb - spacing - trailing))
        case let (.none, .some(meta)):
            widths[meta.index] = min(meta.width, max(0, available - verb - gap))
        case (.none, .none):
            break
        }
        return widths
    }

    /// The width the line spends on space rather than on text.
    private func fixed(_ subviews: Subviews) -> CGFloat {
        let roles = Set(subviews.map { $0[RoleKey.self] })
        return (roles.contains(.subject) ? spacing : 0)
            + (roles.contains(.meta) ? gap : trailing)
    }

    /// Where the shared baseline sits, and how far below it the line reaches.
    private func heights(
        _ subviews: Subviews, widths: [Int: CGFloat]
    ) -> (ascent: CGFloat, descent: CGFloat) {
        var ascent: CGFloat = 0
        var descent: CGFloat = 0
        for (index, subview) in subviews.enumerated() {
            let size = ProposedViewSize(width: widths[index], height: nil)
            let dimensions = subview.dimensions(in: size)
            let baseline = dimensions[.firstTextBaseline]
            ascent = max(ascent, baseline)
            descent = max(descent, dimensions.height - baseline)
        }
        return (ascent, descent)
    }
}

extension View {
    /// Says which of a row line's three texts this one is.
    func rowLine(_ role: RowLine.Role) -> some View {
        layoutValue(key: RowLine.RoleKey.self, value: role)
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
    /// Which end of the subject is worth keeping when it is too long for the
    /// line. A command is read from its front; a path is identified by its
    /// last component, so a written file drops its leading directories rather
    /// than the name of the file it wrote.
    var truncation: Text.TruncationMode = .middle

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            RowLine {
                Text(verb)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                    .rowLine(.verb)
                if let subject {
                    Text(subject)
                        .designFont(mono ? .mono : .body, design)
                        .foregroundStyle(mono ? design.inkMuted.color : design.ink.color)
                        .lineLimit(1)
                        .truncationMode(truncation)
                        .rowLine(.subject)
                }
                if let meta {
                    Text(meta)
                        .designFont(.mono, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                        .rowLine(.meta)
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
/// A provider this build cannot read, including a conversation reached through
/// an old link. Its identity is retained instead of drawing an empty feed.
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
                "Update the app to read this agent’s conversation.")
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
