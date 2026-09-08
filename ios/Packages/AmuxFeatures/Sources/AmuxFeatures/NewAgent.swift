import AmuxCore
import AmuxDesign
import SwiftUI

/// What somebody did on New Agent. Like every other screen it decides nothing
/// and reaches nothing: choosing a machine means asking that machine what it
/// has to offer, and starting an agent means a request leaving the phone, and
/// both of those belong to whoever owns the connection.
public enum NewAgentAction: Equatable, Sendable {
    /// Start on this machine instead. What the last machine offered goes with
    /// it — a directory on one machine names nothing on another.
    case point(HostId)
    /// Ask the machine again with what is being searched for. Only sent for a
    /// machine whose first answer stopped at the limit.
    case search
    /// Start it.
    case start
    /// Left without starting anything.
    case cancel
}

/// Starting an agent: one machine, one directory, one layer.
///
/// Everything is on one screen and nothing is a step. Which machine, where,
/// and what runs there are three answers to one question, and a person who has
/// changed their mind about the machine has usually changed their mind about
/// the directory as well — putting them on separate pages would mean walking
/// back through a wizard to say so.
///
/// The button at the foot names the machine. It is the last thing read before
/// something is started somewhere else, and "Start" alone would leave that
/// unstated on the one control where it matters.
public struct NewAgent: View {
    @Environment(\.design) private var design
    private let model: NewAgentStore
    private let hosts: HostsStore
    private let actions: @MainActor (NewAgentAction) -> Void

    public init(
        model: NewAgentStore, hosts: HostsStore,
        actions: @escaping @MainActor (NewAgentAction) -> Void
    ) {
        self.model = model
        self.hosts = hosts
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            VStack(spacing: 0) {
                bar
                middle
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            foot
            // The chooser opens over the screen rather than beside it: what is
            // being chosen is one field of the thing being built, and going
            // somewhere else to choose it would take the machine, the layer
            // and the button that starts it off the screen.
            if model.browsing {
                DirectorySheet(model: model, actions: actions)
                    .transition(.move(edge: .bottom))
            }
        }
        // A screen is a container of the things on it, not a name for all of
        // them. Without this the system spreads this identifier over every
        // element underneath — the title, the buttons, the rows — so
        // everything on the screen answers to the screen's own name, for
        // VoiceOver and for anything driving the app alike.
        .accessibilityElement(children: .contain)
        .identified("new-agent", value: model.directory)
    }

