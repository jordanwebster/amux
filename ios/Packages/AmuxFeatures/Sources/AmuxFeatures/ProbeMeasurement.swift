import AmuxCore
import AmuxDesign
import SwiftUI

/// What the performance harness puts on screen.
///
/// Two different things live here. The probe home is not a screen of the app:
/// it is deliberately plain — a row is a name and a line of text — so that the
/// cold-start number taken over it is a floor. Whatever the designed home
/// costs, it costs at least this, and a regression there is the list machinery
/// rather than a decoration. The bench conversation is the opposite: it is the
/// shipped page, whole, because the streaming budget is a claim about the
/// screen people actually read.
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

/// The shipped conversation, put on screen so it can be measured.
///
/// Not a stand-in and not a part of one: this is the page the app pushes when
/// somebody opens an agent — the fleet's drawer over the conversation, and
/// inside it the same chrome, the same lazy transcript, the same facts strip
/// and the same composer, projected by the same code. An earlier bench drew
/// the feed alone inside the conversation's scroll container, which was enough
/// to say the list was lazy but not enough to certify the product: the strip,
/// the foot and the composer are laid out on every frame the stream causes,
/// and a number taken without them is a number about a screen nobody uses.
///
/// The drawer is shut and stays shut. Shut is the state a conversation is read
/// in, and it is still built and still laid out beside the page, so what it
/// costs is inside the measurement rather than excluded from it.
public struct BenchConversationScreen: View {
    private let model: ConversationStore
    private let fleet: FleetStore
    private let hosts: HostsStore
    private let subject: ConversationSubject
    private let drew: (@Sendable ([IdentifiedElement]) -> Void)?

    /// `drew`, when it is given, is handed everything on the page that named
    /// itself, every time that changes. Only a view that was built can name
    /// itself, so this is how a measurement can say the list drew a screenful
    /// of a thousand rows rather than a thousand of them, and that the folded
    /// runs among them were still folded.
    ///
    /// Left out, nothing observes the preference at all. Observing it is not
    /// free — the names are gathered up the tree every time a row changes —
    /// and a measurement of how a list behaves under a stream must not be a
    /// measurement of the instrument watching it.
    public init(
        model: ConversationStore,
        fleet: FleetStore,
        hosts: HostsStore,
        subject: ConversationSubject,
        drew: (@Sendable ([IdentifiedElement]) -> Void)? = nil
    ) {
        self.model = model
        self.fleet = fleet
        self.hosts = hosts
        self.subject = subject
        self.drew = drew
    }

    public var body: some View {
        watched(page)
            .identified("bench.conversation", value: "\(model.entries.count)")
    }

    private var page: some View {
        DrawerOverlay(
            open: .constant(false),
            drawer: AgentsDrawer(
                model: fleet, hosts: hosts, current: model.agent) { _ in }
        ) {
            Conversation(
                model: model, subject: subject,
                naming: { fleet.name(of: $0) }) { _ in }
        }
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
