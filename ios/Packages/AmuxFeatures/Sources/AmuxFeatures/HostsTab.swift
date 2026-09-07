import AmuxCore
import AmuxDesign
import SwiftUI

/// What happened on the Hosts tab. The screen navigates nowhere and reaches
/// nothing; it says what the person did and the shell decides where that leads.
public enum HostsAction: Equatable, Sendable {
    case open(HostId)
    /// Start pairing with a machine, or — from the header, where no machine
    /// has been pointed at — with whichever one is on offer.
    case pair(HostId?)
    case newAgent
    /// Stop trusting a machine. Destructive and immediate: what it ends is
    /// the access this phone granted, not a preference.
    case revoke(HostId)
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
            // Read over the machines rather than instead of them. The keys
            // being read are the keys to the machines on the list behind, and
            // a page that replaced them would ask somebody to remember which
            // machine they came to revoke.
            if model.readingDevices {
                DevicesSheet(model: model, actions: actions)
                    .transition(.move(edge: .bottom))
            }
        }
        // A screen is a container of the things on it, not a name for all of
        // them. Without this the system spreads this identifier over every
        // element underneath — the title, the buttons, the rows — so
        // everything on the screen answers to the screen's own name, for
        // VoiceOver and for anything driving the app alike.
        .accessibilityElement(children: .contain)
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
            Button { actions(.pair(model.discovered.count == 1 ? model.discovered[0].id : nil)) } label: {
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
                if !model.discovered.isEmpty { offers }
                if model.hosts.isEmpty && model.discovered.isEmpty { empty }
                if let roster = model.roster { thisPhone(roster) }
            }
            .padding(.horizontal, design.metrics.gutter)
            .padding(.top, 6)
            .padding(.bottom, 120)
        }
        .scrollIndicators(.hidden)
    }

    /// Machines on the network this phone has not paired with.
    ///
    /// Their own section rather than grey rows among the hosts, because they
    /// are not hosts: nothing of theirs is readable and nothing can be started
    /// on them. What each one is, is an offer, so each carries the one thing
    /// there is to do with an offer.
    private var offers: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "Not Paired")
            RowGroup(items: model.discovered, prominence: .subject) { host in
                offer(host)
            }
            Explain("Run `amux pair` on one of these and enter the code it prints.")
                .identified("hosts.caption.unpaired")
        }
    }

    private func offer(_ host: HostEntry) -> some View {
        HStack(spacing: 11) {
            Image(systemName: glyph(host))
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(design.inkFaint.color)
                .frame(width: 22)
            VStack(alignment: .leading, spacing: 2) {
                Text(host.name)
                    .designFont(.identifier, design)
                    .foregroundStyle(design.ink.color)
                Text(offered(host))
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
            }
            Spacer(minLength: 6)
            Button { actions(.pair(host.id)) } label: {
                ActionLabel("Pair", kind: .outline)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Pair with \(host.name)")
            .identified("hosts.pair.\(host.id)", label: "Pair with \(host.name)")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 44)
        .accessibilityElement(children: .contain)
        .identified("hosts.offer.\(host.id)", label: spokenOffer(host), value: "not paired")
    }

    /// "Linux · not paired", and only the half that is known.
    private func offered(_ host: HostEntry) -> String {
        var parts: [String] = []
        if let platform = host.platform { parts.append(platform) }
        parts.append("not paired")
        return parts.joined(separator: " · ")
    }

    private func spokenOffer(_ host: HostEntry) -> String {
        var parts = [host.name]
        if let platform = host.platform { parts.append(platform) }
        parts.append("not paired")
        return parts.joined(separator: ", ")
    }

    // MARK: - This phone

    /// What this phone is, and how many machines hold a key to it.
    ///
    /// Last on the screen and not among the machines, because it is not one of
    /// them: the rows above are places agents run, and this is the identity
    /// this device presents to all of them. The count is a way in rather than
    /// the answer — deciding to revoke means reading a fingerprint, and a
    /// fingerprint on every row would bury the machines.
    private func thisPhone(_ roster: DeviceRoster) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionHead(title: "This Phone")
            RowGroup(items: facts(roster), prominence: .subject) { fact in
                if fact.opensDevices {
                    Button { model.readDevices() } label: { FactRow(fact: fact) }
                        .buttonStyle(.plain)
                } else {
                    FactRow(fact: fact)
                }
            }
            Explain("Revoking a device ends its access immediately.")
                .identified("hosts.caption.devices")
        }
    }

    private func facts(_ roster: DeviceRoster) -> [Fact] {
        [
            Fact(
                label: "Identity", value: identity(roster.identity), mono: true,
                opensDevices: false),
            Fact(
                label: "Paired Devices", value: "\(roster.devices.count)", mono: false,
                opensDevices: true),
        ]
    }

    /// "iPhone · 4bb0…94e0": what this phone calls itself and enough of its
    /// key to recognise, on a row that has one line for both.
    ///
    /// Elided rather than truncated by the layout, so it elides the same way
    /// at every width and type size. The whole fingerprint is one tap away, in
    /// the same place the machines' are, because ends alone are not what
    /// anybody should compare a key by.
    private func identity(_ identity: DeviceIdentity) -> String {
        "\(identity.name) · \(Fingerprint.short(identity.fingerprint))"
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
            Button { actions(.pair(model.discovered.first?.id)) } label: {
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

/// One stated fact about this phone: a label on the left and the answer on the
/// right, with a chevron only where there is somewhere to go.
private struct Fact: Identifiable, Equatable {
    var label: String
    var value: String
    /// Identities and fingerprints are compared character by character against
    /// something printed elsewhere, so they are set in the mono face the rest
    /// of the app spells identifiers in.
    var mono: Bool
    var opensDevices: Bool

    var id: String { label }
}

private struct FactRow: View {
    @Environment(\.design) private var design
    let fact: Fact

    var body: some View {
        HStack(spacing: 10) {
            Text(fact.label)
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
            Spacer(minLength: 8)
            Text(fact.value)
                .designFont(fact.mono ? .mono : .body, design)
                .foregroundStyle(fact.mono ? design.inkMuted.color : design.ink.color)
                .lineLimit(1)
                .truncationMode(.middle)
            if fact.opensDevices {
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(design.inkFaint.color)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .frame(minHeight: 44)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .identified(
            "hosts.fact.\(fact.label.lowercased().replacingOccurrences(of: " ", with: "-"))",
            label: "\(fact.label), \(fact.value)", value: fact.value)
    }
}

/// The machines this phone holds a key to, and the one thing there is to do
/// about one.
///
/// A fingerprint per row, because that is the whole reason to open this: the
/// count on the screen behind answers "how many", and the only question left
/// is which key belongs to what, which is answered by reading the fingerprint
/// against the one the machine itself prints.
private struct DevicesSheet: View {
    @Environment(\.design) private var design
    let model: HostsStore
    let actions: @MainActor (HostsAction) -> Void

    var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 0)
            panel
        }
        .background(alignment: .top) {
            // A tap outside closes it. Nothing has been withdrawn by opening
            // the list, so leaving costs nothing and does not ask.
            Color.black.opacity(0.28)
                .ignoresSafeArea()
                .onTapGesture { model.stopReadingDevices() }
        }
    }

    private var panel: some View {
        VStack(alignment: .leading, spacing: 12) {
            Capsule()
                .fill(design.hairline.color)
                .frame(width: 40, height: 5)
                .frame(maxWidth: .infinity)
            HStack(alignment: .firstTextBaseline) {
                Text("This Phone")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                Spacer(minLength: 8)
                Button { model.stopReadingDevices() } label: {
                    ActionLabel("Done", kind: .plain)
                }
                .buttonStyle(.plain)
                .identified("hosts.devices.done", label: "Done")
            }
            // Long enough to overflow on a phone paired with many machines,
            // and a whole fingerprint is what makes it long. It scrolls only
            // when it has to, so the ordinary case is a panel the size of what
            // is in it rather than one that always reaches for the screen.
            ViewThatFits(in: .vertical) {
                keys
                ScrollView { keys }.scrollIndicators(.hidden)
            }
            Explain("Revoking a device ends its access immediately.")
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
        .identified("hosts.devices.sheet", value: "\(model.devices.count)")
    }

    /// This phone's own key and every machine's, whole.
    private var keys: some View {
        VStack(alignment: .leading, spacing: 12) {
            // This phone's own key, whole. It is the one a machine shows while
            // it waits to be told to trust this device, so the place somebody
            // comes to compare keys is the place it has to be readable.
            if let identity = model.roster?.identity {
                Surface(prominence: .subject) {
                    VStack(alignment: .leading, spacing: 3) {
                        Text(identity.name)
                            .designFont(.identifier, design)
                            .foregroundStyle(design.ink.color)
                        Text(Fingerprint.grouped(identity.fingerprint))
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.inkFaint.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .accessibilityElement(children: .combine)
                    .identified(
                        "hosts.devices.identity", label: identity.name,
                        value: identity.fingerprint)
                }
            }
            SectionHead(title: "Paired Devices")
            if model.devices.isEmpty {
                Explain("No machine holds a key to this phone.")
                    .identified("hosts.devices.none")
            } else {
                RowGroup(items: model.devices, prominence: .subject) { device in
                    row(device)
                }
            }
        }
    }

    private func row(_ device: PairedDevice) -> some View {
        HStack(spacing: 11) {
            VStack(alignment: .leading, spacing: 2) {
                Text(device.name)
                    .designFont(.identifier, design)
                    .foregroundStyle(design.ink.color)
                Text(Fingerprint.grouped(device.fingerprint))
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 6)
            Button { actions(.revoke(device.host)) } label: {
                ActionLabel("Revoke", kind: .outline)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Revoke \(device.name)")
            .identified("hosts.revoke.\(device.host)", label: "Revoke \(device.name)")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 44)
        .accessibilityElement(children: .contain)
        .identified(
            "hosts.device.\(device.host)", label: "\(device.name), \(device.fingerprint)",
            value: device.fingerprint)
    }
}


