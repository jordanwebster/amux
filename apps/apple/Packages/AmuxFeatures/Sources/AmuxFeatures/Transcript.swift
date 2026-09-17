import AmuxCore
import AmuxDesign
import SwiftUI

private struct ExpandedPeerMessagesKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    fileprivate var expandedPeerMessages: Bool {
        get { self[ExpandedPeerMessagesKey.self] }
        set { self[ExpandedPeerMessagesKey.self] = newValue }
    }
}

public extension View {
    /// Opens peer exchanges on entry when the surrounding screen asks to show
    /// their full contents.
    func expandedPeerMessages(_ expanded: Bool = true) -> some View {
        environment(\.expandedPeerMessages, expanded)
    }
}

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
struct TranscriptFeed<Trailing: View>: View {
    @Environment(\.design) private var design
    @Environment(\.transcriptTops) private var tops
    let rows: [TranscriptRow]
    /// What stands after the last confirmed row, inside the same lazy stack
    /// so it is laid out and scrolled as part of the feed.
    @ViewBuilder let trailing: Trailing

    var body: some View {
        LazyVStack(alignment: .leading, spacing: 0) {
            ForEach(Array(rows.enumerated()), id: \.element.id) { index, _ in
                entry(at: index)
            }
            trailing
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.top, 4)
    }

    @ViewBuilder
    private func entry(at index: Int) -> some View {
        let row = rows[index]
        entry(
            row,
            railBegins: index > 0 && rows[index - 1].onRail,
            railContinues: index + 1 < rows.count && rows[index + 1].onRail)
    }

    @ViewBuilder
    private func entry(
        _ row: TranscriptRow, railBegins: Bool, railContinues: Bool
    ) -> some View {
        let content = TranscriptRowView(
            row: row, railBegins: railBegins, railContinues: railContinues)
            .equatable()
        if let tops {
            // Where this entry begins, measured against the page rather than
            // against the feed. This geometry is only installed by debug
            // recording and restoration; the production screen has no
            // consumer for it and should not recompute every visible row's
            // global position whenever the tail grows.
            content
                .onGeometryChange(for: CGFloat.self) {
                    $0.frame(in: .named(TranscriptTops.space)).minY
                } action: { tops.begins(row.id, at: $0) }
                .onDisappear { tops.forget(row.id) }
        } else {
            content
        }
    }
}

/// The optimistic tail is its own observation boundary. A local send can add
/// one prompt without rebuilding the confirmed lazy history beside it.
///
/// Each pending send is drawn exactly as its confirmed row will be — the same
/// bubble and the same gap under it, inside the feed's own gutter — so when the
/// host's echo arrives and takes its place in the same frame, nothing on the
/// page moves.
struct PendingTranscriptFeed: View {
    let model: ConversationStore

    var body: some View {
        ForEach(model.unacknowledged) { pending in
            PendingPrompt(text: pending.text)
                .padding(.bottom, 15)
        }
    }
}

/// Takes the transcript back to its foot whenever a new message is sent.
///
/// A person who scrolled back to reread something and then wrote a reply
/// expects to watch the reply go. This stands beside the scroll view rather
/// than in the feed, because the feed is lazy: a row at the foot of a long
/// transcript read from its top has not been built, and could not notice
/// anything. It observes only the newest pending send, so the transcript it
/// sits beside is not re-evaluated by a send.
struct SendFollower: View {
    let model: ConversationStore
    let follow: TranscriptFollow

    var body: some View {
        Color.clear
            .accessibilityHidden(true)
            .onChange(of: model.unacknowledged.last?.id) { before, now in
                guard let now, now != before else { return }
                follow.action?()
            }
    }
}

/// How the things standing beside a transcript reach its scroll view.
///
/// A reference, filled in by the scroll view that can act on it, so that
/// asking does not route through view state that would rebuild the feed.
@MainActor
final class TranscriptFollow {
    /// Takes the feed to its foot. Set by the transcript once it is on screen.
    var action: (@MainActor () -> Void)?

