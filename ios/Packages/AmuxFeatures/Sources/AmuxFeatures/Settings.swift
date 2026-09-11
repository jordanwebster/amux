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
                        .designFont(.monoSmall, design)
                        .lineLimit(1)
                    Image(systemName: "chevron.down")
                        .font(.system(size: 7, weight: .bold))
                        .opacity(0.7)
                }
                .foregroundStyle(design.inkFaint.color)
                .padding(.horizontal, 9)
                .padding(.vertical, 6)
                .background { Capsule().fill(design.sunken.color.opacity(0.7)) }
                .thumbTarget(y: 9)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Model and Effort, \(label)")
            .identified("composer.model", label: "Model and Effort", value: label)
            .reclaimingThumbTarget(y: 9)
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
                    .padding(.horizontal, 15)
                    .padding(.bottom, 14)
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
        .frosted(
            RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous),
            wash: 0.88)
        .accessibilityElement(children: .contain)
        .identified("settings", value: ModelChip.label(provider) ?? "")
    }

    private func heading(_ text: String) -> some View {
        Text(text.uppercased())
            .designFont(.sectionTitle, design)
            .foregroundStyle(design.inkFaint.color)
            .padding(.horizontal, 15)
            .padding(.top, 13)
            .padding(.bottom, 8)
    }

    private func modelRow(_ model: ModelInfo) -> some View {
        let chosen = model.id == provider.model
        return Button { change(.model(model.id)) } label: {
            HStack(spacing: 11) {
                Radio(chosen: chosen)
                Text(model.name)
                    .designFont(.body, design)
                    .foregroundStyle(design.ink.color)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 15)
            .padding(.vertical, 11)
            .contentShape(Rectangle())
            .thumbTarget(y: 2)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(model.name)
        .accessibilityAddTraits(chosen ? [.isSelected] : [])
        .identified(
            "settings.model.\(model.id)", label: model.name, value: chosen ? "current" : "")
        .reclaimingThumbTarget(y: 2)
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
                .frame(width: 15, height: 15)
            if chosen {
                Circle().fill(mark).frame(width: 7, height: 7)
            }
        }
        .frame(width: 18, height: 18)
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

    private var index: Int { levels.firstIndex(of: current ?? "") ?? 0 }
    private var last: Int { max(levels.count - 1, 1) }

    private let knob: CGFloat = 15
    private let bar: CGFloat = 5

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            GeometryReader { proxy in
                let travel = proxy.size.width - knob
                let x = travel * CGFloat(index) / CGFloat(last)
                ZStack(alignment: .leading) {
                    Capsule()
                        .fill(design.sunken.color)
                        .frame(height: bar)
                    Capsule()
                        .fill(design.ink.color.opacity(0.55))
                        .frame(width: x + knob / 2, height: bar)
                    HStack(spacing: 0) {
                        ForEach(Array(levels.enumerated()), id: \.element) { stop, _ in
                            Circle()
                                .fill(stop <= index
                                    ? design.ground.color.opacity(0.7)
                                    : design.inkFaint.color.opacity(0.5))
                                .frame(width: 3, height: 3)
                                .frame(maxWidth: .infinity, alignment: alignment(stop))
                        }
                    }
                    .padding(.horizontal, knob / 2)
                    ForEach(Array(levels.enumerated()), id: \.element) { stop, level in
                        let at = travel * CGFloat(stop) / CGFloat(last)
                        Circle()
                            .fill(level == current ? design.ink.color : Color.clear)
                            .frame(width: knob, height: knob)
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
                .frame(height: knob, alignment: .center)
            }
            .frame(height: knob)
            HStack(spacing: 0) {
                ForEach(Array(levels.enumerated()), id: \.element) { stop, level in
                    Text(level.capitalizedFirst)
                        .designFont(.caption, design)
                        .foregroundStyle(
                            level == current ? design.ink.color : design.inkFaint.color)
                        .frame(maxWidth: .infinity, alignment: alignment(stop))
                }
            }
        }
        .accessibilityElement(children: .contain)
    }

    private func alignment(_ index: Int) -> Alignment {
        if index == 0 { return .leading }
        if index == levels.count - 1 { return .trailing }
        return .center
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
        .frosted(
            RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous),
            wash: 0.88)
        .accessibilityElement(children: .contain)
        .identified("permissions", value: permission.current ?? "")
    }

    private func row(_ choice: PermissionChoice) -> some View {
        Button { change(.permission(choice.id)) } label: {
            HStack(spacing: 11) {
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
