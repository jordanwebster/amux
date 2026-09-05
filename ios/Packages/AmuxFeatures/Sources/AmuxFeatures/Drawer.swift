import AmuxCore
import AmuxDesign
import SwiftUI

/// What the drawer was used for. Like the home, it decides nothing: it says
/// where the person wants to go and whoever presented it takes them there.
public enum DrawerAction: Equatable, Sendable {
    case open(AgentId)
    case newAgent
    case hosts
    case you
    case dismiss
}

/// The fleet as a panel that comes in from the left of a conversation.
///
/// A conversation is one agent, and moving between agents is the thing this
/// app is for, so it cannot cost a trip back to a list. The panel comes from an
/// edge nothing else uses, holds a full-height list, and has room at its foot
/// for the rest of the app.
///
/// It is not the home in miniature. The home is a place to decide what to open
/// next, with ages, headlines, arithmetic and a fold; this is a place to switch
/// while you are already reading one, so it is two groups — what needs you and
/// everything else — and a name per line. The day-old fold is deliberately not
/// here: the fold exists to keep a home short, and a panel you are scrolling
/// with your thumb already is.
public struct AgentsDrawer: View {
    @Environment(\.design) private var design
    private let model: FleetStore
    private let hosts: HostsStore
    /// The conversation this was opened from, drawn as the one you are in.
    private let current: AgentId?
    private let actions: @MainActor (DrawerAction) -> Void

    public init(
        model: FleetStore,
        hosts: HostsStore,
        current: AgentId?,
        actions: @escaping @MainActor (DrawerAction) -> Void
    ) {
        self.model = model
        self.hosts = hosts
        self.current = current
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                header
                list
                foot
            }
        }
        // A panel that names itself must not name everything inside it: a
        // name put on a container is handed down to every element under it
        // that the system does not already treat as its own, so without this
        // the rows keep their names and the title, New Agent and the foot
        // answer to "drawer" — for VoiceOver and for anything driving the app
        // alike.
        .accessibilityElement(children: .contain)
        .identified("drawer", value: current?.description ?? "none")
    }

    private var header: some View {
        HStack {
            Text("amux")
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .identified("drawer.title", value: "amux")
            Spacer()
            Button { actions(.newAgent) } label: {
                GlassIcon(glyph: "plus", prominent: true, size: 30)
            }
            .accessibilityLabel("New Agent")
            .identified("drawer.newAgent", label: "New Agent")
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.bottom, 12)
    }

    private var list: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                group("Needs you", groups.needsYou)
                group("Everything else", groups.everythingElse)
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.bottom, 24)
        }
        .scrollIndicators(.hidden)
    }

    private var groups: DrawerGroups { DrawerGroups(model.sections) }

    @ViewBuilder
    private func group(_ title: String, _ rows: [AgentRow]) -> some View {
        if !rows.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                SectionHead(title: title)
                VStack(spacing: 0) {
                    ForEach(rows) { row in
                        Button { actions(.open(row.id)) } label: {
                            DrawerRow(
                                row: row,
                                host: model.host(row.hostId)?.name,
                                open: row.id == current)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(spoken(row))
                        .accessibilityAddTraits(row.id == current ? [.isSelected] : [])
                        .identified(
                            "drawer.row.\(row.id)", label: spoken(row),
                            value: row.id == current ? "open" : row.attention.spoken)
                    }
                }
            }
        }
    }

    private func spoken(_ row: AgentRow) -> String {
        var parts = [row.name, row.attention.spoken]
        parts.append(row.headline ?? model.host(row.hostId)?.name ?? row.workingDirectory)
        if row.id == current { parts.append("the conversation you are in") }
        return parts.joined(separator: ", ")
    }

    /// The room a tab bar does not have.
    ///
    /// A panel can carry the rest of the app along its bottom edge without
    /// spending a fifth of the screen on it, which is why the two places that
    /// are not agents live here rather than beside them.
    private var foot: some View {
        VStack(spacing: 0) {
            Rectangle()
                .fill(design.hairline.color)
                .frame(height: design.metrics.hairline)
            HStack(spacing: 18) {
                footLink("externaldrive.connected.to.line.below", "Hosts", "drawer.hosts") {
                    actions(.hosts)
                }
                footLink("person.crop.circle", "You", "drawer.you") { actions(.you) }
                Spacer(minLength: 0)
                Text(online)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkFaint.color)
                    .identified("drawer.online", value: online)
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 12)
            .padding(.bottom, 34)
        }
    }

    private var online: String {
        let reachable = hosts.hosts.filter(\.online).count
        return "\(reachable) online"
    }

    private func footLink(
        _ glyph: String, _ title: String, _ identifier: String,
        action: @escaping @MainActor () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 6) {
                Image(systemName: glyph).font(.system(size: 14, weight: .medium))
                Text(title).designFont(.detail, design)
            }
            .foregroundStyle(design.inkMuted.color)
            .frame(minHeight: 44)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
        .identified(identifier, label: title)
    }
}

