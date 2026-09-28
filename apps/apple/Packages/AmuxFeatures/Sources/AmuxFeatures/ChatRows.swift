import AmuxCore
import AmuxDesign
import ImageIO
import SwiftUI

/// One chat row, drawn from its value.
///
/// Work rows hang off a rail with a glyph, a verb, a subject in mono and
/// meta on the trailing edge; the person's prompts sit in a bubble; the
/// agent's prose takes the full width; session events are rules and quiet
/// footers. The accent is kept for rows an open ask points at and for
/// failures. A row that opens says so with a chevron and opens in place.
public struct ChatRowView: View {
    @Environment(\.design) private var design
    let row: Row
    let expanded: Bool
    let bytes: (BlobRef) -> Data?
    let toggle: () -> Void

    public init(
        row: Row, expanded: Bool, bytes: @escaping (BlobRef) -> Data? = { _ in nil },
        toggle: @escaping () -> Void = {}
    ) {
        self.row = row
        self.expanded = expanded
        self.bytes = bytes
        self.toggle = toggle
    }

    public var body: some View {
        content
            .padding(.leading, row.parent == nil ? 0 : 20)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.bottom, 12)
    }

    @ViewBuilder
    private var content: some View {
        if let run = row.run, run.isSummary, !expanded {
            RailRow(
                kind: "run", glyph: "magnifyingglass", verb: String(localized: "Explored"),
                subject: run.anchor, meta: ChatWords.run(run), truncation: .head,
                opens: true, open: false, toggle: toggle)
        } else {
            kindView
        }
    }

    @ViewBuilder
    private var kindView: some View {
        switch row.kind {
        case .prompt(let text, let steered):
            PromptBubble(text: text, steered: steered, bytes: bytes)
        case .prose(let text, let streaming, let workingNote):
            ProseRow(text: text, streaming: streaming, workingNote: workingNote)
        case .thinking(let text, let open, let durationMs):
            RailRow(
                kind: "thinking", glyph: "ellipsis", verb: ChatWords.thinking(
                    open: open, durationMs: durationMs),
                quiet: true, detail: expanded && !text.isEmpty ? text : nil,
                opens: !text.isEmpty, open: expanded, toggle: toggle)
        case .toolCall(let server, let tool, let fact, let state, let result):
            let verb = ChatWords.verb(
                state, row, wants: String(localized: "Wants to use"),
                doing: String(localized: "Using"), done: String(localized: "Used"))
            RailRow(
                kind: "tool", glyph: glyph(state, done: "wrench.adjustable"), verb: verb,
                subject: server.isEmpty ? tool : "\(server) · \(tool)",
                meta: ChatWords.meta(
                    [fact, state == .failed ? String(localized: "failed") : ""], row, verb: verb),
                accented: accented(state),
                detail: expanded && !result.isEmpty ? result : nil,
                opens: !result.isEmpty, open: expanded, toggle: toggle)
        case .fileChange(let files, let state):
            FileChangeRow(row: row, files: files, state: state, accented: accented(state))
        case .command(let command, let state, let outputHead, let moreLines, let durationMs, let exitCode):
            let verb = ChatWords.verb(
                state, row, wants: String(localized: "Wants to run"),
                doing: String(localized: "Running"), done: String(localized: "Ran"))
            let meta = commandMeta(state, exitCode: exitCode, durationMs: durationMs)
            RailRow(
                kind: "command", glyph: glyph(state, done: "chevron.left.forwardslash.chevron.right"),
                verb: verb, subject: ChatWords.firstLine(command),
                meta: ChatWords.meta(meta, row, verb: verb), accented: accented(state),
                output: outputHead, more: moreLines, failed: state == .failed)
        case .explore(let verb, let subject, let state):
            RailRow(
                kind: "explore", glyph: glyph(state, done: "magnifyingglass"),
                verb: ChatWords.explore(verb), subject: subject,
                meta: [ChatWords.meta([ChatWords.state(state) ?? ""], row),
                       row.run.map { expanded && $0.isSummary ? ChatWords.run($0) : "" } ?? ""]
                    .filter { !$0.isEmpty }.joined(separator: " · "),
                accented: accented(state), truncation: .head,
                opens: row.run?.isSummary == true, open: expanded, toggle: toggle)
        case .subagent(let description, let running, let toolCount, let lastTool, let answer, let durationMs):
            RailRow(
                kind: "subagent", glyph: "arrow.triangle.branch", verb: String(localized: "Agent"),
                subject: ChatWords.firstLine(description),
                meta: ChatWords.subagent(toolCount: toolCount, durationMs: durationMs),
                accented: false, live: running,
                note: running && !lastTool.isEmpty ? "└ \(lastTool)" : nil,
                detail: !running && !answer.isEmpty ? answer : nil,
                detailLines: expanded ? nil : 2,
                opens: true, open: expanded, toggle: toggle)
        case .background(let command, let running):
            RailRow(
                kind: "background", glyph: "clock", verb: String(localized: "In background"),
                subject: ChatWords.firstLine(command),
                meta: running ? String(localized: "running") : String(localized: "finished"))
        case .image(let path, let generated, let image):
            ImageRow(path: path, generated: generated, image: image, bytes: bytes)
        case .slashOutput(let command, let args, let output):
            RailRow(
                kind: "slash", glyph: "slash.circle", verb: "",
                subject: args.isEmpty ? command : "\(command) \(args)",
                detail: output.isEmpty ? nil : output, detailLines: expanded ? nil : 6,
                opens: true, open: expanded, toggle: toggle)
        case .ask(let ask):
            AskRowView(ask: ask, expanded: expanded, toggle: toggle)
        case .turnEnd(let failed, let costUsd, let durationMs):
            Footer(
                kind: "turn-end",
                text: ChatWords.turnEnd(durationMs: durationMs, costUsd: costUsd, failed: failed),
                accented: failed)
        case .stopped:
            Footer(kind: "stopped", text: String(localized: "You stopped it"))
        case .compaction(let automatic, let after, let before):
            FeedRule(
                kind: "compaction",
                label: ChatWords.compaction(before: before, after: after, automatic: automatic))
        case .error(let errorKind, let message, let attempts, let gaveUp):
            RailRow(
                kind: "error", glyph: "exclamationmark.triangle", verb: "",
                subject: errorKind.isEmpty ? ChatWords.firstLine(message) : errorKind,
                meta: gaveUp && attempts > 1 ? String(localized: "gave up after \(attempts) tries") : "",
                accented: true, mono: false,
                detail: !errorKind.isEmpty && !message.isEmpty ? message : nil,
                detailLines: expanded ? nil : 3, opens: !errorKind.isEmpty && !message.isEmpty,
                open: expanded, toggle: toggle)
        case .modelSwitch(_, let to, let reason):
            RailRow(
                kind: "model-switch", glyph: "arrow.left.arrow.right",
                verb: String(localized: "Switched to"), subject: to, meta: reason)
        case .boundary(let kind, let cause):
            FeedRule(kind: "boundary", label: ChatWords.boundary(kind, cause: cause))
        case .agentMessage(let from, let kind, let text, let to, let sent, let rejection):
            RailRow(
                kind: "agent-message", glyph: to.isEmpty ? "arrow.turn.down.left" : "arrow.turn.up.right",
                verb: to.isEmpty ? String(localized: "From") : String(localized: "To"),
                subject: to.isEmpty ? from : to,
                meta: messageMeta(kind, sent, rejection), accented: sent == .rejected,
                detail: text.trimmingCharacters(in: .whitespacesAndNewlines),
                detailLines: expanded ? nil : 1, detailInk: true,
                opens: true, open: expanded, toggle: toggle)
        case .autoReview(let decision, let risk, let rationale, _):
            RailRow(
                kind: "auto-review", glyph: "checkmark.shield",
                verb: String(localized: "Auto-reviewed"), subject: decision,
                meta: risk.isEmpty ? "" : String(localized: "\(risk) risk"),
                detail: expanded && !rationale.isEmpty ? rationale : nil,
                opens: !rationale.isEmpty, open: expanded, toggle: toggle)
        case .unrecognized(let what, let summary):
            RailRow(
                kind: "unrecognized", glyph: "questionmark.square.dashed",
                verb: String(localized: "Unreadable"), subject: what,
                meta: ChatWords.firstLine(summary))
        case .hidden:
            EmptyView()
        }
    }

    private func glyph(_ state: ToolStateView, done: String) -> String {
        switch state {
        case .pending, .running: row.attention ? "hand.raised" : done
        case .succeeded: done
        case .failed: "xmark"
        case .denied, .cancelled: "nosign"
        }
    }

    /// "exit 101 · 4.2s": a failure names its code where the agent gives one.
    private func commandMeta(_ state: ToolStateView, exitCode: Int32?, durationMs: Int64?) -> [String] {
        var meta: [String] = []
        if let exitCode, exitCode != 0 {
            meta.append(String(localized: "exit \(exitCode)"))
        } else if exitCode == nil, state == .failed {
            meta.append(String(localized: "failed"))
        }
        if let durationMs { meta.append(ChatWords.duration(durationMs)) }
        return meta
    }

    private func accented(_ state: ToolStateView) -> Bool {
        row.attention || state == .failed
    }

    private func messageMeta(_ kind: EnvelopeKind, _ sent: SendState, _ rejection: String) -> String {
        switch (kind, sent) {
        case (.finished, _): String(localized: "finished")
        case (.failed, _): String(localized: "failed")
        case (_, .sending): String(localized: "sending")
        case (_, .rejected): String(localized: "not delivered · \(rejection)")
        default: ""
        }
    }
}