    /// The scrolling middle, split out because the screen's own body already
    /// carries the bar, the foot and the chooser.
    private var middle: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                machines
                directory
                layers
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 10)
            // Clear of the tray at the foot, which floats over this.
            .padding(.bottom, 130)
        }
        .scrollIndicators(.hidden)
    }

    // MARK: - The bar

    private var bar: some View {
        ZStack {
            Text("New Agent")
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
                .identified("new-agent.title", value: "New Agent")
            HStack {
                Button { actions(.cancel) } label: {
                    HStack(spacing: 3) {
                        Image(systemName: "chevron.left")
                            .font(.system(size: 17, weight: .semibold))
                        Text("Agents")
                            .designFont(.body, design)
                    }
                    .foregroundStyle(design.accent.color)
                    .thumbTarget(y: 13)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Back to Agents")
                .identified("new-agent.back", label: "Back to Agents")
                .reclaimingThumbTarget(y: 13)
                Spacer()
            }
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.vertical, 10)
    }

    // MARK: - Which machine

    /// The machines this phone is paired with, whether or not they are
    /// answering.
    ///
    /// An unreachable one is on the list and cannot be chosen. Leaving it out
    /// would be this screen deciding that a machine somebody paired with does
    /// not exist; showing it disabled says the true thing, which is that it is
    /// away and nothing can be started on it until it is back.
    private var machines: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "Host")
            if hosts.hosts.isEmpty {
                Explain("Pair a machine before starting an agent.")
                    .identified("new-agent.no-hosts")
            } else {
                RowGroup(items: hosts.hosts, prominence: .subject) { host in
                    machine(host)
                }
            }
        }
    }

    private func machine(_ host: HostEntry) -> some View {
        Button { actions(.point(host.id)) } label: {
            HStack(spacing: 12) {
                Radio(chosen: model.machine == host.id)
                    .opacity(host.online ? 1 : 0.4)
                VStack(alignment: .leading, spacing: 2) {
                    Text(host.name)
                        .designFont(.identifier, design)
                        .foregroundStyle(host.online ? design.ink.color : design.inkFaint.color)
                    Text(reach(host))
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 6)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 11)
            .frame(minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!host.online)
        .accessibilityLabel(spoken(host))
        .accessibilityAddTraits(model.machine == host.id ? [.isSelected] : [])
        .identified(
            "new-agent.host.\(host.id)", label: spoken(host),
            value: model.machine == host.id ? "chosen" : "not chosen", enabled: host.online)
    }

    /// "macOS · via relay", or the sentence that says why this row cannot be
    /// chosen. The refusal is on the row it is about rather than under the
    /// group, because it is the reason this one row is grey.
    private func reach(_ host: HostEntry) -> String {
        var parts: [String] = []
        if let platform = host.platform { parts.append(platform) }
        parts.append(host.online ? "via relay" : "offline, cannot start here")
        return parts.joined(separator: " · ")
    }

    private func spoken(_ host: HostEntry) -> String {
        var parts = [host.name]
        if let platform = host.platform { parts.append(platform) }
        parts.append(host.online ? "reachable via relay" : "offline, cannot start here")
        return parts.joined(separator: ", ")
    }

    // MARK: - Where

    /// Where the agent will start, and the directories this machine was used
    /// in most recently under it.
    ///
    /// One row and a handful of chips rather than a list: the answer is almost
    /// always somewhere this machine has been used before, and the whole
    /// enumeration is one tap away for the times it is not.
    private var directory: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "Directory")
            Surface(prominence: .subject) {
                Button { model.browsing = true } label: {
                    HStack(spacing: 11) {
                        Image(systemName: "folder")
                            .font(.system(size: 15, weight: .medium))
                            .foregroundStyle(design.inkMuted.color)
                            .frame(width: 22)
                        Text(model.directory.isEmpty ? "Choose a directory" : model.directory)
                            .designFont(.mono, design)
                            .foregroundStyle(
                                model.directory.isEmpty ? design.inkFaint.color : design.ink.color)
                            .lineLimit(1)
                            .truncationMode(.head)
                        Spacer(minLength: 6)
                        Image(systemName: "chevron.right")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(design.inkFaint.color)
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
                    .frame(minHeight: 44)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(model.machine == nil)
                .accessibilityLabel("Directory")
                .identified(
                    "new-agent.directory", label: "Directory",
                    value: model.directory, enabled: model.machine != nil)
            }
            if !chips.isEmpty { recent }
            if model.listing == .unavailable {
                Explain("\(machineName) could not list its projects. Type a path instead.")
                    .identified("new-agent.directory.unavailable")
            }
            if let failure = model.failure {
                Refusal(text: failure)
                    .identified("new-agent.refusal", value: failure)
            }
        }
    }

    /// The other directories this machine was used in recently — the chosen one
    /// is in the row above and is not repeated as a chip.
    private var chips: [Project] {
        model.recent.filter { $0.path != model.directory }.prefix(4).map { $0 }
    }

    private var recent: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 8) {
                ForEach(chips) { project in
                    Button { model.choose(directory: project.path) } label: {
                        Text(project.name)
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.ink.color)
                            .padding(.horizontal, 14)
                            .padding(.vertical, 9)
                            .background(Capsule().fill(design.sunken.color))
                            .thumbTarget(y: 6)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Start in \(project.name)")
                    .identified(
                        "new-agent.recent.\(project.name)", label: "Start in \(project.name)",
                        value: project.path)
                    .reclaimingThumbTarget(y: 6)
                }
            }
            .padding(.vertical, 1)
        }
        .scrollIndicators(.hidden)
    }

    // MARK: - What runs there

    private var layers: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "Agent")
            HStack(spacing: 12) {
                ForEach(NewAgentStore.Provider.allCases) { provider in
                    LayerCard(
                        provider: provider, chosen: model.provider == provider,
                        model: modelLine(provider), choices: choices(provider),
                        choose: { model.choose(provider: provider) },
                        chooseModel: { model.choose(model: $0) })
                }
            }
        }
    }

    /// What this layer will start under.
    ///
    /// Claude's is always the machine's own default: a create request names
    /// Claude's driver and nothing else, so a model chosen here would be a
    /// choice this app silently dropped. Codex's is whatever was chosen, or the
    /// same default where nothing was.
    private func modelLine(_ provider: NewAgentStore.Provider) -> String {
        switch provider {
        case .claude: "host default"
        case .codex: model.model ?? "host default"
        }
    }

    /// The models this card can offer.
    ///
    /// Nothing tells a phone what models a layer has until that layer is
    /// running, so the only honest list is the one the sessions on this account
    /// already reported. Claude offers none because its create request cannot
    /// carry one.
    private func choices(_ provider: NewAgentStore.Provider) -> [ModelInfo] {
        provider == .codex ? model.codexModels : []
    }

    // MARK: - Starting it

    private var machineName: String {
        model.machine.flatMap { hosts.host($0)?.name } ?? "the machine"
    }

    private var foot: some View {
        VStack {
            Spacer(minLength: 0)
            Button { actions(.start) } label: {
                ActionLabel(model.starting ? "Starting…" : "Start on \(machineName)", fill: true)
            }
            .buttonStyle(.plain)
            .disabled(!model.ready)
            .opacity(model.ready ? 1 : 0.4)
            // On the button and not on the bar it sits in: the bar is pinned
            // to the foot of a full-height stack, so a name given to it covers
            // everything from here to the top of the page and a finger aimed
            // at the middle of what that name covers lands nowhere near the
            // one thing on it anybody presses.
            .identified(
                "new-agent.start", label: "Start on \(machineName)",
                value: model.starting ? "starting" : "ready", enabled: model.ready)
            .padding(14)
            .frame(maxWidth: .infinity)
            .frosted(RoundedRectangle(
                cornerRadius: design.metrics.floatRadius, style: .continuous))
            .padding(.horizontal, design.metrics.gutter)
            .padding(.bottom, 8)
        }
    }
}

