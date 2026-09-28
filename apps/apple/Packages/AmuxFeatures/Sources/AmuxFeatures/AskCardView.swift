import AmuxCore
import AmuxDesign
import Foundation
import SwiftUI

/// What the person did on an ask card.
public enum AskAction: Equatable, Sendable {
    /// The choice at this position on the card, with the note it takes.
    case choose(Int, note: String?)
    /// One pick per question, and the note that goes with them.
    case pick([Pick], note: String?)
    /// A form's Submit at this position, with the fields as a JSON object.
    case submit(Int, content: String)
    /// The interrupt: the turn ends, the ask is dismissed, the agent stays.
    case stop
    /// The answer was not confirmed: send it again, or forget it.
    case resend
    case discard
}

/// Where a card starts, for a capture of one part-way through: a note being
/// written, a question's "Something else" open, an option highlighted, or
/// the answers under review.
public enum AskPreset: Equatable, Sendable {
    case noting(Int)
    case other(String)
    case highlighted(UInt32)
    case reviewing([Pick])
    case autoAccept
}

/// Where a question card keeps its progress: what it was left at, and where
/// each change goes.
public struct QuestionKeeping: Sendable {
    let kept: QuestionDraft?
    let keep: @MainActor @Sendable (QuestionDraft) -> Void

    public init(kept: QuestionDraft?, keep: @escaping @MainActor @Sendable (QuestionDraft) -> Void) {
        self.kept = kept
        self.keep = keep
    }

    /// Kept only while the card is on screen.
    public static let none = QuestionKeeping(kept: nil) { _ in }
}

/// The head ask, docked where the composer was. One anatomy for every
/// kind: what it wants and "1 of 3", the subject verbatim, then choices
/// stated as outcomes with the likely one first. Stop is always one tap
/// away in the ⋯ menu.
public struct AskCardView: View {
    @Environment(\.design) private var design
    let card: AskCard
    let preset: AskPreset?
    let questions: QuestionKeeping
    let act: (AskAction) -> Void

    /// `questions` holds a question card's progress somewhere that outlives
    /// the card, so leaving the chat and coming back finds it as it was.
    public init(
        card: AskCard, preset: AskPreset? = nil, questions: QuestionKeeping = .none,
        act: @escaping (AskAction) -> Void
    ) {
        self.card = card
        self.preset = preset
        self.questions = questions
        self.act = act
    }

    public var body: some View {
        Group {
            if case .sending = card.state {
                HStack(spacing: 8) {
                    Image(systemName: "arrow.up.circle")
                        .foregroundStyle(design.inkFaint.color)
                    Text(String(localized: "Sending your answer · \(ChatWords.headline(card))"))
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkMuted.color)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .identified("ask.sending", label: ChatWords.headline(card))
            } else {
                VStack(alignment: .leading, spacing: 12) {
                    head
                    stateLine
                    if showsBody { AskBodyView(card: card, preset: preset, questions: questions, act: act) }
                }
                .padding(16)
            }
        }
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .id(card.key)
        .identified("ask", label: ChatWords.headline(card), value: stateName)
    }

    private var showsBody: Bool {
        switch card.state {
        case .open, .rejected: true
        case .sending, .notConfirmed, .dismissed: false
        }
    }

    private var stateName: String {
        switch card.state {
        case .open: "open"
        case .sending: "sending"
        case .rejected: "rejected"
        case .notConfirmed: "not-confirmed"
        case .dismissed: "dismissed"
        }
    }

