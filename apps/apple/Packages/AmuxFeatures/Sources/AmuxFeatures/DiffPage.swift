import AmuxCore
import AmuxDesign
import SwiftUI

/// What somebody did to a review. Like every other screen this decides nothing
/// and stores nothing: it says what happened and whoever presented it applies
/// that to the review.
public enum ReviewAction: Equatable, Sendable {
    /// Back to the conversation these changes belong to.
    case back
    /// A range a finger took hold of, which is what the sheet opens about.
    case select(LineRange)
    /// Something said about that range.
    case comment(LineRange, String)
    /// The sheet dismissed without saying anything.
    case cancelComment
    /// The whole review, back to the composer as a token.
    case attachReview
    /// A file folded away or opened again.
    case toggleFile(String)
    /// Jumped to a file, from the file list or the wheel on the edge.
    case scrubTo(String)
}

/// The changes one turn made, as one scroll.
///
/// It is one document rather than a file list you drill into. A phone review
/// is read the way a patch is read — top to bottom, in order — and a list of
/// files that each open a page of their own turns twelve taps into the price
/// of reading twelve short changes. So every file stays in the producer's
/// narrative order, and the way to skip one is to fold it rather than to leave
/// the page.
///
/// The runtime currently carries each hunk's first row but not its contextual
/// title, so this screen does not invent one. Machine-coordinate headers such
/// as `@@ -118,7 +118,6 @@` add no information beyond the row numbers already
/// shown. Nothing scrolls sideways: a line too long for the display wraps,
/// because a horizontal scroll view inside a vertical one on a phone makes
/// neither gesture reliable and the end of a long line is often the important
/// part of a change.
public struct DiffPage: View {
    @Environment(\.design) private var design
    private let model: ReviewStore
    private let subject: String
    private let actions: @MainActor (ReviewAction) -> Void
    private let wheel: String?
    /// The file list, which the heading opens.
    @State private var listing = false
    /// Where each row is, so a finger dragging over the page can be told which
    /// row it is on. Written by the rows themselves as they lay out.
    @State private var rowFrames: [RowRef: CGRect] = [:]
    /// Where the drag started, kept so the range grows from the row that was
    /// held rather than from the last row crossed.
    @State private var anchorRow: RowRef?

    private nonisolated static let space = "diff"

    public init(
        model: ReviewStore,
        subject: String,
        wheel: String? = nil,
        actions: @escaping @MainActor (ReviewAction) -> Void
    ) {
        self.model = model
        self.subject = subject
        self.wheel = wheel
        self.actions = actions
    }

    public var body: some View {
        ZStack(alignment: .bottom) {
            Ground()
            scroll
            if let range = model.selection {
                Color.black.opacity(Glass.scrim)
                    .ignoresSafeArea()
                    .accessibilityHidden(true)
                CommentSheet(
                    model: model, range: range,
                    add: { actions(.comment(range, $0)) },
                    cancel: { actions(.cancelComment) })
                    .transition(.move(edge: .bottom))
            } else if !model.comments.isEmpty {
                attach
            }
        }
        .moving(value: model.selection != nil)
        .accessibilityElement(children: .contain)
        .identified("review", value: model.diff.description)
    }