/// The rail: a glyph column, then a verb, a subject in mono, meta on the
/// trailing edge, and what the row opens to beneath.
struct RailRow: View {
    @Environment(\.design) private var design
    let kind: String
    let glyph: String
    let verb: String
    var subject: String = ""
    var meta: String = ""
    var accented = false
    var mono = true
    var quiet = false
    /// A subagent still under way.
    var live = false
    var note: String?
    var output: [String] = []
    var more: UInt = 0
    var failed = false
    var detail: String?
    /// How many lines of the detail show; all of it when nil.
    var detailLines: Int?
    var detailInk = false
    /// Which end of a long mono subject is kept: a path keeps its file name. A prose
    /// subject always keeps its opening words, so it still reads as a sentence.
    var truncation: Text.TruncationMode = .middle
    var opens = false
    var open = false
    var toggle: () -> Void = {}

    var body: some View {
        Group {
            if opens {
                // A one-line row is under 20 pt: its target reaches into the
                // gaps around it, which leaves the chat drawn as it was.
                Button(action: toggle) { layout.thumbTarget(y: RailRow.reach) }
                    .buttonStyle(.amuxRow)
            } else {
                layout
            }
        }
        .accessibilityElement(children: .combine)
        .identified(
            "chat.row.\(kind)",
            label: [verb, subject, meta].filter { !$0.isEmpty }.joined(separator: ", "),
            value: opens ? (open ? "open" : "folded") : nil)
        .reclaimingThumbTarget(y: opens ? RailRow.reach : 0)
    }

