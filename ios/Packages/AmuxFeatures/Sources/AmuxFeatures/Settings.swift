import AmuxCore
import AmuxDesign
import SwiftUI

/// A choice about how this agent runs.
public enum SettingChange: Equatable, Sendable {
    case model(String)
    case effort(String)
    case permission(String)
}

/// The chip in the composer's footer, and the one thing it opens.
///
/// The model and the effort share it because they are one choice about how
/// hard this thinks. The permission mode does not: it is the safety setting,
/// and putting it next to a list of models would make picking a model and
/// deciding what the agent may do to your working tree the same gesture. It is
/// behind the plus instead.
struct ModelChip: View {
    @Environment(\.design) private var design
    let provider: ProviderFacts
    let press: @MainActor () -> Void

    /// "opus 4.6 · high", or just the model where the layer reports no effort.
    /// Nothing is drawn at all where it reports no model: a chip naming a
    /// default nobody stated would be the app inventing a fact.
    static func label(_ provider: ProviderFacts) -> String? {
        guard let model = provider.model else { return nil }
        let name = provider.models.first { $0.id == model }?.name ?? model
        guard let effort = provider.effort else { return name }
        return "\(name) \u{00B7} \(effort)"
    }

    var body: some View {
        if let label = Self.label(provider) {
            Button(action: press) {
                HStack(spacing: 5) {
                    Text(label)
                        .designFont(.mono, design)
                        .foregroundStyle(design.inkMuted.color)
                        .lineLimit(1)
                    Image(systemName: "chevron.down")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(design.inkFaint.color)
                }
                .padding(.horizontal, 12)
                .frame(minHeight: 34)
                .background { Capsule().fill(design.sunken.color) }
                .contentShape(Capsule())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Model and effort, \(label)")
            .identified("composer.model", label: "Model and effort", value: label)
        }
    }
}

/// Model and effort, on one card over the composer that opened it.
///
/// The furniture is gone on purpose: no sublabels under the model names, and
/// effort is not a segmented control. Three settings in one sheet, two of them
/// lists and the third a row of chips, made effort look like a different kind
/// of thing than it is — it is a position on one axis, so it is drawn as one.
struct SettingsCard: View {
    @Environment(\.design) private var design
    let provider: ProviderFacts
    /// Why a change would be refused, where the layer will refuse it. The card
    /// still opens: what an agent is running under is worth reading even where
    /// it cannot be changed from here.
    let refusal: String?
    let change: @MainActor (SettingChange) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            heading("Model")
            ForEach(Array(provider.models.enumerated()), id: \.element.id) { index, model in
                if index > 0 { Divider().overlay(design.hairline.color) }
                modelRow(model)
            }
            if !provider.efforts.isEmpty {
                heading("Effort")
                EffortAxis(
                    levels: provider.efforts, current: provider.effort,
                    pick: { change(.effort($0)) })
                    .padding(.horizontal, 16)
                    .padding(.bottom, 18)
            }
            if let refusal {
                Text(refusal)
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkFaint.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 16)
                    .identified("settings.refused", label: refusal)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .identified("settings", value: ModelChip.label(provider) ?? "")
    }

    private func heading(_ text: String) -> some View {
        Text(text.uppercased())
            .designFont(.sectionTitle, design)
            .foregroundStyle(design.inkFaint.color)
            .padding(.horizontal, 16)
            .padding(.top, 16)
            .padding(.bottom, 10)
    }

    private func modelRow(_ model: ModelInfo) -> some View {
        let chosen = model.id == provider.model
        return Button { change(.model(model.id)) } label: {
            HStack(spacing: 14) {
                Radio(chosen: chosen)
                Text(model.name)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 16)
            .frame(minHeight: 52)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(model.name)
        .accessibilityAddTraits(chosen ? [.isSelected] : [])
        .identified(
            "settings.model.\(model.id)", label: model.name, value: chosen ? "current" : "")
    }
}

/// The mark beside the thing that is chosen.
///
/// Drawn in the accent, except where the screen around it has already spent
/// its one colour on something else: on the permissions card the only
/// coloured thing is the mode that stops asking, and a second coloured mark
/// there would compete with the warning for the eye.
struct Radio: View {
    @Environment(\.design) private var design
    let chosen: Bool
    var mark: Ramp?

