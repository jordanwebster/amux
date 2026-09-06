import AmuxCore
import AmuxDesign
import SwiftUI

/// What the performance harness puts on screen.
///
/// Two different things live here. The probe home is not a screen of the app:
/// it is deliberately plain — a row is a name and a line of text — so that the
/// cold-start number taken over it is a floor. Whatever the designed home
/// costs, it costs at least this, and a regression there is the list machinery
/// rather than a decoration. The bench transcript is the opposite: it is the
/// shipped transcript, in a container that only decides where the list rests,
/// because the streaming budget is a claim about the rows people actually
/// read.
public struct ProbeHomeScreen: View {
    @Environment(\.design) private var design
    private let rows: [AgentRow]

    public init(rows: [AgentRow]) {
        self.rows = rows
    }

    public var body: some View {
        ZStack {
            Ground()
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(rows) { row in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(row.name)
                                .designFont(.identifier, design)
                                .foregroundStyle(design.ink.color)
                            Text(row.card.agent.workingOn?.text ?? row.workingDirectory)
                                .designFont(.caption, design)
                                .foregroundStyle(design.inkMuted.color)
                        }
                        .padding(design.metrics.rowPadding)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .identified("probe.home.row.\(row.id.description)", label: row.name)
                    }
                }
            }
            .padding(.horizontal, design.metrics.gutter)
        }
        .identified("probe.home", value: "\(rows.count)")
    }
}

/// The shipped transcript, put on screen so it can be measured.
///
/// Not a stand-in: these are the rows the conversation draws, projected by the
/// same `transcriptRows()` and laid out by the same lazy stack, so a number
/// taken here is a number about the product's list rather than about a plainer
/// one standing in for it.
///
/// The scroll view around them is this bench's, and it copies the
/// conversation's — the same stack, the same bottom padding, the same full
/// width, the same hidden indicators — with one addition: it rests at the
/// bottom. Streaming is defined as rows arriving while the list follows its
/// tail, and a row appended below the fold of a lazy stack is never built, so
/// without an anchor the measurement would be of a list nobody is looking at.
/// The shipped conversation has no anchor because where a conversation rests
/// when you open it is a product question nobody has answered yet.
public struct BenchTranscriptScreen: View {
    private let model: ConversationStore
    private let drew: (@Sendable ([IdentifiedElement]) -> Void)?

    /// `drew`, when it is given, is handed everything under the scroll view
    /// that named itself, every time that changes. Only a view that was built
    /// can name itself, so this is how a measurement can say the list drew a
    /// screenful of a thousand rows rather than a thousand of them, and that
    /// the folded runs among them were still folded.
    ///
    /// Left out, nothing observes the preference at all. Observing it is not
    /// free — the names are gathered up the tree every time a row changes —
    /// and a measurement of how a list behaves under a stream must not be a
    /// measurement of the instrument watching it.
    public init(
        model: ConversationStore, drew: (@Sendable ([IdentifiedElement]) -> Void)? = nil
    ) {
        self.model = model
        self.drew = drew
    }

    public var body: some View {
        ZStack {
            Ground()
            watched(transcript)
        }
        .identified("bench.transcript", value: "\(model.entries.count)")
    }

    private var transcript: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                TranscriptFeed(rows: model.entries.transcriptRows())
            }
            .padding(.bottom, 120)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .scrollIndicators(.hidden)
        // The tail is followed by resting there rather than by asking to
        // scroll to the last row on every arrival: an explicit scroll makes
        // the stack measure everything above it, which at a thousand rows
        // costs more than drawing the frame does.
        .defaultScrollAnchor(.bottom)
    }

    @ViewBuilder
    private func watched(_ content: some View) -> some View {
        if let drew {
            content.onPreferenceChange(IdentifiedElements.self) { drew($0) }
        } else {
            content
        }
    }
}
