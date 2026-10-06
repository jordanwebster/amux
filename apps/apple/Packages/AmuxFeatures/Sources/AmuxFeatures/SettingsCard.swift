import AmuxCore
import AmuxDesign
import SwiftUI

/// What the agent offers to change, as the settings view lists it: the
/// models, the current model's efforts, the permissions and (Codex) the
/// modes, each from the agent's catalogue with the current value marked. A pick is sent at once and the mark moves when
/// the agent reports the new value. Where a setting cannot change from here
/// the card says why instead of offering a pick.
struct SettingsCard: View {
    @Environment(\.design) private var design
    let view: SettingsView
    let kind: Kind?
    let change: (SettingChange) -> Void
    let close: () -> Void
    @State private var height: CGFloat = 0
    private static let tallest: CGFloat = 470

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .center) {
                Text("Agent settings")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                Spacer(minLength: 6)
                Button(action: close) {
                    Image(systemName: "xmark")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(design.inkMuted.color)
                        .thumbTarget(x: 12, y: 12)
                }
                .buttonStyle(.amuxControl)
                .accessibilityLabel("Close")
                .identified("chat.settings.close", label: "Close")
                .reclaimingThumbTarget(x: 12, y: 12)
            }
            // The sections as they are, until they measure taller than the
            // cap; past it they scroll inside it.
            if height > Self.tallest {
                ScrollView { measured }
                    .scrollIndicators(.hidden)
                    .frame(height: Self.tallest)
            } else {
                measured
            }
        }
        .padding(16)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .identified("chat.settings", value: kind.map { "\($0)" })
    }

    private var measured: some View {
        sections.onGeometryChange(for: CGFloat.self, of: \.size.height) { height = $0 }
    }

    private var sections: some View {
        VStack(alignment: .leading, spacing: 16) {
            if !view.models.isEmpty || view.modelRefusal != nil { models }
            if !view.efforts.isEmpty || view.effortRefusal != nil { efforts }
            if let typing = view.changeByTyping { sentence(typing, id: "chat.settings.typing") }
            if !view.permissions.isEmpty || view.cyclePermission || view.permissionRefusal != nil { permissions }
            if !view.modes.isEmpty { modes }
        }
    }

    // MARK: - Model

    private var picksModel: Bool { view.modelRefusal == nil && view.changeByTyping == nil }

    @ViewBuilder
    private var models: some View {
        VStack(alignment: .leading, spacing: 4) {
            heading(String(localized: "MODEL"))
            if picksModel {
                ForEach(Array(view.models.enumerated()), id: \.element.value) { index, choice in
                    if index > 0 { rule }
                    radio(
                        id: "chat.settings.model.\(choice.value)",
                        title: ChatWords.model(choice),
                        detail: choice.reported ? String(localized: "Reported by the agent") : choice.description,
                        current: choice.current, warn: false
                    ) { change(.model(choice.value)) }
                }
            } else {
                if let current = view.models.first(where: { $0.current }) {
                    fact(ChatWords.model(current), id: "chat.settings.model")
                }
                if let refusal = view.modelRefusal { sentence(refusal, id: "chat.settings.model.refusal") }
            }
        }
    }

    // MARK: - Effort

    private var picksEffort: Bool { view.effortRefusal == nil && view.changeByTyping == nil }

    @ViewBuilder
    private var efforts: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                heading(String(localized: "EFFORT"))
                Spacer(minLength: 6)
                if picksEffort, let fallback = view.efforts.first(where: { $0.default }) {
                    Text("Default \(fallback.value)")
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                }
            }
            if picksEffort {
                EffortAxis(efforts: view.efforts) { change(.effort($0)) }
            } else {
                if let current = view.efforts.first(where: { $0.current }) {
                    fact(current.value, id: "chat.settings.effort")
                }
                if let refusal = view.effortRefusal { sentence(refusal, id: "chat.settings.effort.refusal") }
            }
        }
    }

    // MARK: - Permissions

    /// The permissions a pick can set, and the current one whether or not
    /// it can be picked.
    private var pickable: [PermissionChoice] {
        view.permissions.filter { $0.settable || $0.current }
    }

    @ViewBuilder
    private var permissions: some View {
        VStack(alignment: .leading, spacing: 4) {
            heading(ChatWords.permissionsHeading(kind))
            if view.cyclePermission {
                HStack(alignment: .center, spacing: 10) {
                    if let current = view.permissions.first(where: { $0.current }) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(ChatWords.permission(current))
                                .designFont(.bodyEmphasis, design)
                                .foregroundStyle(current.neverAsks ? design.removed.color : design.ink.color)
                            let detail = ChatWords.permissionDetail(current)
                            if !detail.isEmpty {
                                Text(detail)
                                    .designFont(.detail, design)
                                    .foregroundStyle(design.inkMuted.color)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        }
                        .identified("chat.settings.mode", label: ChatWords.permission(current))
                    }
                    Spacer(minLength: 6)
                    Button { change(.cyclePermission) } label: {
                        ActionLabel(String(localized: "Cycle"), kind: .outline)
                    }
                    .buttonStyle(.amuxControl)
                    .identified("chat.settings.cycle", label: "Cycle")
                }
                if let refusal = view.permissionRefusal { sentence(refusal, id: "chat.settings.mode.refusal") }
            } else if let refusal = view.permissionRefusal {
                sentence(refusal, id: "chat.settings.mode.refusal")
            } else {
                ForEach(Array(pickable.enumerated()), id: \.offset) { index, choice in
                    if index > 0 { rule }
                    radio(
                        id: "chat.settings.mode.\(choice.value)",
                        title: ChatWords.permission(choice),
                        detail: ChatWords.permissionDetail(choice),
                        current: choice.current, warn: choice.neverAsks
                    ) { change(.permission(choice.value)) }
                }
            }
        }
    }

    // MARK: - Modes

    /// How the agent works (Codex's default or plan), beside how much it
    /// may do: a second pick.
    private var modes: some View {
        VStack(alignment: .leading, spacing: 4) {
            heading(String(localized: "MODE"))
            ForEach(Array(view.modes.filter { $0.settable || $0.current }.enumerated()), id: \.offset) {
                index, choice in
                if index > 0 { rule }
                radio(
                    id: "chat.settings.workmode.\(choice.value)",
                    title: ChatWords.mode(choice),
                    detail: choice.reported ? String(localized: "Reported by the agent") : "",
                    current: choice.current, warn: false
                ) { change(.mode(choice.value)) }
            }
        }
    }

    // MARK: - Pieces

    /// The hairline between two choices, from the text column to the edge.
    private var rule: some View {
        Rectangle()
            .fill(design.hairline.color)
            .frame(height: design.metrics.hairline)
            .padding(.leading, 30)
    }

    private func heading(_ text: String) -> some View {
        Text(text)
            .designFont(.caption, design)
            .foregroundStyle(design.inkFaint.color)
    }

    private func fact(_ text: String, id: String) -> some View {
        Text(text)
            .designFont(.mono, design)
            .foregroundStyle(design.ink.color)
            .identified(id, label: text)
    }

    private func sentence(_ text: String, id: String) -> some View {
        Text(text)
            .designFont(.detail, design)
            .foregroundStyle(design.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
            .identified(id, label: text)
    }

    /// One choice as a list row: the current one's radio filled; the one
    /// that stops asking in red.
    private func radio(
        id: String, title: String, detail: String, current: Bool, warn: Bool,
        pick: @escaping () -> Void
    ) -> some View {
        Button {
            if !current { pick() }
        } label: {
            HStack(alignment: .center, spacing: 10) {
                Radio(chosen: current)
                    .frame(width: 20)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .designFont(.body, design)
                        .foregroundStyle(warn ? design.removed.color : design.ink.color)
                    if !detail.isEmpty, detail != title {
                        Text(detail)
                            .designFont(.detail, design)
                            .foregroundStyle(design.inkMuted.color)
                            .lineLimit(2)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.vertical, 9)
            .frame(minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxControl)
        .accessibilityAddTraits(current ? [.isSelected] : [])
        .identified(id, label: title, value: current ? "current" : nil)
    }
}

/// The current model's efforts as one segmented track, lowest first, drawn
/// as the You tab draws Appearance: a tap picks one, there is nothing to drag.
private struct EffortAxis: View {
    @Environment(\.design) private var design
    let efforts: [EffortChoice]
    let pick: (String) -> Void

    var body: some View {
        HStack(spacing: 0) {
            ForEach(efforts, id: \.value) { effort in
                Button {
                    if !effort.current { pick(effort.value) }
                } label: {
                    Text(effort.value)
                        .designFont(.caption, design)
                        .foregroundStyle(effort.current ? design.ground.color : design.inkMuted.color)
                        .lineLimit(1)
                        .minimumScaleFactor(0.7)
                        // Room inside each stop, so a long level shrinks before it
                        // reaches the stop's edge when six share the row.
                        .padding(.horizontal, 5)
                        .padding(.vertical, 7)
                        .frame(maxWidth: .infinity)
                        .background {
                            if effort.current { Capsule().fill(design.ink.color) }
                        }
                        .thumbTarget(y: 7)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.amuxControl)
                .accessibilityAddTraits(effort.current ? [.isSelected] : [])
                .identified(
                    "chat.settings.effort.\(effort.value)", label: effort.value,
                    value: effort.current ? "current" : nil)
                .reclaimingThumbTarget(y: 7)
            }
        }
        .padding(2)
        .background(Capsule().fill(design.sunken.color))
    }
}