    private var scroll: some View {
        ScrollViewReader { scroller in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0, pinnedViews: []) {
                    ForEach(Array(model.files.enumerated()), id: \.element.id) { index, file in
                        FileHeading(
                            label: label(for: file), file: file,
                            comments: model.comments(in: file.path),
                            collapsed: model.isCollapsed(file.path),
                            toggle: { actions(.toggleFile(file.path)) },
                            list: { listing = true })
                            .id(file.path)
                        if !model.isCollapsed(file.path) {
                            rows(of: file, at: index)
                        }
                    }
                }
                .padding(.bottom, 120)
                .coordinateSpace(.named(Self.space))
                .gesture(holdAndDrag {
                    reveal(with: scroller, animated: true)
                })
            }
            .scrollIndicators(.hidden)
            .safeAreaInset(edge: .top, spacing: 0) { chrome }
            .overlay(alignment: .trailing) {
                EdgeWheel(
                    files: model.files, comments: { model.comments(in: $0) },
                    initiallyOn: wheel.flatMap { path in model.files.firstIndex { $0.path == path } }
                ) { path in
                    actions(.scrubTo(path))
                    withAnimation(Motion.quick) { scroller.scrollTo(path, anchor: .top) }
                }
            }
            // A page opened on a review that already has a range held — coming
            // back to one, or a named state a screenshot is taken of — starts
            // looking at it, not at the top of the patch.
            .onAppear { reveal(with: scroller, animated: false) }
            .sheet(isPresented: $listing) {
                FileList(files: model.files, comments: commentCounts) { path in
                    listing = false
                    actions(.scrubTo(path))
                    withAnimation(Motion.quick) { scroller.scrollTo(path, anchor: .top) }
                }
            }
        }
    }

    /// Enough of a path to distinguish this file from the others in the
    /// patch. A basename is easier to scan; its parent joins only when two
    /// basenames would otherwise be identical.
    private func label(for file: ReviewFile) -> String {
        let name = file.path.split(separator: "/").last.map(String.init) ?? file.path
        guard model.files.filter({
            ($0.path.split(separator: "/").last.map(String.init) ?? $0.path) == name
        }).count > 1 else { return name }
        let parts = file.path.split(separator: "/")
        return parts.count >= 2 ? parts.suffix(2).joined(separator: "/") : name
    }

    @ViewBuilder
    private func rows(of file: ReviewFile, at index: Int) -> some View {
        ForEach(Array(file.rows.enumerated()), id: \.offset) { position, row in
            let ref = RowRef(file: index, row: position)
            DiffRowView(
                row: row,
                selected: model.selection?.contains(ref) ?? false,
                commented: !model.comments(under: ref).isEmpty)
                .onGeometryChange(for: CGRect.self) { proxy in
                    proxy.frame(in: .named(Self.space))
                } action: { frame in
                    rowFrames[ref] = frame
                }
                .id(ref)
            ForEach(model.comments(under: ref)) { comment in
                CommentThread(comment: comment)
            }
        }
    }

    /// Hold a line, then drag: the phone's own way of saying "this bit".
    ///
    /// A plain drag cannot be it — that is how the page scrolls — and a tap
    /// cannot be it either, because a range needs two ends. Holding first is
    /// what tells the scroll view to let go, and it is the same gesture the
    /// system uses to start a text selection, so nobody has to be taught it.
    private func holdAndDrag(reveal: @escaping @MainActor () -> Void) -> some Gesture {
        LongPressGesture(minimumDuration: 0.3)
            .sequenced(before: DragGesture(minimumDistance: 0, coordinateSpace: .named(Self.space)))
            .onChanged { value in
                guard case .second(true, let drag?) = value else { return }
                guard let over = row(at: drag.location) else { return }
                let from = anchorRow ?? over
                if anchorRow == nil { anchorRow = over }
                // A range that reached into another file is not a range: the
                // selection keeps the file it began in rather than anchoring
                // somewhere the reader did not choose.
                guard over.file == from.file else { return }
                actions(.select(LineRange(file: from.file, from: from.row, to: over.row)))
            }
            .onEnded { _ in
                anchorRow = nil
                // Wait until the finger is up before moving the selection
                // under the chrome. Moving the scroll view while the drag is
                // still measured against it changes which row is under that
                // stationary finger and silently grows the range.
                reveal()
            }
    }

    /// Puts the held range under the chrome, where the sheet is not covering
    /// it.
    private func reveal(with scroller: ScrollViewProxy, animated: Bool) {
        guard let range = model.selection else { return }
        let row = RowRef(file: range.file, row: range.from)
        guard animated else { return scroller.scrollTo(row, anchor: .top) }
        withAnimation(Motion.quick) { scroller.scrollTo(row, anchor: .top) }
    }

    private func row(at point: CGPoint) -> RowRef? {
        rowFrames.first { $0.value.contains(point) }?.key
    }

    // MARK: - The chrome

    /// The way out, what this is, and how much has been said about it.
    private var chrome: some View {
        HStack(alignment: .center, spacing: 8) {
            BackLink(subject, identifier: "review.back") { actions(.back) }
                .lineLimit(1)
            Spacer(minLength: 4)
            VStack(spacing: 0) {
                Text("Review")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                Text(magnitudes)
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
            }
            Spacer(minLength: 4)
            CommentCount(count: model.comments.count)
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.vertical, 6)
        // Opaque, unlike the conversation's floating chrome. A patch is read
        // by running down a column of numbered lines, and a bar you can read
        // the lines through puts two of them in the same place; the numbers
        // stop being a column the moment that happens.
        .background {
            design.ground.color
                .ignoresSafeArea(edges: .top)
                .overlay(alignment: .bottom) {
                    Rectangle()
                        .fill(design.hairline.color)
                        .frame(height: design.metrics.hairline)
                }
        }
        .accessibilityElement(children: .contain)
        .identified("review.chrome", label: "Review", value: magnitudes)
    }

    private var commentCounts: [String: Int] {
        Dictionary(
            model.files.map { ($0.path, model.comments(in: $0.path)) },
            uniquingKeysWith: { first, _ in first })
    }

    /// "4 files · +18 −28", counted from the document rather than from
    /// whatever the turn reported: what this page shows is this patch.
    private var magnitudes: String {
        let files = model.files.count
        return "\(files) file\(files == 1 ? "" : "s") \u{00B7} "
            + "+\(model.document.insertions) \u{2212}\(model.document.deletions)"
    }

    private var attach: some View {
        Button {
            // The remark sheet's keyboard does not belong to the conversation
            // this review is about to land in. See `Keyboard`.
            Keyboard.putDown()
            actions(.attachReview)
        } label: {
            ActionLabel(attachTitle, kind: .primary, fill: true)
        }
        .buttonStyle(.amuxControl)
        .modifier(ReviewBottomAction())
        .accessibilityLabel(attachTitle)
        .identified("review.attach", label: attachTitle)
    }

    private var attachTitle: String {
        let count = model.comments.count
        return "Attach Review \u{00B7} \(count) comment\(count == 1 ? "" : "s")"
    }
}