    private var head: some View {
        HStack(alignment: .center, spacing: 10) {
            NeedsYouDot()
            Text(ChatWords.headline(card))
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 6)
            if let position = ChatWords.position(card) {
                Text(position)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkFaint.color)
            }
            Menu {
                Button(role: .destructive) { act(.stop) } label: {
                    Label(String(localized: "Stop the turn"), systemImage: "stop.circle")
                }
            } label: {
                Image(systemName: "ellipsis")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(design.inkMuted.color)
                    .thumbTarget(x: 14, y: 20)
            }
            .accessibilityRepresentation {
                Menu {
                    Button(role: .destructive) { act(.stop) } label: {
                        Text("Stop the turn")
                    }
                } label: {
                    Text("More")
                }
            }
            .identified("ask.more", label: "More")
            .reclaimingThumbTarget(x: 14, y: 20)
        }
    }

    @ViewBuilder
    private var stateLine: some View {
        switch card.state {
        case .rejected(let reason):
            Text(String(localized: "Not sent · \(reason)"))
                .designFont(.detail, design)
                .foregroundStyle(design.accent.color)
                .identified("ask.rejected", label: reason)
        case .notConfirmed:
            VStack(alignment: .leading, spacing: 10) {
                Text("Your answer was not confirmed. The connection dropped before the agent replied.")
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 10) {
                    choiceButton(String(localized: "Resend"), kind: .primary, id: "ask.resend") {
                        act(.resend)
                    }
                    choiceButton(String(localized: "Discard"), kind: .outline, id: "ask.discard") {
                        act(.discard)
                    }
                }
            }
        case .dismissed:
            Text("The agent exited with this open. Resume it to carry on.")
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
        case .open, .sending:
            EmptyView()
        }
    }
}

/// A full-width button on a card.
@MainActor
func choiceButton(
    _ title: String, kind: ActionLabel.Kind, id: String, enabled: Bool = true,
    action: @escaping () -> Void
) -> some View {
    Button(action: action) { ActionLabel(title, kind: kind, fill: true) }
        .buttonStyle(.amuxControl)
        .disabled(!enabled)
        .identified(id, label: title, enabled: enabled)
}

/// The subject and the choices, per body variant.
private struct AskBodyView: View {
    @Environment(\.design) private var design
    @Environment(\.openURL) private var openURL
    let card: AskCard
    let preset: AskPreset?
    let questions: QuestionKeeping
    let act: (AskAction) -> Void
    /// The choice waiting for its note.
    @State private var noting: Int?
    @State private var note = ""
    @State private var wholeDiff = false
    @State private var wholePlan = false
    @State private var autoAccept = false
    @State private var forSession = false
    @State private var opened = false
    @State private var fields: [FormField]?

