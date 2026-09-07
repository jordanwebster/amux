import AmuxCore
import AmuxDesign
import SwiftUI

/// What happened on the Hosts tab. The screen navigates nowhere and reaches
/// nothing; it says what the person did and the shell decides where that leads.
public enum HostsAction: Equatable, Sendable {
    case open(HostId)
    case pair
    case newAgent
}

/// The machines agents run on.
///
/// Each row says what the machine is and how this phone is reaching it,
/// because that is the explanation for latency a person would otherwise blame
/// on the app: a relay hop feels different from a link on the same desk, and
/// there is nothing the app can do about it except say so.
///
/// Reachable and unreachable are separate groups rather than one list with
/// grey rows. A machine you cannot use is not a worse version of one you can —
/// nothing you start on it will run — and the agents already on it are in a
/// state nobody can report, which is said once under the group rather than
/// implied on every row.
public struct HostsTab: View {
    @Environment(\.design) private var design
    private let model: HostsStore
    private let actions: @MainActor (HostsAction) -> Void

    public init(model: HostsStore, actions: @escaping @MainActor (HostsAction) -> Void) {
        self.model = model
        self.actions = actions
    }

    public var body: some View {
        ZStack {
            Ground()
            VStack(alignment: .leading, spacing: 0) {
                header
                list
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
        .identified("hosts", value: subtitle)
    }

    // MARK: - Header

    private var header: some View {
        HStack(alignment: .center, spacing: 10) {
            VStack(alignment: .leading, spacing: 1) {
                Text("Hosts")
                    .designFont(.screenTitle, design)
                    .foregroundStyle(design.ink.color)
                    .identified("hosts.title", value: "Hosts")
                Text(subtitle)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
                    .identified("hosts.subtitle", value: subtitle)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            // Pairing, not New Agent. The plus on a screen adds one of the
            // things the screen lists, and what this screen lists is machines.
            Button { actions(.pair) } label: {
                GlassIcon(glyph: "plus", prominent: true)
            }
            .accessibilityLabel("Pair a Machine")
            .identified("hosts.pair", label: "Pair a Machine")
        }
        .padding(.horizontal, design.metrics.gutter)
        .padding(.vertical, 10)
    }

    /// "3 reachable · 1 offline", and only the half that is true.
    private var subtitle: String {
        let reachable = model.online.count
        let lost = model.offline.count
        var parts = ["\(reachable) reachable"]
        if lost > 0 { parts.append("\(lost) offline") }
        return parts.joined(separator: " · ")
    }

    // MARK: - The list

    private var list: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                if !model.online.isEmpty {
                    group(title: "Connected", hosts: model.online, caption: nil)
                }
                if !model.offline.isEmpty {
                    group(
                        title: "Offline", hosts: model.offline,
                        caption: "Agents on an offline host report their state as unknown.")
                }
                if model.hosts.isEmpty { empty }
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 6)
            .padding(.bottom, 120)
        }
        .scrollIndicators(.hidden)
    }

    private func group(title: String, hosts: [HostEntry], caption: String?) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: title)
            RowGroup(items: hosts, prominence: .subject) { host in
                row(host)
            }
            if let caption {
                Explain(caption)
                    .identified("hosts.caption.\(title.lowercased())", value: caption)
            }
        }
    }

    /// A phone with an account but no machines yet. It is not an error and it
    /// is not empty: it is the one step that has not happened.
    private var empty: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("No machines yet")
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            Explain("Run `amux pair` on a machine and enter the code it prints.")
            Button { actions(.pair) } label: {
                ActionLabel("Pair a Machine", kind: .outline)
            }
            .buttonStyle(.plain)
            .identified("hosts.empty.pair", label: "Pair a Machine")
        }
        .identified("hosts.empty", value: "No machines yet")
    }

    private func row(_ host: HostEntry) -> some View {
        Button {
            actions(.open(host.id))
        } label: {
            HStack(spacing: 11) {
                Image(systemName: glyph(host))
                    .font(.system(size: 15, weight: .medium))
                    .foregroundStyle(host.online ? design.inkMuted.color : design.inkFaint.color)
                    .frame(width: 22)
                VStack(alignment: .leading, spacing: 2) {
                    Text(host.name)
                        .designFont(.identifier, design)
                        .foregroundStyle(design.ink.color)
                    Text(status(host))
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.inkFaint.color)
                }
                Spacer(minLength: 6)
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .frame(minHeight: 44)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(spoken(host))
        .identified(
            "hosts.row.\(host.id)", label: spoken(host),
            value: host.online ? "reachable" : "offline")
    }

    /// The machine drawn as the kind of machine it said it was. A host that
    /// has not said gets the plain computer rather than a guess, because the
    /// glyph is the first thing read and a wrong one is a wrong answer.
    private func glyph(_ host: HostEntry) -> String {
        switch host.platform {
        case "Linux": "server.rack"
        case "Windows": "pc"
        default: "desktopcomputer"
        }
    }

    /// The second line: what it is, and either how this phone is reaching it
    /// or when it stopped being reachable.
    ///
    /// "via relay" is not a guess. A phone holds one connection — to the
    /// relay — and every machine is on the far side of it; there is no direct
    /// link from a phone to a host to distinguish it from.
    private func status(_ host: HostEntry) -> String {
        var parts: [String] = []
        if let platform = host.platform { parts.append(platform) }
        if host.online {
            parts.append("via relay")
        } else if let gone = model.wentOffline(host.id) {
            parts.append("offline for \(since(gone))")
        } else {
            parts.append("offline")
        }
        return parts.joined(separator: " · ")
    }

    /// How long it has been gone, in the same units every other age in the
    /// app is written in. An age rather than a wall-clock time because the
    /// machine did not report when it went — this phone noticed, and "for 3d"
    /// stays true where "08:12" would read as this morning.
    private func since(_ moment: Date) -> String {
        Elapsed.spelled(max(0, model.now.timeIntervalSince(moment)))
    }

    /// What a row says to somebody who cannot see it, in the order the row
    /// says it: which machine, what it is, and how it stands.
    private func spoken(_ host: HostEntry) -> String {
        var parts = [host.name]
        if let platform = host.platform { parts.append(platform) }
        if host.online {
            parts.append("reachable via relay")
        } else if let gone = model.wentOffline(host.id) {
            parts.append("offline for \(since(gone))")
            parts.append("its agents' state is unknown")
        } else {
            parts.append("offline")
            parts.append("its agents' state is unknown")
        }
        return parts.joined(separator: ", ")
    }
}