/// Keeps the shared bottom tray around a button without erasing its concrete
/// type before accessibility modifiers are applied.
private struct ReviewBottomAction: ViewModifier {
    func body(content: Content) -> some View {
        BottomAction { content }
    }
}

/// One file's heading: whether it is open, what it is called, how much it
/// changed and how much has been said about it.
///
/// Two controls in one line, deliberately. The row folds the file, which is
/// what a reader wants nine times out of ten; the small stack of chevrons
/// beside the path opens the list of every file, which is the tenth.
private struct FileHeading: View {
    @Environment(\.design) private var design
    let label: String
    let file: ReviewFile
    let comments: Int
    let collapsed: Bool
    let toggle: @MainActor () -> Void
    let list: @MainActor () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Button(action: toggle) {
                HStack(spacing: 8) {
                    Image(systemName: collapsed ? "chevron.right" : "chevron.down")
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(design.inkFaint.color)
                    Text(label)
                        .designFont(.identifier, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(1)
                }
                .thumbTarget(y: 14)
            }
            .buttonStyle(.amuxRow)
            .accessibilityLabel(file.path)
            .identified(
                "review.file", label: file.path,
                value: collapsed ? "collapsed" : "open")
            .reclaimingThumbTarget(y: 14)
            Button(action: list) {
                Image(systemName: "chevron.up.chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
                    .thumbTarget(x: 18, y: 17)
            }
            .buttonStyle(.amuxControl)
            .accessibilityLabel("All Files")
            .identified("review.files", label: "All Files")
            .reclaimingThumbTarget(x: 18, y: 17)
            Spacer(minLength: 6)
            if comments > 0 { CommentCount(count: comments, size: 16) }
            Text("+\(file.added)")
                .designFont(.monoSmall, design)
                .foregroundStyle(design.added.color)
            Text("\u{2212}\(file.removed)")
                .designFont(.monoSmall, design)
                .foregroundStyle(design.removed.color)
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(design.sunken.color)
        .overlay(alignment: .bottom) {
            Rectangle().fill(design.hairline.color).frame(height: design.metrics.hairline)
        }
        .accessibilityElement(children: .contain)
    }
}

/// One line of a patch: which line it is, whether it came or went, and what it
/// says.
private struct DiffRowView: View {
    @Environment(\.design) private var design
    let row: DiffRow
    let selected: Bool
    let commented: Bool