/// One layer, as a card that is chosen whole.
///
/// The model sits inside the card rather than beside the two, because it is a
/// fact about that layer: the model Codex runs under means nothing to Claude.
private struct LayerCard: View {
    @Environment(\.design) private var design
    let provider: NewAgentStore.Provider
    let chosen: Bool
    let model: String
    let choices: [ModelInfo]
    let choose: @MainActor () -> Void
    let chooseModel: @MainActor (String?) -> Void
    /// Whether the list of models is up.
    @State private var picking = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Image(systemName: glyph)
                .font(.system(size: 19, weight: .medium))
                .foregroundStyle(chosen ? design.accent.color : design.inkMuted.color)
            Text(provider.title)
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            models
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background {
            let shape = RoundedRectangle(
                cornerRadius: design.metrics.cardRadius, style: .continuous)
            ZStack {
                shape.fill(design.raised.color)
                shape.strokeBorder(
                    chosen ? design.accent.color : design.hairline.color,
                    lineWidth: chosen ? 2 : design.metrics.hairline)
            }
        }
        .contentShape(RoundedRectangle(
            cornerRadius: design.metrics.cardRadius, style: .continuous))
        .onTapGesture(perform: choose)
        .accessibilityElement(children: .contain)
        .accessibilityAddTraits(chosen ? [.isSelected] : [])
        .identified(
            "new-agent.provider.\(provider.rawValue)",
            label: "\(provider.title), \(model)", value: chosen ? "chosen" : "not chosen")
    }

    /// The model line. A chevron only where there is a list behind it: one that
    /// opened on nothing would be an offer this screen cannot keep.
    @ViewBuilder
    private var models: some View {
        if choices.isEmpty {
            line
                .identified("new-agent.model.\(provider.rawValue)", value: model)
        } else {
            // A button and a list of choices rather than a `Menu`, which draws
            // the same line but puts two controls in the accessibility tree:
            // its own, and a second one inside it that answers to nothing
            // stated out here — no name, no identifier — so somebody using
            // VoiceOver meets a control with nothing to read out and no way to
            // guess what it does. Neither hiding it, naming it from inside nor
            // combining the pair reaches it.
            Button { picking = true } label: {
                HStack(spacing: 4) {
                    line
                    Image(systemName: "chevron.down")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(design.inkFaint.color)
                }
                .thumbTarget(y: 15)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Model for \(provider.title)")
            .identified(
                "new-agent.model.\(provider.rawValue)",
                label: "Model for \(provider.title)", value: model)
            .reclaimingThumbTarget(y: 15)
            .confirmationDialog(
                "Model for \(provider.title)", isPresented: $picking,
                titleVisibility: .visible
            ) {
                Button("host default") { chooseModel(nil) }
                ForEach(choices, id: \.id) { choice in
                    Button(choice.name) { chooseModel(choice.id) }
                }
            }
        }
    }

    private var line: some View {
        Text(model)
            .designFont(.monoSmall, design)
            .foregroundStyle(design.inkMuted.color)
            .lineLimit(1)
    }

    private var glyph: String {
        switch provider {
        case .claude: "asterisk"
        case .codex: "chevron.left.forwardslash.chevron.right"
        }
    }
}