    private static let reach: CGFloat = 14

    @ViewBuilder private var verbText: some View {
        if !verb.isEmpty {
            Text(verb)
                .designFont(quiet ? .detail : .bodyEmphasis, design)
                .italic(quiet)
                .foregroundStyle(quiet ? design.inkFaint.color : design.ink.color)
                .fixedSize()
        }
    }

    @ViewBuilder private var subjectText: some View {
        if !subject.isEmpty {
            Text(subject)
                .designFont(mono ? .monoSmall : .detail, design)
                .foregroundStyle(design.inkMuted.color)
                .truncationMode(mono ? truncation : .tail)
        }
    }

    @ViewBuilder private var metaText: some View {
        if !meta.isEmpty {
            Text(meta)
                .designFont(.caption, design)
                .foregroundStyle(accented ? design.accent.color : design.inkFaint.color)
                .lineLimit(1)
                .fixedSize()
                // Usually a measured duration: a compared screenshot masks it.
                .reported("chat.row.meta.volatile")
        }
    }

    @ViewBuilder private var chevron: some View {
        if opens {
            Image(systemName: open ? "chevron.up" : "chevron.down")
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(design.inkFaint.color)
        }
    }

    private var layout: some View {
        HStack(alignment: .firstTextBaseline, spacing: 9) {
            Image(systemName: glyph)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(accented ? design.accent.color : design.inkFaint.color)
                .frame(width: 18)
                .opacity(live ? 0.9 : 1)
            VStack(alignment: .leading, spacing: 3) {
                ViewThatFits(in: .horizontal) {
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        verbText
                        subjectText.lineLimit(1)
                        Spacer(minLength: 4)
                        metaText
                        chevron
                    }
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        verbText
                        VStack(alignment: .leading, spacing: 2) {
                            subjectText.lineLimit(1)
                            metaText
                        }
                        Spacer(minLength: 4)
                        chevron
                    }
                }
                if let note {
                    Text(note)
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                }
                if !output.isEmpty {
                    VStack(alignment: .leading, spacing: 1) {
                        ForEach(Array(output.enumerated()), id: \.offset) { _, line in
                            Text(line)
                                .designFont(.monoSmall, design)
                                .foregroundStyle(failed ? design.accent.color : design.inkFaint.color)
                                .lineLimit(1)
                        }
                        if more > 0 {
                            Text(String(localized: "··· \(more) more lines"))
                                .designFont(.monoSmall, design)
                                .foregroundStyle(design.inkFaint.color)
                        }
                    }
                }
                if let detail {
                    Text(detail)
                        .designFont(detailInk ? .detail : .monoSmall, design)
                        .foregroundStyle(detailInk ? design.ink.color : design.inkMuted.color)
                        .lineLimit(detailLines)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
            }
        }
        .contentShape(Rectangle())
    }
}