    var body: some View {
        switch row.kind {
        case .boundary:
            // The break between hunks says only that the next line is not the
            // one after the last, and the numbers on either side of it say
            // that already. So it is a gap rather than a sentence.
            Rectangle()
                .fill(design.hairline.color)
                .frame(height: design.metrics.hairline)
                .padding(.vertical, 7)
                .padding(.horizontal, design.metrics.gutter)
        case .note:
            Text(row.text)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkFaint.color)
                .padding(.horizontal, design.metrics.gutter)
                .padding(.vertical, 3)
        case .context, .added, .removed:
            line
        }
    }

    private var line: some View {
        HStack(alignment: .top, spacing: 0) {
            Text(number)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkFaint.color.opacity(0.7))
                .frame(width: 26, alignment: .trailing)
                .padding(.trailing, 6)
            Text(marker)
                .designFont(.monoSmall, design)
                .foregroundStyle(mark)
                .frame(width: 11, alignment: .center)
            Text(content)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.ink.color)
                // Wrapped, never scrolled sideways. The end of a long line is
                // where the interesting half of a change usually is.
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, design.metrics.gutter - 6)
        .padding(.vertical, 2)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tint)
        .overlay {
            if selected {
                Rectangle().fill(design.accent.color.opacity(0.16))
                    .overlay(alignment: .leading) {
                        Rectangle().fill(design.accent.color).frame(width: 2.5)
                    }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(spoken) \(number), \(content)")
    }

    /// The number in the file this row exists in: a removed row is only in the
    /// old one, an added row only in the new. Never both, and never invented.
    private var number: String {
        switch row.kind {
        case .removed: row.old.map(String.init) ?? ""
        default: row.new.map(String.init) ?? ""
        }
    }

    /// The patch writes the sign as the first character of the line. It is
    /// drawn in its own column, so the text beside it is the text.
    private var content: String { String(row.text.dropFirst()) }

    private var marker: String {
        switch row.kind {
        case .added: "+"
        case .removed: "\u{2212}"
        default: " "
        }
    }

    private var mark: Color {
        switch row.kind {
        case .added: design.added.color
        case .removed: design.removed.color
        default: design.inkFaint.color
        }
    }

    private var spoken: String {
        switch row.kind {
        case .added: "Added line"
        case .removed: "Removed line"
        default: "Line"
        }
    }

    /// A row under a finger is the strongest thing on the page, then the two
    /// diff washes. Selection deliberately reads as a hold rather than as a
    /// third kind of change: it is temporary and it is the reader's, not the
    /// patch's.
    private var tint: Color {
        switch row.kind {
        case .added: return design.added.color.opacity(0.13)
        case .removed: return design.removed.color.opacity(0.13)
        default: return commented ? design.sunken.color : .clear
        }
    }
}

/// Something somebody said, under the line they said it about.
private struct CommentThread: View {
    @Environment(\.design) private var design
    let comment: ReviewComment