    init(card: AskCard, preset: AskPreset?, questions: QuestionKeeping, act: @escaping (AskAction) -> Void) {
        self.card = card
        self.preset = preset
        self.questions = questions
        self.act = act
        if case .noting(let index)? = preset { _noting = State(initialValue: index) }
        _autoAccept = State(initialValue: preset == .autoAccept)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            subject
            if let noting {
                noteField(noting)
            } else {
                choices
            }
        }
    }

    // MARK: The subject

    @ViewBuilder
    private var subject: some View {
        switch card.body {
        case .command(let command, let cwd, let reason, let description):
            verbatim(command)
            let purpose = [description.isEmpty ? reason : description,
                           cwd.isEmpty ? "" : String(localized: "in \(cwd)")]
                .filter { !$0.isEmpty }.joined(separator: " · ")
            if !purpose.isEmpty { explain(purpose) }
        case .edit(let path, _, let added, let removed, let diff, let reason):
            HStack(alignment: .firstTextBaseline) {
                verbatim(path)
                Spacer(minLength: 6)
                Text("+\(added) −\(removed)")
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
            }
            if !diff.isEmpty { DiffPreview(diff: diff, whole: wholeDiff) }
            if diff.split(separator: "\n").count > DiffPreview.lines {
                Button { wholeDiff.toggle() } label: {
                    Text(wholeDiff ? "Show less" : "Show the whole diff")
                        .designFont(.detail, design)
                        .foregroundStyle(design.accent.color)
                }
                .buttonStyle(.amuxControl)
                .identified("ask.diff", value: wholeDiff ? "open" : "folded")
            }
            if !reason.isEmpty { explain(reason) }
        case .tool(_, _, let arguments):
            if !arguments.isEmpty { verbatim(arguments, lines: 8) }
        case .question:
            EmptyView()
        case .plan(let plan):
            VStack(alignment: .leading, spacing: 6) {
                Prose(markdown: plan)
                    .frame(maxHeight: wholePlan ? 420 : 150, alignment: .top)
                    .clipped()
                    .mask {
                        LinearGradient(
                            stops: [.init(color: .black, location: 0.75),
                                    .init(color: wholePlan ? .black : .clear, location: 1)],
                            startPoint: .top, endPoint: .bottom)
                    }
                Button { wholePlan.toggle() } label: {
                    Text(wholePlan ? "Fold the plan" : "Read the plan")
                        .designFont(.detail, design)
                        .foregroundStyle(design.accent.color)
                }
                .buttonStyle(.amuxControl)
                .identified("ask.plan.read", value: wholePlan ? "open" : "folded")
            }
        case .form(_, let message, _):
            if !message.isEmpty { explain(message) }
        case .link(_, let message, let url):
            if !message.isEmpty { explain(message) }
            verbatim(url)
        case .access(let reason, let read, let write, let network, let hosts):
            if !reason.isEmpty { explain(reason) }
            VStack(alignment: .leading, spacing: 4) {
                ForEach(write, id: \.self) { path in fact(String(localized: "Write to"), path) }
                ForEach(read, id: \.self) { path in fact(String(localized: "Read"), path) }
                if network {
                    fact(
                        String(localized: "Network access"),
                        hosts.isEmpty ? String(localized: "Any host") : hosts.joined(separator: ", "))
                }
            }
        case .unanswerable(let reason):
            explain(reason.isEmpty
                ? String(localized: "The agent is showing a menu this build can’t read. Attach from a terminal to answer it, or stop the turn.")
                : reason)
        }
    }

    private func verbatim(_ text: String, lines: Int? = 4) -> some View {
        Text(text)
            .designFont(.mono, design)
            .foregroundStyle(design.ink.color)
            .lineLimit(lines)
            .fixedSize(horizontal: false, vertical: true)
            .textSelection(.enabled)
            .identified("ask.subject", label: text)
    }

    private func explain(_ text: String) -> some View {
        Text(text)
            .designFont(.detail, design)
            .foregroundStyle(design.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func fact(_ label: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(label)
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
            Text(value)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.ink.color)
                .lineLimit(2)
        }
    }

    // MARK: The choices

    @ViewBuilder
    private var choices: some View {
        switch card.body {
        case .question(let questions):
            QuestionCard(
                questions: questions, takesNote: card.questionNote, preset: preset,
                keeping: self.questions
            ) { picks, note in
                act(.pick(picks, note: note))
            }
        case .plan:
            planChoices
        case .access:
            accessChoices
        case .form(_, _, let schema):
            formChoices(schema)
        case .link(_, _, let url):
            linkChoices(url)
        case .unanswerable:
            choiceButton(String(localized: "Stop the turn"), kind: .primary, id: "ask.stop") {
                act(.stop)
            }
        case .command, .edit, .tool:
            VStack(spacing: 8) {
                ForEach(Array(card.choices.enumerated()), id: \.offset) { index, choice in
                    choiceButton(
                        ChatWords.choice(choice),
                        kind: choice.primary ? .primary : (isDeny(choice) ? .outline : .quiet),
                        id: "ask.choice.\(index)"
                    ) { pick(index) }
                }
            }
        }
    }

    private func isDeny(_ choice: Choice) -> Bool {
        switch choice.outcome {
        case .deny, .denyAndStop, .decline, .sendBack: true
        default: false
        }
    }

    private func pick(_ index: Int) {
        if card.choices[index].takesNote {
            noting = index
            note = ""
        } else {
            act(.choose(index, note: nil))
        }
    }

    /// Approve with a switch for accepting edits, and Send back.
    @ViewBuilder
    private var planChoices: some View {
        let approve = card.choices.indices.filter {
            if case .approvePlan = card.choices[$0].outcome { true } else { false }
        }
        let switched = approve.first {
            card.choices[$0].outcome == .approvePlan(autoAcceptEdits: true)
        }
        let plain = approve.first {
            card.choices[$0].outcome == .approvePlan(autoAcceptEdits: false)
        }
        VStack(alignment: .leading, spacing: 10) {
            if switched != nil, plain != nil {
                Toggle(isOn: $autoAccept) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text("Accept edits without asking")
                            .designFont(.body, design)
                            .foregroundStyle(design.ink.color)
                        Text("Until the plan is done")
                            .designFont(.caption, design)
                            .foregroundStyle(design.inkFaint.color)
                    }
                }
                .tint(design.accent.color)
                .identified("ask.plan.auto", value: autoAccept ? "on" : "off")
            }
            HStack(spacing: 10) {
                if let index = (autoAccept ? switched : plain) ?? approve.first {
                    choiceButton(String(localized: "Approve"), kind: .primary, id: "ask.approve") {
                        act(.choose(index, note: nil))
                    }
                }
                ForEach(card.choices.indices.filter { !approve.contains($0) }, id: \.self) { index in
                    choiceButton(
                        ChatWords.choice(card.choices[index]), kind: .outline,
                        id: "ask.choice.\(index)"
                    ) { pick(index) }
                }
            }
        }
    }

    /// Grant for this turn or this session, or deny.
    @ViewBuilder
    private var accessChoices: some View {
        let turn = card.choices.firstIndex { $0.outcome == .grantForTurn }
        let session = card.choices.firstIndex { $0.outcome == .grantForSession }
        VStack(alignment: .leading, spacing: 10) {
            if turn != nil, session != nil {
                Picker(String(localized: "How long"), selection: $forSession) {
                    Text("This turn").tag(false)
                    Text("This session").tag(true)
                }
                .pickerStyle(.segmented)
                .identified("ask.grant.length", value: forSession ? "session" : "turn")
            }
            HStack(spacing: 10) {
                if let index = (forSession ? session : turn) ?? turn ?? session {
                    choiceButton(String(localized: "Grant"), kind: .primary, id: "ask.grant") {
                        act(.choose(index, note: nil))
                    }
                }
                ForEach(
                    card.choices.indices.filter { $0 != turn && $0 != session }, id: \.self
                ) { index in
                    choiceButton(
                        ChatWords.choice(card.choices[index]), kind: .outline,
                        id: "ask.choice.\(index)"
                    ) { pick(index) }
                }
            }
        }
    }

    /// Native fields from the tool server's schema; required ones gate Submit.
    @ViewBuilder
    private func formChoices(_ schema: String) -> some View {
        let current = fields ?? FormField.parse(schema)
        VStack(alignment: .leading, spacing: 12) {
            ForEach(Array(current.enumerated()), id: \.offset) { index, field in
                FormFieldView(field: field) { value in
                    var edited = current
                    edited[index].value = value
                    fields = edited
                }
            }
            HStack(spacing: 10) {
                ForEach(Array(card.choices.enumerated()), id: \.offset) { index, choice in
                    if choice.outcome == .submit {
                        choiceButton(
                            String(localized: "Submit"), kind: .primary, id: "ask.submit",
                            enabled: current.allSatisfy(\.valid)
                        ) { act(.submit(index, content: FormField.content(current))) }
                    } else {
                        choiceButton(
                            ChatWords.choice(choice), kind: .outline, id: "ask.choice.\(index)"
                        ) { pick(index) }
                    }
                }
            }
        }
    }

    /// Open the link, then say it is done.
    @ViewBuilder
    private func linkChoices(_ url: String) -> some View {
        VStack(spacing: 8) {
            if let link = URL(string: url) {
                choiceButton(
                    String(localized: "Open link"), kind: opened ? .quiet : .primary, id: "ask.open"
                ) {
                    opened = true
                    openURL(link)
                }
            }
            ForEach(Array(card.choices.enumerated()), id: \.offset) { index, choice in
                choiceButton(
                    ChatWords.choice(choice),
                    kind: choice.outcome == .openLink && opened ? .primary : .outline,
                    id: "ask.choice.\(index)"
                ) { pick(index) }
            }
        }
    }

    /// The note a deny or a send-back takes; a send-back needs one.
    private func noteField(_ index: Int) -> some View {
        let choice = card.choices[index]
        let required = choice.outcome == .sendBack
        let label = ChatWords.choice(choice).replacingOccurrences(of: "…", with: "")
        return VStack(alignment: .leading, spacing: 10) {
            TextField(
                required
                    ? String(localized: "What should change")
                    : String(localized: "Tell it why (optional)"),
                text: $note, axis: .vertical
            )
            .lineLimit(2...6)
            .designFont(.body, design)
            .padding(10)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.sunken.color)
            }
            .identified("ask.note", value: note)
            HStack(spacing: 10) {
                choiceButton(
                    label, kind: .primary, id: "ask.note.send",
                    enabled: !required || !note.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                ) {
                    act(.choose(index, note: note.isEmpty ? nil : note))
                }
                choiceButton(String(localized: "Back"), kind: .outline, id: "ask.note.back") {
                    noting = nil
                }
            }
        }
    }
}