/// What the person said, in a bubble set in from the leading edge.
struct PromptBubble: View {
    @Environment(\.design) private var design
    let text: [Segment]
    var steered = false
    var bytes: (BlobRef) -> Data? = { _ in nil }

    var body: some View {
        HStack {
            Spacer(minLength: 44)
            VStack(alignment: .trailing, spacing: 6) {
                SegmentsView(segments: text, bytes: bytes)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .background {
                        RoundedRectangle(
                            cornerRadius: design.metrics.controlRadius + 3, style: .continuous)
                        .fill(design.sunken.color)
                    }
                if steered {
                    Text("steered")
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                }
            }
        }
        .accessibilityElement(children: .combine)
        .identified(
            "chat.row.prompt", label: ChatWords.text(of: text), value: steered ? "steered" : nil)
    }
}

/// Text with attachment chips at their places.
struct SegmentsView: View {
    @Environment(\.design) private var design
    let segments: [Segment]
    var bytes: (BlobRef) -> Data? = { _ in nil }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(segments.enumerated()), id: \.offset) { _, segment in
                switch segment {
                case .text(let text):
                    Text(text)
                        .designFont(.body, design)
                        .foregroundStyle(design.ink.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                case .attachment(let view):
                    AttachmentChip(view: view, bytes: bytes)
                }
            }
        }
    }
}

/// An attachment as the composer and the rows show it: its name and size,
/// and a photo's thumbnail once its bytes are here.
public struct AttachmentChip: View {
    @Environment(\.design) private var design
    let view: AttachmentView
    let bytes: (BlobRef) -> Data?
    var remove: (() -> Void)?

    public init(
        view: AttachmentView, bytes: @escaping (BlobRef) -> Data? = { _ in nil },
        remove: (() -> Void)? = nil
    ) {
        self.view = view
        self.bytes = bytes
        self.remove = remove
    }

    public var body: some View {
        HStack(spacing: 7) {
            if case .image(let blob) = view, let image = Thumbnail.image(bytes(blob)) {
                image.resizable().scaledToFill()
                    .frame(width: 28, height: 28)
                    .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
            } else {
                Image(systemName: ChatWords.chipGlyph(view))
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(design.inkMuted.color)
            }
            Text(ChatWords.chip(view))
                .designFont(.caption, design)
                .foregroundStyle(design.inkMuted.color)
                .lineLimit(1)
                .truncationMode(.middle)
            if let remove {
                Button(action: remove) {
                    Image(systemName: "xmark")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(design.inkFaint.color)
                        .thumbTarget(x: 10, y: 10)
                }
                .buttonStyle(.amuxControl)
                .accessibilityLabel("Remove")
                .identified("chat.attachment.remove", label: "Remove")
                .reclaimingThumbTarget(x: 10, y: 10)
            }
        }
        .padding(.horizontal, 9)
        .padding(.vertical, 5)
        .background {
            Capsule().fill(design.raised.color)
                .overlay(Capsule().strokeBorder(design.hairline.color, lineWidth: 1))
        }
        .identified("chat.attachment", label: ChatWords.chip(view))
    }
}

