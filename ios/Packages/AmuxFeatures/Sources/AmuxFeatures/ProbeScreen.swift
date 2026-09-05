import AmuxDesign
import SwiftUI

/// Not a screen of the app: the target the capture harness proves itself on.
///
/// It is deliberately made of the parts a real screen is made of — the ground,
/// a glass surface, the bundled display and mono faces, ink at three strengths
/// and the accent — so that a capture which renders this correctly is evidence
/// that a capture of a real screen would render too, and so that changing one
/// token visibly changes this image.
public struct ProbeScreen: View {
    @Environment(\.design) private var design

    public init() {}

    public var body: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: design.metrics.feedGap) {
                Text("Probe")
                    .designFont(.screenTitle, design)
                    .foregroundStyle(design.ink.color)
                    .identified("probe.title")
                Surface(prominence: .subject) {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("amux/probe")
                            .designFont(.identifier, design)
                            .foregroundStyle(design.ink.color)
                            .identified("probe.identifier")
                        Text("Every token this screen draws is one the app draws.")
                            .designFont(.body, design)
                            .foregroundStyle(design.inkMuted.color)
                            .identified("probe.body")
                        Text("waiting")
                            .designFont(.caption, design)
                            .foregroundStyle(design.accent.color)
                            .identified("probe.accent")
                    }
                    .padding(design.metrics.rowPadding)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                Text("$ amux run probe")
                    .designFont(.mono, design)
                    .foregroundStyle(design.inkFaint.color)
                    .identified("probe.mono")
            }
            .padding(design.metrics.gutter)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
        .identified("probe")
    }
}