    var body: some View {
        HStack(alignment: .top, spacing: 9) {
            Rectangle()
                .fill(design.accent.color)
                .frame(width: 2)
            Text(comment.text)
                .designFont(.detail, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.vertical, 10)
        .padding(.trailing, 12)
        .padding(.leading, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(design.sunken.color)
        .accessibilityElement(children: .combine)
        .identified(
            "review.comment", label: comment.text,
            value: "\(comment.path) \(comment.lines)")
    }
}

/// What is being said, about which lines.
///
/// It is drawn in the page rather than presented as a system sheet because it
/// is about the rows behind it: the range stays highlighted while the words are
/// written, and a presentation that took over the screen would hide the one
/// thing the writing is about.
private struct CommentSheet: View {
    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    let model: ReviewStore
    let range: LineRange
    let add: @MainActor (String) -> Void
    let cancel: @MainActor () -> Void
    /// The field takes the keyboard the moment the sheet arrives: somebody who
    /// held a line and dragged it has already said what they want to do.
    @FocusState private var writing: Bool

    var body: some View {
        @Bindable var model = model
        VStack(spacing: 0) {
            Capsule()
                .fill(design.inkFaint.color.opacity(0.6))
                .frame(width: 38, height: 5)
                .padding(.vertical, 9)

            VStack(alignment: .leading, spacing: 12) {
                HStack(spacing: 6) {
                    Rectangle()
                        .fill(design.accent.color)
                        .frame(width: 2, height: 12)
                    Text("\(range.to - range.from + 1) line\(range.to == range.from ? "" : "s") in")
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkMuted.color)
                    Text(model.anchor(range)?.path ?? "")
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(1)
                        .truncationMode(.head)
                    Spacer(minLength: 6)
                    Text(lines)
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
                }

                TextField("", text: $model.draft, axis: .vertical)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(2...6)
                    .focused($writing)
                    // A remark about a patch is half identifiers: `Code::Internal`
                    // corrected to something English is worse than a typo.
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.sentences)
                    // The caret blinks on a timer of its own, so photographs
                    // omit it while the live field keeps the design tint.
                    .tint(photographed ? .clear : design.accentColor)
                    .identified("review.commentField", value: model.draft)
                    .padding(.top, 2)

                HStack(spacing: 8) {
                    Button { done { add(model.draft) } } label: {
                        ActionLabel("Add to Review", kind: .primary, fill: true)
                    }
                    .buttonStyle(.amuxControl)
                    .disabled(model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityLabel("Add to Review")
                    .identified("review.addComment", label: "Add to Review")
                    Button { done(cancel) } label: {
                        ActionLabel("Cancel", kind: .outline, fill: true)
                    }
                    .buttonStyle(.amuxControl)
                    .accessibilityLabel("Cancel")
                    .identified("review.cancelComment", label: "Cancel")
                }
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 16)
        }
        .frame(maxWidth: .infinity)
        .background {
            Color.clear.frosted(
                UnevenRoundedRectangle(
                    topLeadingRadius: design.metrics.floatRadius,
                    topTrailingRadius: design.metrics.floatRadius,
                    style: .continuous))
                .ignoresSafeArea(edges: .bottom)
        }
        .onAppear { writing = true }
        .accessibilityElement(children: .contain)
        .identified("review.commentSheet", label: model.describe(range) ?? "", value: lines)
    }

    /// Gives the keyboard back, then does the thing that closes the sheet.
    ///
    /// The sheet takes the keyboard when it arrives and is the only thing on
    /// this page that wants one, so it has to hand it back before it goes.
    /// A keyboard left standing after the view that asked for it is gone
    /// belongs to nothing: it stays up over whatever comes next — the
    /// conversation this review is attached to — and that screen, built while
    /// it was already on show, is laid out as though the bottom of the display
    /// were free. Its composer ends up underneath the keys, where neither a
    /// finger nor a driver can reach it.
    private func done(_ act: @MainActor @escaping () -> Void) {
        writing = false
        Keyboard.putDown()
        act()
    }

    /// The lines in the file's own numbering, which is what a comment is
    /// finally addressed by.
    private var lines: String {
        guard let anchor = model.anchor(range) else { return "" }
        return anchor.startLine == anchor.line
            ? "\(anchor.line)"
            : "\(anchor.startLine)\u{2013}\(anchor.line)"
    }
}

/// A dot per file down the trailing edge, and the file it is on named while a
/// thumb is on it.
///
/// A review is one scroll, which makes skipping a file the one thing a scroll
/// cannot do cheaply. The wheel is the answer: it is the length of the page,
/// each file is a stop on it, and it names what it lands on so nobody has to
/// let go to find out where they are.
private struct EdgeWheel: View {
    @Environment(\.design) private var design
    let files: [ReviewFile]
    let comments: (String) -> Int
    let scrub: @MainActor (String) -> Void
    @State private var on: Int?

    init(
        files: [ReviewFile], comments: @escaping (String) -> Int,
        initiallyOn: Int? = nil, scrub: @escaping @MainActor (String) -> Void
    ) {
        self.files = files
        self.comments = comments
        self.scrub = scrub
        _on = State(initialValue: initiallyOn)
    }

    private var weights: [CGFloat] {
        files.map { max(1, CGFloat($0.added + $0.removed)) }
    }