/// A short diff inline, green and red, with the whole of it one tap away.
struct DiffPreview: View {
    @Environment(\.design) private var design
    static let lines = 12
    let diff: String
    let whole: Bool

    var body: some View {
        let lines = diff.split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.hasPrefix("+++") && !$0.hasPrefix("---") }
        let shown = whole ? Array(lines.prefix(400)) : Array(lines.prefix(Self.lines))
        ScrollView(.horizontal) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(shown.enumerated()), id: \.offset) { _, line in
                    Text(String(line))
                        .designFont(.monoSmall, design)
                        .foregroundStyle(ink(line))
                        .padding(.horizontal, 8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(wash(line))
                }
            }
            .padding(.vertical, 6)
        }
        .scrollIndicators(.hidden)
        .background {
            RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                .fill(design.sunken.color)
        }
        .clipShape(RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous))
    }

    private func ink(_ line: Substring) -> Color {
        if line.hasPrefix("@@") { return design.inkFaint.color }
        return design.ink.color
    }

    private func wash(_ line: Substring) -> Color {
        if line.hasPrefix("+") { return design.added.color.opacity(0.18) }
        if line.hasPrefix("-") { return design.removed.color.opacity(0.18) }
        return .clear
    }
}

/// One field of a tool server's form, from its JSON schema.
struct FormField: Equatable {
    enum Kind: Equatable {
        case text
        case number(integer: Bool)
        case toggle
        case choice([String])
    }

