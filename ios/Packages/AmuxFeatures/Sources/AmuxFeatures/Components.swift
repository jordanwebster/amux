import AmuxCore
import AmuxDesign
import SwiftUI

/// The status mark.
///
/// Three marks, and only three, because a glyph nobody can read is worse than
/// no glyph: it occupies the place a reader looks for meaning and returns
/// nothing. So the vocabulary is the states that are worth distinguishing and
/// that the app can honestly know.
///
/// - **needs you** — the agent has stopped and cannot continue without you.
///   The only thing on the screen allowed to be the accent colour.
/// - **working** — achromatic and moving, because work in progress is
///   information rather than a demand.
/// - **unknown** — hollow, because a filled mark would claim knowledge the app
///   does not have.
///
/// Idle draws nothing. A finished turn draws nothing either and says
/// "Finished · 4 files · +118 −40" on the row instead, which is more precise
/// than a tick and readable without having learnt a vocabulary first.
public struct AttentionMark: View {
    @Environment(\.design) private var design
    private let attention: Attention
    private let size: CGFloat

    public init(attention: Attention, size: CGFloat = 19) {
        self.attention = attention
        self.size = size
    }

    public var body: some View {
        switch attention {
        case .idle:
            Color.clear.frame(width: size, height: size)
        case .working:
            WorkingMark(size: size)
        case .unknown:
            Circle()
                .strokeBorder(
                    design.inkFaint.color,
                    style: StrokeStyle(lineWidth: 1.5, dash: [2.2, 2.6]))
                .frame(width: size, height: size)
        case .needsYou(let why):
            if why == .finished {
                Color.clear.frame(width: size, height: size)
            } else {
                NeedsYouMark(glyph: why.glyph, size: size)
            }
        }
    }
}

/// The accent disc with a glyph in it: the one thing on a screen allowed to be
/// coloured, because it is the one thing that is waiting for you.
///
/// The list draws it small on a row and an ask panel draws it larger at the
/// head of the thing being asked. It is one mark either way — a person who has
/// learnt what it means on the home should not have to learn it again inside a
/// conversation.
public struct NeedsYouMark: View {
    @Environment(\.design) private var design
    private let glyph: String
    private let size: CGFloat

    public init(glyph: String, size: CGFloat = 19) {
        self.glyph = glyph
        self.size = size
    }

    public var body: some View {
        ZStack {
            Circle().fill(design.accent.color)
            Image(systemName: glyph)
                .font(.system(size: size * 0.5, weight: .bold))
                .foregroundStyle(design.onAccent.color)
        }
        .frame(width: size, height: size)
    }
}

/// A ring that sweeps. Achromatic on purpose: work in progress is information,
/// not a demand.
private struct WorkingMark: View {
    @Environment(\.design) private var design
    let size: CGFloat
    /// A capture is a still frame, so the sweep is drawn at a fixed angle that
    /// reads as motion rather than relying on an animation nobody will see.
    private let sweep = 0.68

    var body: some View {
        ZStack {
            Circle().strokeBorder(design.inkFaint.color.opacity(0.35), lineWidth: 2)
            Circle()
                .trim(from: 0, to: sweep)
                .stroke(design.inkMuted.color, style: StrokeStyle(lineWidth: 2, lineCap: .round))
                .rotationEffect(.degrees(-90))
        }
        .frame(width: size, height: size)
    }
}

extension Why {
    public var glyph: String {
        switch self {
        case .permission: "hand.raised.fill"
        case .question: "questionmark"
        case .finished: "checkmark"
        }
    }

    /// What the mark means, said aloud. A mark that only exists as a shape is
    /// unreadable to anyone using VoiceOver, so every row spells it.
    public var spoken: String {
        switch self {
        case .permission: "Needs permission"
        case .question: "Has a question"
        case .finished: "Finished"
        }
    }
}

extension Attention {
    /// The row's state in a word, for a reader who cannot see the mark.
    public var spoken: String {
        switch self {
        case .idle: "Idle"
        case .working: "Working"
        case .unknown: "State unknown"
        case .needsYou(let why): why.spoken
        }
    }
}

/// A group of rows on one surface, hairline-separated.
public struct RowGroup<Item: Identifiable, Content: View>: View {
    @Environment(\.design) private var design
    private let items: [Item]
    private let prominence: Design.Prominence
    private let row: (Item) -> Content

    public init(
        items: [Item],
        prominence: Design.Prominence = .plain,
        @ViewBuilder row: @escaping (Item) -> Content
    ) {
        self.items = items
        self.prominence = prominence
        self.row = row
    }

    public var body: some View {
        Surface(prominence: prominence) {
            VStack(spacing: 0) {
                ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
                    row(item)
                    if index < items.count - 1 {
                        Rectangle()
                            .fill(design.hairline.color)
                            .frame(height: design.metrics.hairline)
                            .padding(.leading, 46)
                    }
                }
            }
        }
    }
}

/// A section header. Quiet, uppercase, and never coloured — a heading is
/// structure, not attention.
public struct SectionHead: View {
    @Environment(\.design) private var design
    private let title: String
    private let trailing: String?

    public init(title: String, trailing: String? = nil) {
        self.title = title
        self.trailing = trailing
    }