/// Something the machine refused, in the machine's own words.
private struct Refusal: View {
    @Environment(\.design) private var design
    let text: String

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "exclamationmark.triangle")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(design.inkMuted.color)
            Text(text)
                .designFont(.detail, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                .fill(design.sunken.color)
        }
        .accessibilityElement(children: .combine)
    }
}

/// The directories a machine has to offer, and a field for one it does not.
///
/// The typed path is under the list rather than instead of it. A machine that
/// would not enumerate anything still runs agents, and a directory that is not
/// a repository is an ordinary thing to want; the list is the shortcut and the
/// field is the answer to everything it does not cover.
private struct DirectorySheet: View {
    @Environment(\.design) private var design
    let model: NewAgentStore
    let actions: @MainActor (NewAgentAction) -> Void

    var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 0)
            panel
        }
        .background(alignment: .top) {
            Color.black.opacity(0.28)
                .ignoresSafeArea()
                .onTapGesture { model.browsing = false }
        }
    }

    private var panel: some View {
        VStack(alignment: .leading, spacing: 12) {
            Capsule()
                .fill(design.hairline.color)
                .frame(width: 40, height: 5)
                .frame(maxWidth: .infinity)
            HStack(alignment: .firstTextBaseline) {
                Text("Directory")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                Spacer(minLength: 8)
                Button { model.browsing = false } label: {
                    ActionLabel("Done", kind: .plain)
                }
                .buttonStyle(.plain)
                .identified("new-agent.browse.done", label: "Done")
            }
            search
            ViewThatFits(in: .vertical) {
                listing
                ScrollView { listing }.scrollIndicators(.hidden)
            }
            path
        }
        .padding(16)
        .frame(maxWidth: .infinity)
        .background {
            UnevenRoundedRectangle(
                topLeadingRadius: design.metrics.cardRadius,
                topTrailingRadius: design.metrics.cardRadius, style: .continuous)
                .fill(design.raised.color)
                .ignoresSafeArea(edges: .bottom)
        }
        .accessibilityElement(children: .contain)
        .identified("new-agent.browse", value: model.query)
    }

    @ViewBuilder
    private var search: some View {
        if model.listing != .unavailable {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(design.inkFaint.color)
                TextField("Search repositories", text: Binding(
                    get: { model.query },
                    set: { typed in
                        model.query = typed
                        actions(.search)
                    }))
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.sunken.color)
            }
            .identified("new-agent.search", label: "Search repositories", value: model.query)
        }
    }

    @ViewBuilder
    private var listing: some View {
        VStack(alignment: .leading, spacing: 12) {
            if !model.recent.isEmpty && model.query.isEmpty {
                SectionHead(title: "Recent")
                RowGroup(items: model.recent, prominence: .subject) { project in
                    row(project, recent: true)
                }
            }
            switch model.listing {
            case .asking:
                Explain("Reading this machine's projects…")
                    .identified("new-agent.browse.reading")
            case .unavailable:
                Explain("This machine could not list its projects.")
                    .identified("new-agent.browse.unavailable")
            case .none, .ready:
                if !model.found.isEmpty {
                    SectionHead(title: model.query.isEmpty ? "Repositories" : "Found")
                    RowGroup(items: model.found, prominence: .subject) { project in
                        row(project, recent: false)
                    }
                } else if model.listing == .ready {
                    Explain(nothing)
                        .identified("new-agent.browse.none")
                }
            }
        }
    }

    /// What "no repositories" means here: only useful beside where the machine
    /// looked, which is the roots it reported.
    private var nothing: String {
        guard model.query.isEmpty else { return "Nothing under this machine's roots matches." }
        guard !model.roots.isEmpty else { return "This machine listed no repositories." }
        return "No repositories under \(model.roots.joined(separator: ", "))."
    }

    private func row(_ project: Project, recent: Bool) -> some View {
        Button {
            model.choose(directory: project.path)
            model.browsing = false
        } label: {
            HStack(spacing: 11) {
                Image(systemName: recent ? "clock" : "folder")
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(design.inkFaint.color)
                    .frame(width: 20)
                VStack(alignment: .leading, spacing: 2) {
                    Text(project.name)
                        .designFont(.identifier, design)
                        .foregroundStyle(design.ink.color)
                    Text(project.path)
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                Spacer(minLength: 6)
                if model.directory == project.path {
                    Image(systemName: "checkmark")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(design.accent.color)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .frame(minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(project.path)
        .identified(
            "new-agent.project.\(project.name)", label: project.path,
            value: model.directory == project.path ? "chosen" : "")
    }

    /// A path typed by hand, for a directory the machine did not list. The
    /// machine has the last word on whether it exists — this phone has no way
    /// to know — so it is taken as written and the refusal, if there is one,
    /// comes back from the machine in its own words.
    private var path: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "Or a path")
            HStack(spacing: 8) {
                TextField("~/somewhere/else", text: Binding(
                    get: { model.typed }, set: { model.typed = $0 }))
                    .designFont(.mono, design)
                    .foregroundStyle(design.ink.color)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .background {
                        RoundedRectangle(
                            cornerRadius: design.metrics.controlRadius, style: .continuous)
                            .fill(design.sunken.color)
                    }
                    .identified("new-agent.typed", label: "Or a path", value: model.typed)
                Button {
                    model.choose(directory: model.typedPath)
                    model.browsing = false
                } label: {
                    ActionLabel("Use", kind: .outline)
                }
                .buttonStyle(.plain)
                .disabled(model.typedPath.isEmpty)
                .identified(
                    "new-agent.typed.use", label: "Use this path",
                    enabled: !model.typedPath.isEmpty)
            }
        }
    }
}
