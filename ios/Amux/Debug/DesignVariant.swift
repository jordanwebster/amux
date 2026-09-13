import AmuxDesign
import SwiftUI

/// The alternatives compiled together for a native design-round benchmark.
///
/// This file is excluded from Release with the rest of the driving door. The
/// Two ideas move one shared layout token each; the fourth adds a separate
/// context band so the batch includes a structural alternative.
enum DesignVariant: String, CaseIterable {
    case production
    case tightGutter = "tight-gutter"
    case largeTitle = "large-title"
    case contextBand = "context-band"

    init?(name: String?) {
        self.init(rawValue: name ?? Self.production.rawValue)
    }

    var design: Design {
        switch self {
        case .production, .contextBand:
            .app
        case .tightGutter:
            Self.alternative(gutter: 14, titleSize: 26)
        case .largeTitle:
            Self.alternative(gutter: 18, titleSize: 30)
        }
    }

    private static func alternative(gutter: CGFloat, titleSize: CGFloat) -> Design {
        let source = Design.app
        return Design(
            name: source.name,
            ground: source.ground,
            raised: source.raised,
            sunken: source.sunken,
            hairline: source.hairline,
            ink: source.ink,
            inkMuted: source.inkMuted,
            inkFaint: source.inkFaint,
            accent: source.accent,
            onAccent: source.onAccent,
            added: source.added,
            removed: source.removed,
            faces: source.faces,
            metrics: Design.Metrics(
                cardRadius: source.metrics.cardRadius,
                controlRadius: source.metrics.controlRadius,
                floatRadius: source.metrics.floatRadius,
                rowPadding: source.metrics.rowPadding,
                gutter: gutter,
                rowGap: source.metrics.rowGap,
                feedGap: source.metrics.feedGap,
                hairline: source.metrics.hairline),
            type: Design.Typography(
                identifierIsMono: source.type.identifierIsMono,
                titleWeight: source.type.titleWeight,
                titleSize: titleSize,
                bodySize: source.type.bodySize,
                tightTracking: source.type.tightTracking),
            surfaces: source.surfaces)
    }
}

/// A structure-changing idea around the same production shell. It is a debug
/// experiment, not another home implementation: the ordinary shell and home
/// remain the content being measured and Release never compiles this wrapper.
struct DesignVariantLayout: ViewModifier {
    @Environment(\.design) private var design
    let variant: DesignVariant

    func body(content: Content) -> some View {
        if variant == .contextBand {
            content.safeAreaInset(edge: .top, spacing: 0) {
                HStack {
                    Text("LOCAL FLEET")
                        .designFont(.sectionTitle, design)
                    Spacer()
                    Text("3 HOSTS")
                        .designFont(.caption, design)
                }
                .foregroundStyle(design.inkFaint.color)
                .padding(.horizontal, design.metrics.gutter)
                .padding(.vertical, 10)
                .background(design.sunken.color)
            }
        } else {
            content
        }
    }
}
