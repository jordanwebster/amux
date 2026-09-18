import AmuxCore
import AmuxDesign
import SwiftUI

/// The status mark.
///
/// One mark, because a glyph nobody can read is worse than no glyph: it
/// occupies the place a reader looks for meaning and returns nothing. There
/// used to be three, and two of them did exactly that.
///
/// A sweep ring stood for "working" and never swept — the angle was fixed for
/// the sake of a capture, so a working agent showed a frozen ring on a real
/// phone. Setting it turning was the wrong repair. The mark answers "is this
/// worth opening", and working is the state where the answer is no; drawn at
/// the same size and weight as a demand, it made the least actionable rows the
/// most eye-catching, and a fleet of a dozen agents a screen of pinwheels.
///
/// A dashed circle stood for "unknown", hollow so that it would not claim
/// knowledge the app lacks. Good instinct, wired to almost nothing: the phone
/// is sent an attention the core has already degraded, and it degrades to
/// unknown when the machine is offline or when a Claude turn's working
/// inference has expired. So the circle meant "your machine is offline" and
/// declined to say so.
///
/// What is left is the demand, in the one colour this app reserves for it.
/// Every other state is a word on the row — `Idle`, `Working`,
/// `Finished · 4 files · +118 −40`, `studio offline` — which is more precise
/// than a glyph and readable without having learnt a vocabulary first.
public struct AttentionMark: View {
    private let attention: Attention
    private let size: CGFloat

    public init(attention: Attention, size: CGFloat = 19) {
        self.attention = attention
        self.size = size
    }

    public var body: some View {
        // The space is held whatever the state, so a row with nothing to
        // demand lines its name up with the rows that do.
        switch attention {
        case .idle, .working, .unknown:
            Color.clear.frame(width: size, height: size)
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

/// The accent dot a list row carries beside its age while its agent is
/// waiting on you.
///
/// Small and on the trailing edge, where a conversation list marks what is
/// unread, rather than a disc in front of the name: a disc there needed a slot
/// held open on every row that had nothing to show, and a list of calm agents
/// became a column of gaps.
public struct NeedsYouDot: View {
    @Environment(\.design) private var design

    public init() {}

    public var body: some View {
        Circle()
            .fill(design.accent.color)
            .frame(width: 8, height: 8)
            .accessibilityHidden(true)
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
                            .padding(
                                .leading,
                                prominence == .subject
                                    ? 13 : design.surfaces.separation == .rule ? 0 : 46)
                    }
                }
            }
        }
    }
}

/// A settings-style row: a label, its current value, and somewhere to go.
///
/// Interaction stays outside this view so the same production presentation
/// can be used for a button or for a fact that has nowhere deeper to open.
public struct FieldRow: View {
    @Environment(\.design) private var design
    private let label: String
    private let value: String?
    private let mono: Bool
    private let glyph: String?
    private let chevron: Bool
    private let tint: Color?

    public init(
        label: String,
        value: String? = nil,
        mono: Bool = false,
        glyph: String? = nil,
        chevron: Bool = true,
        tint: Color? = nil
    ) {
        self.label = label
        self.value = value
        self.mono = mono
        self.glyph = glyph
        self.chevron = chevron
        self.tint = tint
    }

    public var body: some View {
        HStack(spacing: 10) {
            if let glyph {
                Image(systemName: glyph)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 20)
            }
            Text(label)
                .designFont(.body, design)
                .foregroundStyle(tint ?? design.ink.color)
            Spacer(minLength: 10)
            if let value {
                Text(value)
                    .designFont(mono ? .mono : .body, design)
                    .foregroundStyle(design.inkMuted.color)
                    .lineLimit(1)
                    .truncationMode(.head)
            }
            if chevron {
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 13)
        .frame(minHeight: 44)
        .contentShape(Rectangle())
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
                    Color.clear.frosted(Circle(), as: .glass)
                }
            }
            .contentShape(Circle())
    }
}

/// The way back from a pushed screen.
///
/// The selected presentation uses the same compact tinted label everywhere.
/// The action and accessibility name remain explicit because production
/// screens navigate real state rather than depicting a static destination.
public struct BackLink: View {
    @Environment(\.design) private var design
    private let title: String
    private let identifier: String
    private let spokenLabel: String
    private let action: @MainActor () -> Void

    public init(
        _ title: String,
        identifier: String,
        accessibilityLabel: String? = nil,
        action: @escaping @MainActor () -> Void
    ) {
        self.title = title
        self.identifier = identifier
        spokenLabel = accessibilityLabel ?? "Back to \(title)"
        self.action = action
    }

    public var body: some View {
        Button(action: action) {
            HStack(spacing: 3) {
                Image(systemName: "chevron.left")
                    .font(.system(size: 15, weight: .semibold))
                Text(title)
                    .designFont(.body, design)
            }
            .foregroundStyle(design.accent.color)
            .thumbTarget(x: 1, y: 13)
        }
        .buttonStyle(.amuxControl)
        .accessibilityLabel(spokenLabel)
        .identified(identifier, label: spokenLabel)
        .reclaimingThumbTarget(x: 1, y: 13)
    }
}

/// A screen's primary action on glass above the home indicator.
///
/// Kept separate from the full-height screen so scrolling content can run
/// behind it and every flow gets the same reachable geometry.
public struct BottomAction<Content: View>: View {
    @Environment(\.design) private var design
    private let content: Content

    public init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    public var body: some View {
        VStack(spacing: 0) { content }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .frosted(RoundedRectangle(
                cornerRadius: design.metrics.floatRadius,
                style: .continuous))
            .padding(.horizontal, 12)
            .padding(.bottom, 10)
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

/// How a key is written where a person has to read it.
///
/// A fingerprint is sixty-four hex characters and the only thing anybody does
/// with one is compare it against the same key written somewhere else, so how
/// it is set is the whole of whether that comparison is possible.
public enum Fingerprint {
    /// In fours.
    ///
    /// An unbroken run of sixty-four is where an eye loses its place; in fours
    /// the comparison is short hops. The characters and their order are
    /// untouched, so what is on screen is still the fingerprint.
    public static func grouped(_ fingerprint: String) -> String {
        stride(from: 0, to: fingerprint.count, by: 4).map { start in
            String(Array(fingerprint)[start..<min(start + 4, fingerprint.count)])
        }.joined(separator: " ")
    }

    /// The first four characters and the last four, with the middle said to be
    /// missing rather than merely absent.
    ///
    /// For a row that names a key rather than asks about one. Four and four is
    /// what somebody can hold in their head while glancing between two
    /// screens, and it is not a comparison — anywhere a key is actually being
    /// decided about, the whole of it is shown.
    public static func short(_ fingerprint: String) -> String {
        guard fingerprint.count > 11 else { return fingerprint }
        return "\(fingerprint.prefix(4))…\(fingerprint.suffix(4))"
    }
}