/// Decodes an image without reaching for a platform image type.
enum Thumbnail {
    static func image(_ data: Data?) -> Image? {
        guard let data, let source = CGImageSourceCreateWithData(data as CFData, nil),
              let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                  kCGImageSourceCreateThumbnailFromImageAlways: true,
                  kCGImageSourceThumbnailMaxPixelSize: 900,
                  kCGImageSourceCreateThumbnailWithTransform: true,
              ] as CFDictionary)
        else { return nil }
        return Image(decorative: image, scale: 1)
    }
}

/// What the agent said, as markdown, parsed away from the main thread.
struct ProseRow: View {
    @Environment(\.design) private var design
    let text: [Segment]
    let streaming: Bool
    let workingNote: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(text.enumerated()), id: \.offset) { _, segment in
                switch segment {
                case .text(let markdown):
                    Prose(markdown: markdown, muted: workingNote)
                case .attachment(let view):
                    AttachmentChip(view: view)
                }
            }
        }
        .identified(
            "chat.row.prose", label: ChatWords.text(of: text),
            value: streaming ? "streaming" : (workingNote ? "working-note" : "final"))
    }
}

/// Markdown, rendered: headings, paragraphs, lists, quotes, rules, code
/// and tables that scroll sideways.
struct Prose: View {
    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    let markdown: String
    var muted = false
    @State private var document: MarkdownDocument?
    @State private var parsed: String?

    /// What is drawn: the parsed document, or in front of a camera, which
    /// cannot wait for the parse, the same document parsed on the spot.
    private var blocks: [MarkdownBlock] {
        if parsed == markdown, let document { return document.blocks }
        if photographed { return MarkdownDocument.parse(markdown).blocks }
        return document?.blocks ?? []
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                MarkdownBlockView(block: block, muted: muted)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .tint(design.accent.color)
        .task(id: markdown) {
            let source = markdown
            let document = await Task.detached(priority: .userInitiated) {
                MarkdownDocument.parse(source)
            }.value
            guard !Task.isCancelled else { return }
            self.document = document
            parsed = source
        }
        .reported(
            "chat.prose.render", value: parsed == markdown || photographed ? "rendered" : "pending")
    }
}

private struct MarkdownBlockView: View {
    @Environment(\.design) private var design
    let block: MarkdownBlock
    let muted: Bool

    private var ink: Color { muted ? design.inkMuted.color : design.ink.color }

