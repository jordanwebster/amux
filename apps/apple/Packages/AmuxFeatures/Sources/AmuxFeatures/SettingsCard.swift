import AmuxCore
import AmuxDesign
import SwiftUI

/// What the agent offers to change, as the settings view lists it: the
/// models, the current model's efforts and the permission modes, each with
/// the current value marked. A pick is sent at once and the mark moves when
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
            if !view.modes.isEmpty || view.cycleMode || view.modeRefusal != nil { modes }
        }
    }

    // MARK: - Model

    private var picksModel: Bool { view.modelRefusal == nil && view.changeByTyping == nil }

    @ViewBuilder
    private var models: some View {
        VStack(alignment: .leading, spacing: 6) {
            heading(String(localized: "MODEL"))
            if picksModel {
                ForEach(view.models, id: \.value) { choice in
                    radio(
                        id: "chat.settings.model.\(choice.value)",
                        title: choice.displayName.isEmpty ? choice.value : choice.displayName,
                        detail: choice.reported ? String(localized: "Reported by the agent") : choice.description,
                        current: choice.current, warn: false
                    ) { change(.model(choice.value)) }
                }
            } else {
                if let current = view.models.first(where: { $0.current }) {
                    fact(current.displayName.isEmpty ? current.value : current.displayName, id: "chat.settings.model")
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

    @ViewBuilder
    private var modes: some View {
        VStack(alignment: .leading, spacing: 6) {
            heading(ChatWords.permissionsHeading(kind))
            if view.cycleMode {
                HStack(alignment: .center, spacing: 10) {
                    if let current = view.modes.first(where: { $0.current }) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(ChatWords.mode(current.value))
                                .designFont(.bodyEmphasis, design)
                                .foregroundStyle(current.stopsAsking ? design.removed.color : design.ink.color)
                            Text(ChatWords.modeDetail(current.value))
                                .designFont(.detail, design)
                                .foregroundStyle(design.inkMuted.color)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        .identified("chat.settings.mode", label: ChatWords.mode(current.value))
                    }
                    Spacer(minLength: 6)
                    Button { change(.cycleMode) } label: {
                        ActionLabel(String(localized: "Cycle"), kind: .outline)
                    }
                    .buttonStyle(.amuxControl)
                    .identified("chat.settings.cycle", label: "Cycle")
                }
                if let refusal = view.modeRefusal { sentence(refusal, id: "chat.settings.mode.refusal") }
            } else if let refusal = view.modeRefusal {
                sentence(refusal, id: "chat.settings.mode.refusal")
            } else {
                ForEach(Array(view.modes.enumerated()), id: \.offset) { _, choice in
                    radio(
                        id: "chat.settings.mode.\(Self.key(choice.value))",
                        title: ChatWords.mode(choice.value), detail: ChatWords.modeDetail(choice.value),
                        current: choice.current, warn: choice.stopsAsking
                    ) { change(.mode(choice.value)) }
                }
            }
        }
    }

    // MARK: - Pieces

    /// A mode by what it sets, for the element identifier.
    private static func key(_ value: ModeValue) -> String {
        switch value {
        case .claude(let mode): mode
        case .codex(let approval, let sandbox, let preset): preset ?? "\(approval).\(sandbox)"
        }
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

    /// One choice: the current one filled; the one that stops asking in red.
    private func radio(
        id: String, title: String, detail: String, current: Bool, warn: Bool,
        pick: @escaping () -> Void
    ) -> some View {
        Button {
            if !current { pick() }
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: current ? "largecircle.fill.circle" : "circle")
                    .foregroundStyle(current ? design.ink.color : design.inkFaint.color)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .designFont(.bodyEmphasis, design)
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
            .padding(10)
            .frame(minHeight: 44)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(current ? design.sunken.color : .clear)
                    .strokeBorder(design.hairline.color, lineWidth: 1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxControl)
        .identified(id, label: title, value: current ? "current" : nil)
    }
}

/// The current model's efforts as stops on one axis, lowest first: a tap
/// picks one, there is nothing to drag.
private struct EffortAxis: View {
    @Environment(\.design) private var design
    let efforts: [EffortChoice]
    let pick: (String) -> Void

    var body: some View {
        HStack(spacing: 4) {
            ForEach(efforts, id: \.value) { effort in
                Button {
                    if !effort.current { pick(effort.value) }
                } label: {
                    Text(effort.value)
                        .designFont(.monoSmall, design)
                        .foregroundStyle(effort.current ? design.onAccent.color : design.ink.color)
                        .lineLimit(1)
                        .minimumScaleFactor(0.7)
                        .frame(maxWidth: .infinity, minHeight: 44)
                        .background {
                            RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                                .fill(effort.current ? design.accent.color : design.sunken.color)
                        }
                        .contentShape(Rectangle())
                }
                .buttonStyle(.amuxControl)
                .identified(
                    "chat.settings.effort.\(effort.value)", label: effort.value,
                    value: effort.current ? "current" : nil)
            }
        }
    }
}