    let name: String
    let title: String
    let required: Bool
    let kind: Kind
    /// Text, number and choice values; "true" or "false" for a toggle.
    var value: String

    static func parse(_ schema: String) -> [FormField] {
        guard let object = try? JSONSerialization.jsonObject(with: Data(schema.utf8)) as? [String: Any],
              let properties = object["properties"] as? [String: Any]
        else { return [] }
        let required = Set(object["required"] as? [String] ?? [])
        // A decoded object has lost its order: required fields first, then by
        // name, so the one Submit waits on is at the top.
        let names = properties.keys.sorted {
            (required.contains($0) ? 0 : 1, $0) < (required.contains($1) ? 0 : 1, $1)
        }
        return names.compactMap { name in
            guard let property = properties[name] as? [String: Any] else { return nil }
            let kind: Kind
            if let options = property["enum"] as? [Any] {
                kind = .choice(options.map { "\($0)" })
            } else {
                switch property["type"] as? String {
                case "boolean": kind = .toggle
                case "number": kind = .number(integer: false)
                case "integer": kind = .number(integer: true)
                default: kind = .text
                }
            }
            var value = ""
            switch (property["default"], kind) {
            case (let text as String, _): value = text
            case (let flag as Bool, _): value = flag ? "true" : "false"
            case (let number as NSNumber, _): value = number.stringValue
            case (_, .toggle): value = "false"
            case (_, .choice(let options)): value = options.first ?? ""
            default: break
            }
            return FormField(
                name: name, title: property["title"] as? String ?? name,
                required: required.contains(name), kind: kind, value: value)
        }
    }

    var json: Any? {
        switch kind {
        case .toggle: return value == "true"
        case _ where value.isEmpty: return nil
        case .number(integer: true): return Int(value)
        case .number(integer: false): return Double(value)
        case .text, .choice: return value
        }
    }

    var valid: Bool {
        switch kind {
        case .toggle: return true
        case _ where value.isEmpty: return !required
        case .number: return json != nil
        default: return true
        }
    }