    /// The curve the next change in the space reserved under the feed is
    /// happening on, when whoever changed it animated it.
    private var curve: Animation?

    /// Says the space reserved under the feed is about to change on this
    /// curve, so the feed's tail can travel with it.
    func reserving(on curve: Animation?) {
        self.curve = curve
    }

    func takeCurve() -> Animation? {
        defer { curve = nil }
        return curve
    }
}

/// A prompt this phone has sent and the host has not echoed yet.
///
/// It is the same bubble as the confirmed prompt. What it adds is a quiet
/// "Sending" under it, and only once the wait is long enough to notice: most
/// echoes arrive within a few frames, and a caption that flashed on and off
/// for those would draw the eye to nothing. The caption is drawn over the gap
/// under the bubble rather than laid out beside it, so it neither moves the
/// feed when it appears nor costs the frame the message is first drawn in.
private struct PendingPrompt: View {
    @Environment(\.design) private var design
    let text: String
    @State private var waited = false

    /// How long a send has to be on its way before it says so.
    private static let patience = Duration.milliseconds(600)

    var body: some View {
        PromptSurface(text: text)
            .overlay(alignment: .bottomTrailing) {
                if waited {
                    Text("Sending")
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                        .alignmentGuide(.bottom) { $0[.top] - 2 }
                        .transition(.opacity)
                }
            }
            .task {
                try? await Task.sleep(for: Self.patience)
                waited = true
            }
    }
}

/// Where each entry of a transcript is on the page, kept beside the feed
/// rather than in it.
///
/// A reference rather than view state on purpose. These are answers about the
/// layout that arrive during layout, and writing one into view state would
/// invalidate the feed that was in the middle of being laid out. Nothing draws
/// from this; it is read at the two moments somebody asks where in a
/// transcript the reader is, and told where to put them back.
@MainActor
final class TranscriptTops {
    /// The name the page answers geometry questions in.
    nonisolated static let space = "transcript.page"

    private var tops: [String: CGFloat] = [:]

    func begins(_ entry: String, at top: CGFloat) {
        tops[entry] = top
    }

    func forget(_ entry: String) {
        tops[entry] = nil
    }

    func top(of entry: String) -> CGFloat? { tops[entry] }

    /// The entry the top of the readable page is inside: the last one to begin
    /// at or above it. Only entries on the page are here, which is the only
    /// place the top of it can be. Nothing before the feed has been laid out.
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
/// The latest row stays beside the composer, including when the whole transcript
/// is shorter than the viewport. Longer transcripts still open at their tail and
/// follow it while a turn streams.
///
/// Markdown and lazy rows acquire their heights after the scroll view first
/// lays out. The composer can also change the viewport as its measured height
/// and the safe-area insets arrive. Keep the opening tail attached to those
/// layout changes until the reader takes control. Waiting for every lazy row
/// to report that it finished measuring can wait forever on an offscreen row.
///
/// Two of those late changes need separate answers. A feed that grows taller is
/// a change of size, and the bottom anchor follows it. The space reserved under
/// the feed for whatever floats over it — the composer, and the strip of work
/// above it when it is unfolded — arrives instead as a bottom content inset,
/// which is not a change of size and which the anchor therefore ignores: an
/// inset several hundred points tall landing after the feed had already reached
/// its tail leaves the reader that far short of it, looking at the middle of the
/// conversation with the newest row hidden below. So the inset is watched too,
/// and reaching the tail is asked for again whenever it changes, up to a small
/// number of times so that an inset that never stops moving cannot scroll
/// forever.
struct TranscriptContainer<Content: View>: View {
    /// Where a recording left the reader, to be put back instead of the tail.
    /// Nothing is the ordinary case and the one the app itself always passes:
    /// open at the latest entry and follow it.
    var resting: TranscriptResting?
    /// Told where the reader has come to rest, once they have taken the feed
    /// off its tail. Nothing in the shipping app listens.
    var moved: ((TranscriptResting) -> Void)?
    /// The newest row now published to the view. Following this identity once
    /// is cheaper and more stable than issuing a scroll from every geometry
    /// change the resulting layout causes.
    var tail: String?
    /// Where something beside the feed asks to be taken back to its foot —
    /// a message being sent — and says how the next change in the space
    /// reserved under the feed should be followed.
    var follow: TranscriptFollow?
    @ViewBuilder let content: Content
    @Environment(\.photographed) private var photographed
    @Environment(\.reducesMotion) private var reduceMotion
    @State private var position = ScrollPosition()
    @State private var readerMoved = false
    @State private var openedAtTail = false
    @State private var tops = TranscriptTops()
    @State private var page = TranscriptPage()