    var body: some View {
        switch block {
        case .heading(let level, let text):
            Text(inline(text))
                .font(heading(level))
                .foregroundStyle(ink)
                .fixedSize(horizontal: false, vertical: true)
        case .paragraph(let text):
            Text(inline(text))
                .designFont(.body, design)
                .foregroundStyle(ink)
                .lineSpacing(4)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        case .list(_, let items):
            // Each item is its own row so a wrapped line hangs under the item's text, not
            // under its marker.
            VStack(alignment: .leading, spacing: 6) {
                ForEach(items) { item in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(item.marker)
                            .foregroundStyle(design.inkFaint.color)
                        Text(inline(item.text))
                            .foregroundStyle(ink)
                            .lineSpacing(4)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.leading, CGFloat(item.depth) * 16)
                }
            }
            .designFont(.body, design)
        case .code(let language, let text):
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
                let shape = RoundedRectangle(
                    cornerRadius: design.metrics.controlRadius, style: .continuous)
                shape.fill(design.sunken.color)
                    .overlay(shape.strokeBorder(design.hairline.color, lineWidth: 1))
            }
        case .quote(let lines):
            HStack(alignment: .top, spacing: 10) {
                Capsule().fill(design.inkFaint.color.opacity(0.45)).frame(width: 2.5)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                        Text(inline(line))
                            .designFont(.body, design)
                            .foregroundStyle(design.inkMuted.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            .fixedSize(horizontal: false, vertical: true)
        case .table(let header, let rows):
            ScrollView(.horizontal) {
                VStack(alignment: .leading, spacing: 0) {
                    tableLine(header, emphasis: true)
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                        Rectangle().fill(design.hairline.color)
                            .frame(height: design.metrics.hairline)
                        tableLine(row, emphasis: false)
                    }
                }
                .background {
                    let shape = RoundedRectangle(
                        cornerRadius: design.metrics.controlRadius, style: .continuous)
                    shape.fill(design.sunken.color.opacity(0.6))
                        .overlay(shape.strokeBorder(design.hairline.color, lineWidth: 1))
                }
            }
            .scrollIndicators(.hidden)
        case .rule:
            Rectangle().fill(design.hairline.color).frame(height: design.metrics.hairline)
        }
    }

    private func tableLine(_ cells: [AttributedString], emphasis: Bool) -> some View {
        HStack(alignment: .top, spacing: 0) {
            ForEach(Array(cells.enumerated()), id: \.offset) { index, cell in
                Text(inline(cell))
                    .designFont(emphasis ? .caption : .monoSmall, design)
                    .foregroundStyle(emphasis ? design.inkMuted.color : design.ink.color)
                    .frame(width: index == 0 ? 146 : 128, alignment: .leading)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
            }
        }
    }

    private func inline(_ source: AttributedString) -> AttributedString {
        var text = source
        for run in text.runs {
            if run.inlinePresentationIntent == .code {
                text[run.range].font = design.font(.mono)
                text[run.range].foregroundColor = design.inkMuted.color
            }
            if run.link != nil {
                text[run.range].foregroundColor = design.accent.color
                text[run.range].underlineStyle = .single
            }
        }
        return text
    }

    private func heading(_ level: Int) -> Font {
        BundledFonts.register()
        let size: CGFloat = level == 1 ? 21 : (level == 2 ? 18 : 15.5)
        return .custom(design.faces.display, size: size, relativeTo: level == 1 ? .title3 : .headline)
            .weight(.semibold)
    }
}

/// Edited, created, deleted and moved files: one line each.
private struct FileChangeRow: View {
    let row: Row
    let files: [FileRow]
    let state: ToolStateView
    let accented: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if files.isEmpty {
                RailRow(
                    kind: "file-change", glyph: "pencil", verb: String(localized: "Editing"),
                    meta: ChatWords.meta([], row), accented: accented)
            }
            ForEach(Array(files.enumerated()), id: \.offset) { index, file in
                let (verb, subject, meta) = words(file)
                RailRow(
                    kind: "file-change", glyph: glyph, verb: verb, subject: subject,
                    meta: index == 0
                        ? ChatWords.meta([meta, ChatWords.state(state) ?? ""], row)
                        : meta,
                    accented: accented, truncation: .head)
            }
        }
    }

    private var glyph: String {
        switch state {
        case .failed: "xmark"
        case .denied, .cancelled: "nosign"
        case .pending, .running: row.attention ? "hand.raised" : "pencil"
        case .succeeded: "pencil"
        }
    }

    private func words(_ file: FileRow) -> (String, String, String) {
        switch file.change {
        case .edited:
            (String(localized: "Edited"), file.path, "+\(file.added) −\(file.removed)")
        case .created(let lines):
            (String(localized: "Created"), file.path, String(localized: "\(lines) lines"))
        case .deleted: (String(localized: "Deleted"), file.path, "")
        case .moved(let to): (String(localized: "Moved"), "\(file.path) → \(to)", "")
        }
    }
}

/// An image the agent read or made: its thumbnail once the bytes are here.
private struct ImageRow: View {
    @Environment(\.design) private var design
    let path: String
    let generated: Bool
    let image: BlobRef?
    let bytes: (BlobRef) -> Data?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            RailRow(
                kind: "image", glyph: "photo",
                verb: generated ? String(localized: "Generated image") : String(localized: "Image"),
                subject: path.isEmpty ? image?.name ?? "" : path,
                meta: image.map { ChatWords.bytes($0.size) } ?? "")
            if let image, let picture = Thumbnail.image(bytes(image)) {
                picture.resizable().scaledToFit()
                    .frame(maxWidth: 240, maxHeight: 240, alignment: .leading)
                    .clipShape(RoundedRectangle(cornerRadius: design.metrics.controlRadius))
                    .padding(.leading, 27)
            }
        }
    }
}