    static func content(_ fields: [FormField]) -> String {
        var object: [String: Any] = [:]
        for field in fields { if let json = field.json { object[field.name] = json } }
        let data = (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]))
            ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }
}

private struct FormFieldView: View {
    @Environment(\.design) private var design
    let field: FormField
    let set: (String) -> Void

    var body: some View {
        switch field.kind {
        case .toggle:
            Toggle(isOn: Binding(get: { field.value == "true" }, set: { set($0 ? "true" : "false") })) {
                Text(field.title).designFont(.body, design)
            }
            .tint(design.accent.color)
            .identified("ask.field.\(field.name)", value: field.value)
        case .choice(let options):
            HStack {
                Text(field.title).designFont(.body, design)
                Spacer()
                Picker(field.title, selection: Binding(get: { field.value }, set: { set($0) })) {
                    ForEach(options, id: \.self) { Text($0).tag($0) }
                }
                .pickerStyle(.menu)
                .tint(design.accent.color)
            }
            .identified("ask.field.\(field.name)", value: field.value)
        case .text, .number:
            VStack(alignment: .leading, spacing: 4) {
                Text(field.required ? "\(field.title) *" : field.title)
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkMuted.color)
                TextField(field.title, text: Binding(get: { field.value }, set: { set($0) }))
                    .keyboardType(field.kind == .text ? .default : .decimalPad)
                    .designFont(.body, design)
                    .padding(10)
                    .background {
                        RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                            .fill(design.sunken.color)
                    }
                    .identified("ask.field.\(field.name)", value: field.value)
            }
        }
    }
}

/// Questions: one at a time with the headers as steps, a review of every
/// answer before sending, previews, "Something else…" and secret answers.
/// One pick-one question without previews answers on the tap.
struct QuestionCard: View {
    @Environment(\.design) private var design
    let questions: [QuestionView]
    let takesNote: Bool
    let keeping: QuestionKeeping
    let send: ([Pick], String?) -> Void
    @State private var step = 0
    @State private var picks: [Set<UInt32>]
    @State private var others: [String?]
    @State private var highlighted: [UInt32?]
    @State private var reviewing = false
    @State private var note = ""
    @State private var noting = false