    public var body: some View {
        HStack {
            Text(title.uppercased())
                .designFont(.sectionTitle, design)
                .foregroundStyle(design.inkFaint.color)
            Spacer()
            if let trailing {
                Text(trailing)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkFaint.color)
            }
        }
        .padding(.leading, 2)
        .accessibilityElement(children: .combine)
    }
}

/// A button. Four weights, and the outline exists so a refusal can sit beside
/// an approval at the same size without looking like the same offer.
public struct ActionLabel: View {
    @Environment(\.design) private var design
    private let title: String
    private let kind: Kind
    private let fill: Bool

    /// `primary` is ink, not accent. Three waiting agents on one screen means
    /// three primary buttons, and filling those with the accent floods a
    /// screen whose whole rule is that colour means attention.
    public enum Kind: Sendable { case primary, quiet, outline, plain }

    public init(_ title: String, kind: Kind = .primary, fill: Bool = false) {
        self.title = title
        self.kind = kind
        self.fill = fill
    }

    public var body: some View {
        Text(title)
            .designFont(.bodyEmphasis, design)
            .foregroundStyle(foreground)
            .lineLimit(1)
            .padding(.horizontal, 16)
            .padding(.vertical, 11)
            .frame(maxWidth: fill ? .infinity : nil, minHeight: 44)
            .background {
                let shape = RoundedRectangle(
                    cornerRadius: design.metrics.controlRadius, style: .continuous)
                switch kind {
                case .primary: shape.fill(design.ink.color)
                case .quiet: shape.fill(design.sunken.color)
                case .outline: shape.strokeBorder(design.hairline.color, lineWidth: 1)
                case .plain: shape.fill(.clear)
                }
            }
    }

    private var foreground: Color {
        switch kind {
        case .primary: design.ground.color
        case .quiet, .outline: design.ink.color
        case .plain: design.accent.color
        }
    }
}

/// A round glass button — the shape iOS uses for a bare action in a bar.
public struct GlassIcon: View {
    @Environment(\.design) private var design
    private let glyph: String
    private let prominent: Bool
    private let size: CGFloat

    public init(glyph: String, prominent: Bool = false, size: CGFloat = 34) {
        self.glyph = glyph
        self.prominent = prominent
        self.size = size
    }

    public var body: some View {
        Image(systemName: glyph)
            .font(.system(size: size * 0.44, weight: .semibold))
            .foregroundStyle(prominent ? design.onAccent.color : design.ink.color)
            .frame(width: size, height: size)
            .background {
                if prominent {
                    Circle().fill(design.accent.color)
                } else {
                    Color.clear.frosted(Circle())
                }
            }
            // The glyph is 34 pt but the thing you hit is not: a control this
            // small has to keep the 44 pt target the guidelines ask for.
            .frame(width: 44, height: 44)
            .contentShape(Circle())
    }
}

/// Supporting prose, one step down from the thing it explains.
public struct Explain: View {
    @Environment(\.design) private var design
    private let text: String

    public init(_ text: String) {
        self.text = text
    }

    public var body: some View {
        Text(text)
            .designFont(.detail, design)
            .foregroundStyle(design.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// What a row looks like while it is still only remembered.
///
/// A launch draws the fleet this phone saw last time before it has reached
/// anything, and those rows are honest but unverified: the agent may have
/// finished, or moved on, or gone. So they are dimmed and a slow highlight
/// passes over them, and the moment the machine that owns a row answers, that
/// row alone goes solid. Nothing is replaced and nothing moves — the list a
/// thumb is already travelling towards is the list that gets confirmed.
///
/// A spinner would be the alternative, and it would be worse: it would cover
/// content the person can already read and act on with a symbol that says only
/// "wait". The screen never spins for this, whatever it costs, because there is
/// always something true to show while it waits.
///
/// The sweep is an implicit animation rather than a display link the app drives
/// itself: Core Animation runs it in the render server, so a screen full of
/// remembered rows costs the main thread nothing per frame, and it stops on its
/// own when the last row is confirmed. Under Reduce Motion the dimming stays
/// and the sweep does not run.
public struct Shimmer: ViewModifier {
    @Environment(\.design) private var design
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// The row is still only remembered.
    private let active: Bool
    @State private var swept = false

    public init(active: Bool) {
        self.active = active
    }

    public func body(content: Content) -> some View {
        content
            .opacity(active ? 0.55 : 1)
            .overlay { if active && !reduceMotion { sweep } }
    }

    private var sweep: some View {
        GeometryReader { frame in
            LinearGradient(
                colors: [.clear, design.ink.color.opacity(0.09), .clear],
                startPoint: .leading, endPoint: .trailing)
                .frame(width: frame.size.width * 0.45)
                .offset(x: swept ? frame.size.width : -frame.size.width * 0.45)
                .animation(
                    .linear(duration: 1.6).repeatForever(autoreverses: false), value: swept)
        }
        .allowsHitTesting(false)
        .onAppear { swept = true }
    }
}

extension View {
    /// Draws this as a row that has not been confirmed yet. See ``Shimmer``.
    public func shimmering(_ active: Bool) -> some View {
        modifier(Shimmer(active: active))
    }
}