/// A rule across the chat with what it marks written into it.
private struct FeedRule: View {
    @Environment(\.design) private var design
    let kind: String
    let label: String

    var body: some View {
        HStack(spacing: 8) {
            Rectangle().fill(design.hairline.color).frame(width: 14, height: design.metrics.hairline)
            // The label wins the width over the trailing rule, which only fills what is left.
            Text(label)
                .designFont(.caption, design)
                .foregroundStyle(design.inkFaint.color)
                .lineLimit(1)
                .layoutPriority(1)
            Rectangle().fill(design.hairline.color).frame(height: design.metrics.hairline)
        }
        .accessibilityElement(children: .combine)
        .identified("chat.row.\(kind)", label: label)
    }
}

/// A quiet line under the last reply.
private struct Footer: View {
    @Environment(\.design) private var design
    let kind: String
    let text: String
    var accented = false

    var body: some View {
        Text(text)
            .designFont(.caption, design)
            .foregroundStyle(accented ? design.accent.color : design.inkFaint.color)
            .padding(.leading, 27)
            .identified("chat.row.\(kind)", label: text)
    }
}

/// An ask that is the work, resolved in place: questions and their answers,
/// a plan and its verdict, a form, a link, an access grant, or a dialog this
/// build could not read.
private struct AskRowView: View {
    @Environment(\.design) private var design
    let ask: AskRow
    let expanded: Bool
    let toggle: () -> Void

    var body: some View {
        switch ask {
        case .question(let questions, let answers, let resolution, let note),
             .questions(let questions, let answers, let resolution, let note):
            questionsView(questions, answers, resolution, note)
        case .plan(let plan, let verdict, let note):
            VStack(alignment: .leading, spacing: 8) {
                RailRow(
                    kind: "plan", glyph: planGlyph(verdict), verb: planVerb(verdict),
                    subject: verdict == .sentBack && note != nil
                        ? "“\(ChatWords.firstLine(note ?? ""))”"
                        : planTitle(plan),
                    accented: verdict == .open, mono: false, opens: true, open: expanded,
                    toggle: toggle)
                if expanded {
                    Prose(markdown: plan).padding(.leading, 27)
                }
            }
        case .form(let server, let message, let fields, let resolution):
            RailRow(
                kind: "form", glyph: resolutionGlyph(resolution),
                verb: formVerb(resolution, count: fields.count, server: server),
                subject: resolution == .answered ? server : "",
                accented: resolution == .open,
                detail: expanded && !message.isEmpty ? message : nil,
                opens: !message.isEmpty, open: expanded, toggle: toggle)
        case .link(let server, let message, let url, let resolution):
            RailRow(
                kind: "link", glyph: resolutionGlyph(resolution),
                verb: resolution == .answered
                    ? String(localized: "Opened link from")
                    : resolution == .open
                        ? String(localized: "Link from")
                        : ChatWords.resolution(resolution, answered: ""),
                subject: server, accented: resolution == .open,
                detail: expanded ? "\(message)\n\(url)" : nil,
                opens: true, open: expanded, toggle: toggle)
        case .grant(let reason, let read, let write, let network, let hosts, let resolution, let granted):
            let asked = access(read: read, write: write, network: network, hosts: hosts)
            RailRow(
                kind: "grant", glyph: resolutionGlyph(resolution),
                verb: resolution == .answered && granted != nil
                    ? String(localized: "Granted")
                    : resolution == .open
                        ? String(localized: "Wants access")
                        : ChatWords.resolution(resolution, answered: String(localized: "Denied")),
                subject: granted.map {
                    access(read: $0.read, write: $0.write, network: $0.network, hosts: hosts)
                } ?? asked,
                meta: granted.map {
                    $0.forSession ? String(localized: "this session") : String(localized: "this turn")
                } ?? "",
                accented: resolution == .open,
                detail: expanded && !reason.isEmpty ? reason : nil,
                opens: !reason.isEmpty, open: expanded, toggle: toggle)
        case .unanswerable(let reason, let resolution):
            RailRow(
                kind: "unanswerable", glyph: resolutionGlyph(resolution),
                verb: resolution == .open
                    ? String(localized: "Can’t answer this here")
                    : String(localized: "\(ChatWords.resolution(resolution, answered: String(localized: "Answered"))) a dialog this build can’t read"),
                accented: resolution == .open,
                detail: expanded && !reason.isEmpty ? reason : nil,
                opens: !reason.isEmpty, open: expanded, toggle: toggle)
        }
    }