    init(
        questions: [QuestionView], takesNote: Bool, preset: AskPreset? = nil,
        keeping: QuestionKeeping = .none, send: @escaping ([Pick], String?) -> Void
    ) {
        self.questions = questions
        self.takesNote = takesNote
        self.keeping = keeping
        self.send = send
        if let kept = keeping.kept, kept.picks.count == questions.count,
           kept.others.count == questions.count, kept.highlighted.count == questions.count {
            _step = State(initialValue: kept.step)
            _picks = State(initialValue: kept.picks)
            _others = State(initialValue: kept.others)
            _highlighted = State(initialValue: kept.highlighted)
            _reviewing = State(initialValue: kept.reviewing)
            _note = State(initialValue: kept.note)
            _noting = State(initialValue: kept.noting)
            return
        }
        var picks = Array(repeating: Set<UInt32>(), count: questions.count)
        var others = Array(repeating: String?.none, count: questions.count)
        var highlighted = Array(repeating: UInt32?.none, count: questions.count)
        switch preset {
        case .other(let text)?: if !others.isEmpty { others[0] = text }
        case .highlighted(let position)?:
            if !picks.isEmpty {
                picks[0] = [position]
                highlighted[0] = position
            }
        case .reviewing(let answers)?:
            for (index, answer) in answers.enumerated() where index < questions.count {
                switch answer {
                case .options(let chosen): picks[index] = Set(chosen)
                case .other(let text): others[index] = text
                }
            }
            _reviewing = State(initialValue: true)
        default: break
        }
        _picks = State(initialValue: picks)
        _others = State(initialValue: others)
        _highlighted = State(initialValue: highlighted)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if questions.count > 1 { steps }
            if reviewing {
                review
            } else if questions.indices.contains(step) {
                question(questions[step], at: step)
            }
        }
        .onChange(of: progress) { _, progress in keeping.keep(progress) }
    }

    private var progress: QuestionDraft {
        QuestionDraft(
            step: step, picks: picks, others: others, highlighted: highlighted,
            reviewing: reviewing, note: note, noting: noting)
    }

    private var steps: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 6) {
                ForEach(Array(questions.enumerated()), id: \.offset) { index, question in
                    let current = !reviewing && index == step
                    Button {
                        reviewing = false
                        step = index
                    } label: {
                        Text(question.header.isEmpty ? String(localized: "Question \(index + 1)") : question.header)
                            .designFont(.caption, design)
                            .foregroundStyle(current ? design.ground.color : design.inkMuted.color)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 5)
                            .background(Capsule().fill(current ? design.ink.color : design.sunken.color))
                    }
                    .buttonStyle(.amuxControl)
                    .identified("ask.step.\(index)", value: answered(index) ? "answered" : "open")
                }
                Button { reviewing = true } label: {
                    Text("Review")
                        .designFont(.caption, design)
                        .foregroundStyle(reviewing ? design.ground.color : design.inkMuted.color)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 5)
                        .background(Capsule().fill(reviewing ? design.ink.color : design.sunken.color))
                }
                .buttonStyle(.amuxControl)
                .identified("ask.step.review")
            }
        }
        .scrollIndicators(.hidden)
    }

    private func answered(_ index: Int) -> Bool {
        guard picks.indices.contains(index) else { return false }
        return !picks[index].isEmpty || !(others[index] ?? "").isEmpty
    }

    private var hasPreviews: Bool { questions.contains { $0.options.contains { !$0.preview.isEmpty } } }

    /// One tap answers only one pick-one question with no previews.
    private var tapAnswers: Bool {
        questions.count == 1 && !questions[0].multiSelect && !hasPreviews
    }

    @ViewBuilder
    private func question(_ question: QuestionView, at index: Int) -> some View {
        Text(question.question)
            .designFont(.body, design)
            .foregroundStyle(design.ink.color)
            .fixedSize(horizontal: false, vertical: true)
            .identified("ask.question", label: question.question)
        let highlight = highlighted.indices.contains(index) ? highlighted[index] : nil
        if let highlight, question.options.indices.contains(Int(highlight)),
           !question.options[Int(highlight)].preview.isEmpty {
            Text(question.options[Int(highlight)].preview)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.ink.color)
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background {
                    RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                        .fill(design.sunken.color)
                }
                .identified("ask.preview")
        }
        VStack(spacing: 6) {
            ForEach(Array(question.options.enumerated()), id: \.offset) { position, option in
                optionButton(question, option, UInt32(position), at: index)
            }
            if question.allowOther { other(question, at: index) }
        }
        if !tapAnswers || (others.indices.contains(index) && others[index] != nil) {
            let count = picks.indices.contains(index) ? picks[index].count : 0
            choiceButton(
                question.multiSelect && count > 0
                    ? String(localized: "Next · \(count) selected")
                    : (questions.count == 1 ? String(localized: "Send") : String(localized: "Next")),
                kind: .primary, id: "ask.next", enabled: answered(index)
            ) { advance(from: index) }
        }
    }

    private func optionButton(
        _ question: QuestionView, _ option: OptionView, _ position: UInt32, at index: Int
    ) -> some View {
        let selected = picks.indices.contains(index) && picks[index].contains(position)
        return Button {
            select(position, in: question, at: index)
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: question.multiSelect
                    ? (selected ? "checkmark.square.fill" : "square")
                    : (selected ? "largecircle.fill.circle" : "circle"))
                    .foregroundStyle(selected ? design.ink.color : design.inkFaint.color)
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(option.label)
                            .designFont(.bodyEmphasis, design)
                            .foregroundStyle(design.ink.color)
                        if option.recommended {
                            Text("RECOMMENDED")
                                .designFont(.caption, design)
                                .foregroundStyle(design.inkFaint.color)
                        }
                    }
                    if !option.description.isEmpty, option.description != option.label {
                        Text(option.description)
                            .designFont(.detail, design)
                            .foregroundStyle(design.inkMuted.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(10)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(selected ? design.sunken.color : .clear)
                    .strokeBorder(design.hairline.color, lineWidth: 1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.amuxControl)
        .identified("ask.option.\(position)", label: option.label, value: selected ? "selected" : nil)
    }

    private func select(_ position: UInt32, in question: QuestionView, at index: Int) {
        guard picks.indices.contains(index) else { return }
        others[index] = nil
        highlighted[index] = position
        if question.multiSelect {
            if picks[index].contains(position) { picks[index].remove(position) } else {
                picks[index].insert(position)
            }
            return
        }
        picks[index] = [position]
        if tapAnswers {
            send([.options([position])], nil)
        } else if !hasPreviews {
            advance(from: index)
        }
    }

    @ViewBuilder
    private func other(_ question: QuestionView, at index: Int) -> some View {
        let open = others.indices.contains(index) && others[index] != nil
        if open {
            let binding = Binding(
                get: { others.indices.contains(index) ? others[index] ?? "" : "" },
                set: { others[index] = $0; picks[index] = [] })
            Group {
                if question.secret {
                    SecureField(String(localized: "Something else"), text: binding)
                } else {
                    TextField(String(localized: "Something else"), text: binding)
                }
            }
            .designFont(.body, design)
            .padding(10)
            .background {
                RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                    .fill(design.sunken.color)
            }
            .identified("ask.other", value: question.secret ? nil : others[index])
        } else {
            Button {
                others[index] = ""
                picks[index] = []
            } label: {
                HStack {
                    Text("Something else…")
                        .designFont(.body, design)
                        .foregroundStyle(design.inkMuted.color)
                    Spacer()
                }
                .padding(10)
                .contentShape(Rectangle())
            }
            .buttonStyle(.amuxControl)
            .identified("ask.option.other")
        }
    }

    private func advance(from index: Int) {
        guard answered(index) else { return }
        if questions.count == 1 && !takesNote {
            send(answers, nil)
        } else if index + 1 < questions.count {
            step = index + 1
        } else {
            reviewing = true
        }
    }

    private var answers: [Pick] {
        questions.indices.map { index in
            if let other = others[index], !other.isEmpty { return .other(other) }
            return .options(picks[index].sorted())
        }
    }

    /// Every answer on one screen; tap one to change it. An unanswered
    /// question is marked and blocks Send.
    @ViewBuilder
    private var review: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(questions.enumerated()), id: \.offset) { index, question in
                Button {
                    reviewing = false
                    step = index
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(question.header.isEmpty ? question.question : "\(question.header) · \(question.question)")
                            .designFont(.caption, design)
                            .foregroundStyle(design.inkFaint.color)
                            .lineLimit(2)
                        Text(summary(question, at: index))
                            .designFont(.body, design)
                            .foregroundStyle(answered(index) ? design.ink.color : design.accent.color)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.amuxControl)
                .identified("ask.review.\(index)", value: answered(index) ? "answered" : "open")
            }
            if takesNote {
                if noting {
                    TextField(String(localized: "A note for the agent"), text: $note, axis: .vertical)
                        .lineLimit(1...4)
                        .designFont(.body, design)
                        .padding(10)
                        .background {
                            RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                                .fill(design.sunken.color)
                        }
                        .identified("ask.review.note", value: note)
                } else {
                    Button { noting = true } label: {
                        Text("Add a note for the agent")
                            .designFont(.detail, design)
                            .foregroundStyle(design.accent.color)
                    }
                    .buttonStyle(.amuxControl)
                    .identified("ask.review.addNote")
                }
            }
            choiceButton(
                String(localized: "Send answers"), kind: .primary, id: "ask.send",
                enabled: questions.indices.allSatisfy(answered)
            ) {
                send(answers, note.isEmpty ? nil : note)
            }
        }
    }

    private func summary(_ question: QuestionView, at index: Int) -> String {
        guard picks.indices.contains(index) else { return "" }
        if let other = others[index], !other.isEmpty {
            return question.secret ? String(localized: "answered (hidden)") : "“\(other)”"
        }
        let labels = picks[index].sorted().compactMap { position in
            question.options.indices.contains(Int(position)) ? question.options[Int(position)].label : nil
        }
        return labels.isEmpty ? String(localized: "Not answered") : labels.joined(separator: ", ")
    }
}