    var body: some View {
        let mark = (mark ?? design.accent).color
        return ZStack {
            Circle()
                .strokeBorder(
                    chosen ? mark : design.hairline.color, lineWidth: chosen ? 2 : 1)
                .frame(width: 20, height: 20)
            if chosen {
                Circle().fill(mark).frame(width: 10, height: 10)
            }
        }
        .frame(width: 22, height: 22)
    }
}

/// Effort as what it is: a position on one axis, low at one end and high at
/// the other, with the ends named.
///
/// Drawn rather than a `Slider`: the platform's control is continuous, and
/// what is being picked is one of a handful of levels a provider reported. A
/// continuous control over three stops promises a precision the setting does
/// not have.
private struct EffortAxis: View {
    @Environment(\.design) private var design
    let levels: [String]
    let current: String?
    let pick: @MainActor (String) -> Void

    var body: some View {
        VStack(spacing: 10) {
            GeometryReader { frame in
                let step = levels.count > 1
                    ? frame.size.width / CGFloat(levels.count - 1) : frame.size.width
                ZStack(alignment: .leading) {
                    Capsule()
                        .fill(design.inkMuted.color)
                        .frame(height: 3)
                    ForEach(Array(levels.enumerated()), id: \.element) { index, level in
                        let at = levels.count > 1 ? step * CGFloat(index) : 0
                        Group {
                            if level == current {
                                Circle().fill(design.ink.color).frame(width: 18, height: 18)
                            } else {
                                Circle().fill(design.inkFaint.color).frame(width: 5, height: 5)
                            }
                        }
                        .frame(width: 44, height: 44)
                        .contentShape(Circle())
                        .offset(x: at - 22)
                        .onTapGesture { pick(level) }
                        .accessibilityLabel(level.capitalizedFirst)
                        .accessibilityAddTraits(level == current ? [.isSelected] : [])
                        .identified(
                            "settings.effort.\(level)", label: level.capitalizedFirst,
                            value: level == current ? "current" : "")
                    }
                }
                .frame(height: 44)
                .frame(maxHeight: .infinity)
            }
            .frame(height: 24)
            HStack {
                ForEach(Array(levels.enumerated()), id: \.element) { index, level in
                    if index > 0 { Spacer(minLength: 0) }
                    Text(level.capitalizedFirst)
                        .designFont(.mono, design)
                        .foregroundStyle(
                            level == current ? design.ink.color : design.inkFaint.color)
                }
            }
        }
        .accessibilityElement(children: .contain)
    }
}

/// What this agent may do without asking, in its provider's own words.
///
/// It opens alone, from the row in the plus, and it names nothing else — no
/// model, because the footer already carries it. Achromatic throughout, with
/// one exception: the mode that stops asking is coloured, because being in it
/// is not a state anybody should be in without seeing it.
struct PermissionsCard: View {
    @Environment(\.design) private var design
    let permission: ProviderPermission
    let refusal: String?
    let change: @MainActor (SettingChange) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let provider = permission.provider {
                Text("\(provider) permissions".uppercased())
                    .designFont(.sectionTitle, design)
                    .foregroundStyle(design.inkFaint.color)
                    .padding(.horizontal, 16)
                    .padding(.top, 16)
                    .padding(.bottom, 10)
            }
            ForEach(Array(permission.choices.enumerated()), id: \.element.id) { index, choice in
                if index > 0 { Divider().overlay(design.hairline.color) }
                row(choice)
            }
            if let refusal {
                Text(refusal)
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkFaint.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 14)
                    .identified("permissions.refused", label: refusal)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .identified("permissions", value: permission.current ?? "")
    }

    private func row(_ choice: PermissionChoice) -> some View {
        Button { change(.permission(choice.id)) } label: {
            HStack(spacing: 14) {
                Radio(chosen: choice.selected, mark: design.ink)
                VStack(alignment: .leading, spacing: 2) {
                    Text(choice.name)
                        .designFont(.body, design)
                        .foregroundStyle(
                            choice.stopsAsking ? design.removed.color : design.ink.color)
                    // The two axes are named under the preset rather than
                    // hidden behind it: what a sandbox permits is the thing
                    // being chosen, and a preset name does not say it. It
                    // wraps rather than truncating for the same reason — a
                    // sandbox shortened to "danger full…" is a preset that
                    // has been hidden behind its name after all.
                    if let detail = choice.detail {
                        Text(detail)
                            .designFont(.monoSmall, design)
                            .foregroundStyle(design.inkFaint.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .frame(minHeight: 52)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(
            [choice.name, choice.detail].compactMap { $0 }.joined(separator: ", "))
        .accessibilityAddTraits(choice.selected ? [.isSelected] : [])
        .identified(
            "permissions.\(choice.id)", label: choice.name,
            value: choice.selected ? "current" : "")
    }
}