    var body: some View {
        // The reader is only here so that an entry can be asked for by
        // identity. Marking the feed as a layout of scroll targets would do
        // that too, and would also move where a feed comes to rest — a
        // transcript is read by dragging it wherever you like, not by settling
        // it onto whichever row is nearest.
        ScrollViewReader { entries in
        restoring(ScrollView {
            // Keep the lazy feed directly under the scroll view. A regular
            // stack around it asks for the whole history's size when a long
            // conversation is reopened, defeating lazy construction and
            // blocking the main thread while every markdown row is measured.
            content
            // Every terminal row owns its trailing feed gap, just as the
            // source transcript does. Adding another gap at the container
            // would shift a short, bottom-anchored conversation upward and
            // leave an empty band above the composer.
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        // Named on the page rather than on the feed inside it, so a row
        // answers where it is now rather than where it sits in a feed whose
        // heights are still settling.
        .coordinateSpace(.named(TranscriptTops.space))
        .environment(\.transcriptTops, resting != nil || moved != nil ? tops : nil)
        .scrollIndicators(.hidden)
        // A transcript opens at its latest entry and stays there as it grows.
        // One put back where a recording left the reader must not: what is
        // above them is what they were reading, and an entry measuring itself
        // below them is not a reason to be taken to the bottom of the feed.
        .defaultScrollAnchor(resting == nil ? .bottom : .top, for: .initialOffset)
        .defaultScrollAnchor(resting == nil ? .bottom : .top, for: .sizeChanges)
        .defaultScrollAnchor(.bottom, for: .alignment)
        // The one place the bottom anchor cannot reach on its own. The space
        // the composer and the strip reserve under the feed arrives as a
        // bottom inset, which the scroll view does not count as a change of
        // size, so a feed already resting on its tail is left that inset's
        // height above it. Only the one number is read, so this runs when the
        // reserved space changes and not while the feed is measuring itself.
        // A feed whose tail is still in view after the change is left alone:
        // asking it to scroll to where it already is restarts the scroll
        // view's own settling, which is a visible stutter under a send.
        //
        // Only the inset is the watched value. Whether the tail is hidden
        // changes with every row a stream adds, and watching it would ask for
        // an update several times a frame; it is noted on the page instead,
        // which nothing draws from.
        .onScrollGeometryChange(for: CGFloat.self) { geometry in
            page.tailHidden = geometry.contentOffset.y + geometry.containerSize.height
                - geometry.contentInsets.bottom < geometry.contentSize.height - 0.5
            return geometry.contentInsets.bottom
        } action: { _, _ in
            // Taken whether or not a scroll follows, so a curve meant for one
            // change is never applied to some later, unrelated one.
            let curve = follow?.takeCurve()
            guard page.tailHidden, resting == nil, !readerMoved,
                  page.tailScrolls < TranscriptPage.tailScrolls
            else { return }
            page.tailScrolls += 1
            // A composer growing because a turn started grows on a curve, and
            // the feed keeps its tail beside it on the same curve rather than
            // jumping ahead of it. Every other change — the keyboard, a strip
            // measuring itself as the screen opens — is followed at once.
            let animation = still ? nil : curve
            Task { @MainActor in
                withAnimation(animation) { position.scrollTo(edge: .bottom) }
            }
        }
        .scrollPosition($position))
        .onChange(of: tail, initial: true) { _, tail in
            guard resting == nil, !readerMoved, !openedAtTail, tail != nil else { return }
            openedAtTail = true
            Task { @MainActor in position.scrollTo(edge: .bottom) }
        }
        .onScrollPhaseChange { _, phase in
            if phase == .tracking || phase == .interacting {
                readerMoved = true
            }
            // Where the reader stopped, rather than everywhere they passed
            // through. A transcript is scrolled in one gesture over hundreds
            // of entries, and a recording of every one of them would say
            // nothing a recording of the last one does not.
            if phase == .idle, readerMoved, resting == nil, let moved,
               let stopped = tops.resting(at: page.readableTop) {
                moved(stopped)
            }
        }
        .onChange(of: position.isPositionedByUser) { _, byReader in
            if byReader { readerMoved = true }
        }
        .onAppear {
            page.entries = entries
            follow?.action = {
                // Sending is taking the feed back: the reader who had
                // scrolled away has now said where they want to be, and it
                // is where the tail keeps being followed from here on.
                readerMoved = false
                page.tailScrolls = 0
                guard page.tailHidden, resting == nil else { return }
                withAnimation(still ? nil : Motion.quick) {
                    position.scrollTo(edge: .bottom)
                }
            }
        }
        }
    }

    /// Held still in front of a camera and for a reader who asked for less
    /// motion, like every other movement in the app.
    private var still: Bool { photographed || reduceMotion }

    /// Installs scroll geometry only for recording or restoring a reading
    /// position. An ordinary conversation has neither consumer; writing
    /// geometry into view state there would relayout the transcript while it
    /// was already laying out a newly arrived row.
    @ViewBuilder
    private func restoring<Scrollable: View>(_ content: Scrollable) -> some View {
        if resting != nil || moved != nil {
            content
                .onScrollGeometryChange(for: TranscriptLayout.self) { geometry in
                    TranscriptLayout(geometry)
                } action: { _, layout in
                    // A geometry callback runs while lazy measurements are
                    // being applied. Ask after that layout has finished.
                    Task { @MainActor in
                        guard layout.containerSize.height > 0, resting != nil else { return }
                        restore()
                    }
                }
                // Where the page has reached, separately from the layout
                // changes that can put a restored reader back in place.
                .onScrollGeometryChange(for: TranscriptReach.self) { geometry in
                    TranscriptReach(geometry)
                } action: { _, reach in
                    page.readableTop = reach.insetTop
                    page.offset = reach.offset
                    guard resting != nil else { return }
                    Task { @MainActor in restore() }
                }
        } else {
            content
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
            page.entries?.scrollTo(resting.entry, anchor: .top)
            return
        }
        // Where that entry should begin on the page: as far above the top of
        // the readable page as the reader had already read past it.
        let wanted = page.readableTop - resting.into
        let error = begins - wanted
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

    /// How many times a feed opening at its tail may be sent back to it after
    /// the space reserved under it changes. A handful covers the composer and
    /// the strip of work each reserving their own space; a ceiling at all is
    /// what stops two insets that keep answering each other from scrolling the
    /// feed for as long as the screen is open.
    static let tailScrolls = 8

    /// How many of those have been spent.
    var tailScrolls = 0

    /// Whether the feed's last row is under the space reserved beneath it, as
    /// of the latest geometry. A feed whose tail is still in view is not sent
    /// back to it.
    var tailHidden = true

    /// Where the readable top of the page is: below the chrome that floats
    /// over it, in the same measure the entries answer in.
    var readableTop: CGFloat = 0
    /// What the scroll view says its own offset is, which is neither the same
    /// number nor counted from the same place. It is only ever added to.
    var offset: CGFloat = 0
    /// The last position asked for, so a correction adds to what was asked
    /// rather than to what was measured.
    var asked: CGFloat?
    var corrections = 0
    /// How an entry is reached by name, for the one case where its position
    /// cannot be measured because it has not been built.
    var entries: ScrollViewProxy?
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

/// What the page is, in the two numbers putting a reader back needs.
///
/// `insetTop` is where the readable part of the page begins — under the chrome
/// that floats over the feed — measured against the page, which is what the
/// entries answer in, so the two can be compared. `offset` is the scroll view's
/// own count, which is neither the same number nor counted from the same place;
/// it is only ever added to.
private struct TranscriptReach: Equatable {
    let insetTop: CGFloat
    let offset: CGFloat

    init(_ geometry: ScrollGeometry) {
        offset = geometry.contentOffset.y
        insetTop = geometry.contentInsets.top
    }
}

/// One row, on the rail or breaking it.
private struct TranscriptRowView: View, Equatable {
    @Environment(\.design) private var design
    let row: TranscriptRow
    let railBegins: Bool
    let railContinues: Bool

    nonisolated static func == (left: Self, right: Self) -> Bool {
        left.row == right.row
            && left.railBegins == right.railBegins
            && left.railContinues == right.railContinues
    }

    var body: some View {
        switch row.kind {
        case .prompt(let text):
            PromptSurface(text: text)
                .padding(.bottom, 15)
        case .prose(let markdown, let open):
            // An agent can attach things too, through its `attach` tool, and
            // they are elements in the message text exactly as yours are — so
            // they are read out of it the same way and drawn as the same chip.
            AttachedText(text: markdown) { said in
                Prose(markdown: said, open: open)
            }
                .padding(.bottom, 15)
        case .turnEnd, .thinking:
            EmptyView()
        case .compaction(let before, let after):
            FeedRule(
                kind: "compaction", glyph: "arrow.down.right.and.arrow.up.left",
                label: Self.compacted(before, after))
                .padding(.bottom, 15)
        default:
            Rail(
                glyph: glyph, accented: accented,
                begins: railBegins, continues: railContinues
            ) {
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
        case .exploration(let reads, let searches, let anchor, let inside):
            ExplorationRow(reads: reads, searches: searches, anchor: anchor, inside: inside)
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
        case .thinking:
            EmptyView()
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
    let begins: Bool
    let continues: Bool
    @ViewBuilder let content: Content

    /// Wide enough for the widest glyph in the vocabulary and no wider: the
    /// column is a margin, and every point of it is width the prose beside it
    /// does not get.
    private let column: CGFloat = 18

    var body: some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: glyph)
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(accented ? design.accent.color : design.inkFaint.color)
                .frame(width: column, height: column)
                .background { Circle().fill(design.ground.color) }
            content
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.bottom, continues ? 11 : 15)
        }
        // A background takes the row's measured height without proposing one
        // back to it. That matters in a lazy transcript: an infinite-height
        // child inside a fixed row makes reopening a long history lay out the
        // entire feed. The path joins the previous and next marks while the
        // ground-coloured disc above knocks the line out behind this glyph.
        .background(alignment: .topLeading) {
            if begins || continues {
                GeometryReader { geometry in
                    Path { path in
                        let middle = column / 2
                        path.move(to: CGPoint(x: middle, y: begins ? 0 : middle))
                        path.addLine(to: CGPoint(
                            x: middle,
                            y: continues ? geometry.size.height : middle))
                    }
                    .stroke(design.hairline.color, lineWidth: design.metrics.hairline)
                }
            }
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
            Spacer(minLength: 44)
            words
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background {
                    RoundedRectangle(cornerRadius: design.metrics.controlRadius + 3, style: .continuous)
                        .fill(design.sunken.color)
                }
        }
        .accessibilityElement(children: .combine)
        .identified("transcript.prompt", label: text)
    }

    @ViewBuilder
    private var words: some View {
        if text.contains("<amux-attachment") {
            AttachedText(text: text, prose: prose)
        } else {
            // A plain prompt is the common send path. The marker's absence is
            // conclusive, so do not build the attachment parser's segmented
            // view hierarchy merely to return this same string as prose.
            prose(text)
        }
    }

    private func prose(_ said: String) -> some View {
        Text(said)
            .designFont(.body, design)
            .foregroundStyle(design.ink.color)
            .fixedSize(horizontal: false, vertical: true)
    }
}

private enum DocumentMetrics {
    static let blockGap: CGFloat = 14
    static let listGap: CGFloat = 6
    static let lineSpacing: CGFloat = 4
}

private func styledInline(_ source: AttributedString, design: Design) -> AttributedString {
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

private func documentHeadingFont(level: Int, design: Design) -> Font {
    let size: CGFloat = level == 1 ? 21 : (level == 2 ? 18 : 15.5)
    let style: Font.TextStyle = level == 1 ? .title3 : .headline
    BundledFonts.register()
    return .custom(design.faces.display, size: size, relativeTo: style).weight(.semibold)
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
        VStack(alignment: .leading, spacing: DocumentMetrics.blockGap) {
            ForEach(Array((document?.blocks ?? []).enumerated()), id: \.offset) { _, block in
                MarkdownBlockView(block: block)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        // Links are underlined by the parser, so the tint only has to stop
        // them arriving in the system's blue, which is not a colour this
        // design owns.
        .tint(design.accent.color)
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
            Text(styledInline(text, design: design))
                .font(documentHeadingFont(level: level, design: design))
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
        case .paragraph(let text):
            Text(styledInline(text, design: design))
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
                .lineSpacing(DocumentMetrics.lineSpacing)
                .fixedSize(horizontal: false, vertical: true)
        case .list(_, let items):
            // A list is one flowing text block. Building a stack, row and two
            // text views for every marker makes a fast transcript spend most
            // of its main-thread time laying out punctuation. Non-breaking
            // indentation and per-run colour retain the same hierarchy while
            // one Text performs the line layout for the whole block.
            Text(listText(items))
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
                .lineSpacing(DocumentMetrics.listGap)
                .fixedSize(horizontal: false, vertical: true)
        case .code(let language, let text):
            CodeBlock(language: language, text: text)
        case .quote(let lines):
            HStack(alignment: .top, spacing: 10) {
                Capsule()
                    .fill(design.inkFaint.color.opacity(0.45))
                    .frame(width: 2.5)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                        Text(styledInline(line, design: design))
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

    private func listText(_ items: [MarkdownBlock.Item]) -> AttributedString {
        var result = AttributedString()
        for (index, item) in items.enumerated() {
            if index > 0 { result.append(AttributedString("\n")) }
            var marker = AttributedString(
                String(repeating: "\u{00A0}", count: item.depth * 4) + item.marker + "\u{00A0}\u{00A0}")
            marker.foregroundColor = design.inkFaint.color
            result.append(marker)
            result.append(styledInline(item.text, design: design))
        }
        return result
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
            let shape = RoundedRectangle(
                cornerRadius: design.metrics.controlRadius, style: .continuous)
            shape.fill(design.sunken.color)
                .overlay(shape.strokeBorder(design.hairline.color, lineWidth: 1))
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
            VStack(alignment: .leading, spacing: 0) {
                line(header, emphasis: true)
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    Rectangle().fill(design.hairline.color)
                        .frame(height: design.metrics.hairline)
                    line(row, emphasis: false)
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
    }

    private func line(_ cells: [AttributedString], emphasis: Bool) -> some View {
        HStack(alignment: .top, spacing: 0) {
            ForEach(Array(cells.enumerated()), id: \.offset) { index, cell in
                Text(styledInline(cell, design: design))
                    .designFont(emphasis ? .caption : .monoSmall, design)
                    .foregroundStyle(emphasis ? design.inkMuted.color : design.ink.color)
                    .frame(width: index == 0 ? 146 : 128, alignment: .leading)
                    .padding(.horizontal, 10)
                    .padding(.vertical, emphasis ? 9 : 8)
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
/// "it looked around, here is where it started", not six lines. It
/// opens, because the paths matter once you are asking a question about them.
private struct ExplorationRow: View {
    @Environment(\.design) private var design
    let reads: Int
    let searches: Int
    let anchor: String
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
                    Text(anchor)
                        .designFont(.monoSmall, design)
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
            .buttonStyle(.amuxRow)
            .accessibilityLabel("\(counts), \(anchor)")
            .identified(
                "transcript.exploration", label: "\(counts), \(anchor)",
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
            .buttonStyle(.amuxRow)
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
        VStack(alignment: .leading, spacing: 2) {
            ActivityRow(kind: "ran", verb: "Ran", subject: command, mono: true, meta: meta)
            if let output {
                OutputPreview(output: output)
                    .padding(.leading, 27)
            }
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
            // SwiftUI shapes the whole string before applying a line limit.
            // Give it only the two lines the design actually exposes; a
            // 200-line build log must not block the frame merely to be clipped.
            Text(visibleHead)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkMuted.color)
                .lineLimit(2)
            if output.hidden != 0 {
                HStack(spacing: 6) {
                    Image(systemName: "ellipsis")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(design.inkFaint.color)
                    Text(hiddenLabel)
                        .designFont(.monoSmall, design)
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

    private var visibleHead: String {
        output.head
            .split(separator: "\n", maxSplits: 2, omittingEmptySubsequences: false)
            .prefix(2)
            .joined(separator: "\n")
    }
}

/// A message between two agents, collapsed to its first line until opened.
///
/// Collapsed because it is somebody else's conversation: what matters at a
/// glance is that it happened and who with. It opens in place, in mono, so the
/// quoted voice is visibly not this agent's prose.
private struct AgentMessageRow: View {
    @Environment(\.design) private var design
    @Environment(\.expandedPeerMessages) private var initiallyExpanded
    let from: String
    let text: String
    let outbound: Bool
    let note: String?
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: open ? 6 : 2) {
            Button { open.toggle() } label: {
                HStack(alignment: .firstTextBaseline, spacing: 9) {
                    Text(from)
                        .designFont(.identifier, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(1)
                    if let note {
                        Text(note)
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.inkFaint.color)
                    }
                    Spacer(minLength: 4)
                    Image(systemName: open ? "chevron.up" : "chevron.down")
                        .font(.system(size: 9, weight: .bold))
                        .foregroundStyle(design.inkFaint.color)
                }
                .thumbTarget(y: 13)
            }
            .buttonStyle(.amuxRow)
            .accessibilityLabel("\(from): \(text)")
            .identified(
                "transcript.agent-message", label: "\(from): \(text)",
                value: open ? "open" : "collapsed")
            .reclaimingThumbTarget(y: 13)
            Text(text)
                .designFont(open ? .mono : .monoSmall, design)
                .foregroundStyle(open ? design.inkMuted.color : design.inkFaint.color)
                .lineLimit(open ? nil : 1)
                .fixedSize(horizontal: false, vertical: open)
        }
        .accessibilityElement(children: .contain)
        .onAppear {
            if initiallyExpanded { open = true }
        }
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
    private let spacing: CGFloat = 9
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
        let warning = ["denied", "failed", "interrupted", "provider-error"].contains(kind)
        VStack(alignment: .leading, spacing: 2) {
            RowLine {
                Text(verb)
                    .designFont(.detail, design)
                    .foregroundStyle(warning ? design.ink.color : design.inkMuted.color)
                    .fixedSize()
                    .rowLine(.verb)
                if let subject {
                    Text(subject)
                        .designFont(mono ? .monoSmall : .detail, design)
                        .foregroundStyle(warning ? design.inkMuted.color : design.inkFaint.color)
                        .lineLimit(1)
                        .truncationMode(truncation)
                        .rowLine(.subject)
                }
                if let meta {
                    Text(meta)
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                        .rowLine(.meta)
                }
            }
            if let note {
                Text(note)
                    .designFont(.monoSmall, design)
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