    var body: some View {
        GeometryReader { proxy in
            let available = max(1, proxy.size.height - 130)
            let total = max(1, weights.reduce(0, +))
            let heights = weights.map { $0 / total * available }
            let current = on ?? 0
            let offset = heights.prefix(current).reduce(0, +)

            ZStack(alignment: .topTrailing) {
                VStack(spacing: 2) {
                    ForEach(Array(files.enumerated()), id: \.element.id) { index, file in
                        ZStack(alignment: .trailing) {
                            Capsule()
                                .fill(on == index
                                    ? design.ink.color.opacity(0.7)
                                    : design.inkFaint.color.opacity(0.3))
                                .frame(width: on == index ? 4 : 3)
                            if comments(file.path) > 0 {
                                Circle()
                                    .fill(design.accent.color)
                                    .frame(width: 5, height: 5)
                                    .offset(x: -7)
                            }
                        }
                        .frame(height: heights[index])
                    }
                }
                .frame(width: 14)
                .padding(.trailing, 6)
                .padding(.top, 65)

                if let on, files.indices.contains(on) {
                    let file = files[on]
                    HStack(spacing: 8) {
                        Text(name(file.path))
                            .designFont(.identifier, design)
                            .foregroundStyle(design.ink.color)
                        Text("+\(file.added)")
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.added.color)
                        Text("\u{2212}\(file.removed)")
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.removed.color)
                    }
                    .padding(.horizontal, 13)
                    .padding(.vertical, 9)
                    .background { Color.clear.frosted(Capsule(), as: .glass) }
                    .padding(.trailing, 26)
                    .offset(y: 65 + offset + heights[on] / 2 - 19)
                    .transition(.opacity)
                }
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
            .overlay(alignment: .trailing) {
                Color.clear
                    .frame(width: 22)
                    .contentShape(Rectangle())
                    .gesture(
                        DragGesture(minimumDistance: 0)
                            .onChanged { drag in
                                let location = min(max(drag.location.y - 65, 0), available)
                                var boundary: CGFloat = 0
                                let index = heights.enumerated().first { _, height in
                                    boundary += height
                                    return location <= boundary
                                }?.offset ?? max(files.count - 1, 0)
                                guard files.indices.contains(index), index != on else { return }
                                on = index
                                scrub(files[index].path)
                            }
                            .onEnded { _ in on = nil })
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .trailing)
        .accessibilityHidden(true)
        .identified("review.wheel", value: on.map(String.init) ?? "idle")
    }

    private func name(_ path: String) -> String {
        path.split(separator: "/").last.map(String.init) ?? path
    }
}

/// Every file in the patch, as somewhere to jump to.
private struct FileList: View {
    @Environment(\.design) private var design
    let files: [ReviewFile]
    let comments: [String: Int]
    let pick: @MainActor (String) -> Void

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 0) {
                    ForEach(files) { file in
                        Button { pick(file.path) } label: {
                            HStack(spacing: 8) {
                                Text(file.path)
                                    .designFont(.mono, design)
                                    .foregroundStyle(design.ink.color)
                                    .lineLimit(1)
                                    .truncationMode(.head)
                                Spacer(minLength: 6)
                                if let said = comments[file.path], said > 0 {
                                    CommentCount(count: said, size: 20)
                                }
                                Text("+\(file.added)")
                                    .designFont(.monoSmall, design)
                                    .foregroundStyle(design.added.color)
                                Text("\u{2212}\(file.removed)")
                                    .designFont(.monoSmall, design)
                                    .foregroundStyle(design.removed.color)
                            }
                            .padding(.horizontal, design.metrics.gutter)
                            .frame(minHeight: 44)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.amuxRow)
                    }
                }
                .padding(.vertical, 8)
            }
            .background { Ground() }
            .navigationTitle("Files")
            .navigationBarTitleDisplayMode(.inline)
        }
        .presentationDetents([.medium, .large])
    }
}

/// How much has been said, as a disc.
///
/// It is ink rather than the accent: a comment somebody wrote is not something
/// waiting on them, and the accent is this app's one word for that.
private struct CommentCount: View {
    @Environment(\.design) private var design
    let count: Int
    var size: CGFloat = 16

    var body: some View {
        ZStack {
            Circle().fill(design.accent.color)
            Text("\(count)")
                .designFont(.caption, design)
                .foregroundStyle(design.onAccent.color)
        }
        .frame(width: size, height: size)
        .accessibilityLabel("\(count) comment\(count == 1 ? "" : "s")")
        .identified("review.count", label: "\(count) comments", value: "\(count)")
    }
}