    @ViewBuilder
    private func questionsView(
        _ questions: [QuestionView], _ answers: [AnswerView], _ resolution: Resolution,
        _ note: String?
    ) -> some View {
        let verb = ChatWords.resolution(resolution, answered: String(localized: "Answered"))
        VStack(alignment: .leading, spacing: 6) {
            if questions.count == 1, let question = questions.first {
                let answer = answers.first.map(ChatWords.answer) ?? ""
                RailRow(
                    kind: "question", glyph: resolutionGlyph(resolution), verb: verb,
                    subject: question.question, accented: resolution == .open, mono: false)
                if !answer.isEmpty { pill(answer).padding(.leading, 27) }
            } else {
                RailRow(
                    kind: "question", glyph: resolutionGlyph(resolution), verb: verb,
                    subject: String(localized: "\(questions.count) questions"),
                    accented: resolution == .open, mono: false)
                ForEach(Array(zip(questions, answers).enumerated()), id: \.offset) { _, pair in
                    VStack(alignment: .leading, spacing: 3) {
                        Text(pair.0.header.isEmpty ? pair.0.question : pair.0.header)
                            .designFont(.caption, design)
                            .foregroundStyle(design.inkFaint.color)
                        pill(ChatWords.answer(pair.1))
                    }
                    .padding(.leading, 27)
                }
            }
            if let note {
                Text(String(localized: "Note: \(ChatWords.firstLine(note))"))
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
                    .padding(.leading, 27)
            }
        }
    }

    private func pill(_ text: String) -> some View {
        Text(text)
            .designFont(.detail, design)
            .foregroundStyle(design.ink.color)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(Capsule().fill(design.sunken.color))
    }

    private func access(read: [String], write: [String], network: Bool, hosts: [String]) -> String {
        var parts: [String] = []
        if !write.isEmpty { parts.append(String(localized: "write \(write.joined(separator: ", "))")) }
        if !read.isEmpty { parts.append(String(localized: "read \(read.joined(separator: ", "))")) }
        if network {
            parts.append(hosts.isEmpty
                ? String(localized: "network")
                : String(localized: "network \(hosts.joined(separator: ", "))"))
        }
        return parts.joined(separator: " · ")
    }

    private func resolutionGlyph(_ resolution: Resolution) -> String {
        switch resolution {
        case .open: "hand.raised"
        case .answered: "checkmark"
        case .declined, .cancelled, .dismissed: "nosign"
        }
    }

    private func formVerb(_ resolution: Resolution, count: Int, server: String) -> String {
        switch resolution {
        case .answered:
            count == 1
                ? String(localized: "Sent 1 field to")
                : String(localized: "Sent \(count) fields to")
        case .open: String(localized: "\(server) needs details")
        default:
            String(localized: "\(ChatWords.resolution(resolution, answered: "")) \(server)’s form")
        }
    }

    private func planVerb(_ verdict: PlanVerdict) -> String {
        switch verdict {
        case .open: String(localized: "Plan proposed")
        case .approved: String(localized: "Plan approved")
        case .sentBack: String(localized: "Plan sent back")
        case .dismissed: String(localized: "Plan dismissed")
        }
    }

    private func planGlyph(_ verdict: PlanVerdict) -> String {
        switch verdict {
        case .open: "hand.raised"
        case .approved: "checkmark"
        case .sentBack: "arrow.uturn.left"
        case .dismissed: "nosign"
        }
    }

    private func planTitle(_ plan: String) -> String {
        ChatWords.firstLine(plan).drop { $0 == "#" }.trimmingCharacters(in: .whitespaces)
    }
}