/// The drawer's two buckets, taken from the home's own grouping.
///
/// The home has a third: work that has been quiet for a day, folded into a
/// line naming what is in it. The fold exists to keep a home short enough to
/// scan, and a panel you are already scrolling with your thumb is not a home,
/// so the folded work is simply the tail of everything else here. Nothing is
/// dropped and nothing needs opening twice.
public struct DrawerGroups: Equatable, Sendable {
    public let needsYou: [AgentRow]
    public let everythingElse: [AgentRow]

    public init(_ sections: [FleetSection]) {
        needsYou = sections.first { $0.kind == .needsYou }?.rows ?? []
        everythingElse = sections.filter { $0.kind != .needsYou }.flatMap(\.rows)
    }
}

/// One agent in the panel: the mark, the name, and one line of what it is
/// doing or where it runs. No age and no arithmetic — you are switching, not
/// deciding, and the row you are switching away from is right above it.
struct DrawerRow: View {
    @Environment(\.design) private var design
    let row: AgentRow
    let host: String?
    let open: Bool

    var body: some View {
        HStack(spacing: 10) {
            AttentionMark(attention: row.attention, size: 17)
            VStack(alignment: .leading, spacing: 1) {
                Text(row.name)
                    .designFont(row.unread ? .identifierUnread : .identifier, design)
                    .foregroundStyle(design.ink.color)
                Text(row.headline ?? host ?? row.workingDirectory)
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
                    .lineLimit(1)
            }
            Spacer(minLength: 4)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            if open {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(design.sunken.color)
            }
        }
        .shimmering(!row.confirmed)
    }
}

/// The drawer over the screen it was opened from.
///
/// The screen behind it is never torn down and never re-entered: it slides and
/// shrinks and comes back, so closing the drawer returns to the conversation
/// exactly as it was, at the scroll position it was left at. A drawer that
/// pushed or presented would rebuild the page, and the transcript would come
/// back at the top.
///
/// It follows the thumb rather than playing an animation at it. The gesture
/// drives the same number the animation does, so a drag can catch a panel that
/// is still opening and take it back, and a flick decides the rest.
public struct DrawerOverlay<Content: View>: View {
    /// How wide the panel is. Wide enough for a name and a headline, narrow
    /// enough that the conversation behind it is still visibly there.
    public static var width: CGFloat { 302 }

    @Environment(\.design) private var design
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Binding private var open: Bool
    private let drawer: AgentsDrawer
    private let content: Content
    /// How far the thumb has taken it, live. Separate from `open` so releasing
    /// mid-drag always resolves to one of the two resting states.
    @GestureState private var dragged: CGFloat = 0

    public init(
        open: Binding<Bool>,
        drawer: AgentsDrawer,
        @ViewBuilder content: () -> Content
    ) {
        self._open = open
        self.drawer = drawer
        self.content = content()
    }

    private var progress: CGFloat {
        let width = Self.width
        return min(1, max(0, ((open ? width : 0) + dragged) / width))
    }

    public var body: some View {
        ZStack(alignment: .leading) {
            // What the screen slides off, so the corner it uncovers is the
            // app's own ground in either appearance rather than whatever the
            // window happens to be filled with.
            Ground().ignoresSafeArea()
            content
                .offset(x: Self.width * progress * 0.82)
                .scaleEffect(1 - 0.04 * progress, anchor: .center)
                // While the panel is out, the screen behind it is scenery.
                // Said before the scrim goes over it, because the scrim is
                // the way back and a disabled scrim is a drawer that can only
                // be dragged shut.
                .disabled(progress > 0)
                .accessibilityHidden(progress > 0)
                .overlay { scrim }
                .clipShape(RoundedRectangle(cornerRadius: 22 * progress, style: .continuous))
                .shadow(color: .black.opacity(0.24 * progress), radius: 24, x: -6)

            drawer
                .frame(width: Self.width)
                .offset(x: -Self.width * (1 - progress))
                .accessibilityHidden(progress == 0)
        }
        .ignoresSafeArea(edges: .bottom)
        .gesture(drag)
        .animation(reduceMotion ? nil : .snappy(duration: 0.28), value: open)
    }

    /// Tapping the sliver of the screen you came from puts it back. It is the
    /// nearest thing to hand and it is what the drawer is covering.
    @ViewBuilder
    private var scrim: some View {
        if progress > 0 {
            Rectangle()
                .fill(.black.opacity(0.22 * progress))
                .ignoresSafeArea()
                .onTapGesture { open = false }
                .accessibilityAddTraits(.isButton)
                .accessibilityLabel("Close the drawer")
                .identified("drawer.scrim", label: "Close the drawer")
        }
    }

    private var drag: some Gesture {
        DragGesture(minimumDistance: 12)
            .updating($dragged) { value, dragged, _ in
                dragged = value.translation.width
            }
            .onEnded { value in
                // Halfway, or thrown hard enough that halfway is where it was
                // heading. Both, because a slow drag and a flick are different
                // intentions and only one of them is about distance.
                let travelled = (open ? Self.width : 0) + value.translation.width
                let thrown = value.predictedEndTranslation.width
                    - value.translation.width
                open = travelled + thrown > Self.width / 2
            }
    }
}
