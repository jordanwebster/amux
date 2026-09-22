import AmuxCore
import AmuxDesign
import AmuxFeatures
import SwiftUI

/// One deterministic production-component state shared by previews and
/// app-hosted snapshot tests.
///
/// The catalogue lives in the app's Debug sources so its invented content is
/// never linked into a build somebody installs. Its views are the production
/// views themselves; the closure supplies only typed state and inert actions.
@MainActor
struct ComponentExample: Identifiable {
    enum Family: String, CaseIterable {
        case controls = "Controls"
        case ask = "Asks"
        case composer = "Composer"
        case facts = "Conversation facts"
        case status = "Conversation status"
    }

    let id: String
    let family: Family
    let canvas: CGSize
    let dynamicTypeSize: DynamicTypeSize
    let reducesTransparency: Bool
    let readinessIdentifier: String?
    let readinessValue: String?
    fileprivate let build: @MainActor () -> AnyView

    init(
        id: String,
        family: Family,
        canvas: CGSize = CGSize(width: 390, height: 180),
        dynamicTypeSize: DynamicTypeSize = .large,
        reducesTransparency: Bool = false,
        readinessIdentifier: String? = nil,
        readinessValue: String? = nil,
        @ViewBuilder build: @escaping @MainActor () -> some View
    ) {
        self.id = id
        self.family = family
        self.canvas = canvas
        self.dynamicTypeSize = dynamicTypeSize
        self.reducesTransparency = reducesTransparency
        self.readinessIdentifier = readinessIdentifier
        self.readinessValue = readinessValue
        self.build = { AnyView(build()) }
    }
}

/// The single inventory consumed by previews, the debug gallery and snapshot
/// tests. IDs name the component state rather than the screen that happens to
/// contain it, so they remain useful as full-screen coverage becomes smaller.
@MainActor
enum ComponentCatalog {
    static let examples: [ComponentExample] = [
        // MARK: Controls
        ComponentExample(
            id: "controls.primary", family: .controls
        ) {
            ActionLabel("Done", kind: .primary)
        },
        ComponentExample(
            id: "controls.quiet", family: .controls
        ) {
            ActionLabel("Not Now", kind: .quiet)
        },
        ComponentExample(
            id: "controls.outline", family: .controls
        ) {
            ActionLabel("Deny", kind: .outline)
        },
        ComponentExample(
            id: "controls.plain", family: .controls
        ) {
            ActionLabel("More", kind: .plain)
        },
        ComponentExample(
            id: "controls.field-value", family: .controls
        ) {
            FieldRow(label: "Appearance", value: "System")
        },
        ComponentExample(
            id: "controls.field-glyph", family: .controls
        ) {
            FieldRow(label: "Report a Problem", glyph: "exclamationmark.bubble")
        },
        ComponentExample(
            id: "controls.field-group", family: .controls,
            canvas: CGSize(width: 390, height: 250), dynamicTypeSize: .accessibility1
        ) {
            RowGroup(items: ControlField.examples) { field in
                FieldRow(label: field.label, value: field.value, glyph: field.glyph)
            }
        },

        // MARK: Asks
        ComponentExample(
            id: "ask.permission", family: .ask,
            canvas: CGSize(width: 390, height: 300)
        ) {
            AskPanelView(panel: CatalogFixtures.permission(), answer: { _ in })
        },
        ComponentExample(
            id: "ask.permission-scoped", family: .ask,
            canvas: CGSize(width: 390, height: 365)
        ) {
            AskPanelView(panel: CatalogFixtures.permission(scoped: true), answer: { _ in })
        },
        ComponentExample(
            id: "ask.permission-unanswerable", family: .ask,
            canvas: CGSize(width: 390, height: 285)
        ) {
            AskPanelView(panel: CatalogFixtures.unanswerablePermission, answer: { _ in })
        },
        ComponentExample(
            id: "ask.plan", family: .ask,
            canvas: CGSize(width: 390, height: 420),
            readinessIdentifier: "transcript.prose.render", readinessValue: "rendered"
        ) {
            AskPanelView(panel: CatalogFixtures.plan, answer: { _ in })
        },
        ComponentExample(
            id: "ask.question", family: .ask,
            canvas: CGSize(width: 390, height: 420), dynamicTypeSize: .xLarge
        ) {
            AskPanelView(panel: CatalogFixtures.question, answer: { _ in })
        },
        ComponentExample(
            id: "ask.approval", family: .ask,
            canvas: CGSize(width: 390, height: 390)
        ) {
            AskPanelView(panel: CatalogFixtures.approval, answer: { _ in })
        },

        // MARK: Composer
        ComponentExample(
            id: "composer.empty", family: .composer,
            canvas: CGSize(width: 390, height: 230)
        ) {
            CatalogComposer(state: .writing)
        },
        ComponentExample(
            id: "composer.draft", family: .composer,
            canvas: CGSize(width: 390, height: 260)
        ) {
            CatalogComposer(
                state: .writing,
                draft: MessageDraft(prose: "Please tighten the retry path and keep the error visible."))
        },
        ComponentExample(
            id: "composer.sending", family: .composer,
            canvas: CGSize(width: 390, height: 250)
        ) {
            CatalogComposer(state: .sending, draft: MessageDraft(prose: "Run the focused tests."))
        },
        ComponentExample(
            id: "composer.working", family: .composer,
            canvas: CGSize(width: 390, height: 265)
        ) {
            CatalogComposer(
                state: .working(ComposerActivity(name: "Running", elapsed: "18s")))
        },
        ComponentExample(
            id: "composer.dictation-denied", family: .composer,
            canvas: CGSize(width: 390, height: 290)
        ) {
            CatalogComposer(
                state: .writing,
                draft: MessageDraft(prose: "Check the parser before the wire format."),
                dictation: CatalogFixtures.deniedDictation)
        },

        // MARK: Conversation facts
        ComponentExample(
            id: "facts.progress", family: .facts,
            canvas: CGSize(width: 390, height: 190)
        ) {
            CatalogFacts(facts: CatalogFixtures.progressFacts)
        },
        ComponentExample(
            id: "facts.queued", family: .facts,
            canvas: CGSize(width: 390, height: 190)
        ) {
            CatalogFacts(facts: CatalogFixtures.queuedFacts(.held))
        },
        ComponentExample(
            id: "facts.queued-sending", family: .facts,
            canvas: CGSize(width: 390, height: 190)
        ) {
            CatalogFacts(facts: CatalogFixtures.queuedFacts(CatalogFixtures.sending))
        },
        ComponentExample(
            id: "facts.combined-folded", family: .facts,
            canvas: CGSize(width: 390, height: 250)
        ) {
            CatalogFacts(
                facts: CatalogFixtures.combinedFacts, children: CatalogFixtures.children)
        },
        ComponentExample(
            id: "facts.combined-open", family: .facts,
            canvas: CGSize(width: 390, height: 700)
        ) {
            CatalogFacts(
                facts: CatalogFixtures.combinedFacts, children: CatalogFixtures.children,
                open: true)
        },

        // MARK: Conversation status
        ComponentExample(
            id: "status.unreachable", family: .status,
            canvas: CGSize(width: 390, height: 245)
        ) {
            ConversationFoot(
                state: .unreachable(host: "Studio", since: "4m"),
                retry: {}, subscribe: {})
        },
        ComponentExample(
            id: "status.away", family: .status,
            canvas: CGSize(width: 390, height: 250)
        ) {
            ConversationFoot(state: .away(host: "Studio"), retry: {}, subscribe: {})
        },
        ComponentExample(
            id: "status.refused", family: .status,
            canvas: CGSize(width: 390, height: 230)
        ) {
            ConversationFoot(
                state: .refused(
                    headline: "Not sent",
                    reason: "the session is replaying history"),
                retry: {}, subscribe: {})
        },
        ComponentExample(
            id: "status.ended-success", family: .status
        ) {
            EndOfRun(ended: .init(code: 0), age: "now", host: "Studio")
        },
        ComponentExample(
            id: "status.ended-failed", family: .status
        ) {
            EndOfRun(ended: .init(code: 1), age: "2m", host: "Build Mac")
        },
    ]

    static func example(id: String) -> ComponentExample? {
        examples.first { $0.id == id }
    }
}

/// A component on a fixed, production-coloured canvas. Conversation chrome is
/// composited over quiet production typography so its glass is exercised over
/// content rather than over an empty, uniform bitmap.
@MainActor
struct ComponentExampleView: View {
    let example: ComponentExample
    let appearance: ColorScheme

    init(example: ComponentExample, appearance: ColorScheme) {
        self.example = example
        self.appearance = appearance
    }

    var body: some View {
        ZStack {
            Ground()
            if example.family != .controls {
                CatalogBackdrop()
            }
            example.build()
                .padding(18)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
        }
        .frame(width: example.canvas.width, height: example.canvas.height)
        .clipped()
        .environment(\.design, Design.app)
        .environment(\.photographed, true)
        .environment(\.reducesMotion, true)
        .environment(\.reducesTransparency, example.reducesTransparency)
        .environment(\.dynamicTypeSize, example.dynamicTypeSize)
        .preferredColorScheme(appearance)
    }
}

/// Keeps the snapshot target at the app boundary while reusing the production
/// in-process reporting path. Package-owned preference types do not need to
/// leak into the app-hosted test bundle just to wait for a named value.
@MainActor
struct ComponentReadinessReportingView: View {
    private let content: AnyView
    private let didChange: @MainActor ([(identifier: String, value: String?)]) -> Void

    init<Content: View>(
        content: Content,
        didChange: @escaping @MainActor ([(identifier: String, value: String?)]) -> Void
    ) {
        self.content = AnyView(content)
        self.didChange = didChange
    }

    var body: some View {
        content
            .reportingIdentifiedElements(includeGeometry: false)
            .onPreferenceChange(IdentifiedElements.self) { declared in
                let values = declared.map { (identifier: $0.identifier, value: $0.value) }
                Task { @MainActor in didChange(values) }
            }
    }
}

/// Browsable preview backed by the same inventory snapshot tests use.
@MainActor
struct ComponentCatalogGallery: View {
    @State private var appearance = Appearance.light

    var body: some View {
        VStack(spacing: 0) {
            Picker("Appearance", selection: $appearance) {
                Text("Light").tag(Appearance.light)
                Text("Dark").tag(Appearance.dark)
            }
            .pickerStyle(.segmented)
            .padding(20)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 24) {
                    ForEach(ComponentExample.Family.allCases, id: \.rawValue) { family in
                        Section {
                            ForEach(ComponentCatalog.examples.filter { $0.family == family }) { example in
                                VStack(alignment: .leading, spacing: 8) {
                                    Text(example.id)
                                        .font(.headline.monospaced())
                                        .foregroundStyle(.secondary)
                                    ComponentExampleView(
                                        example: example, appearance: appearance.colorScheme)
                                        .clipShape(RoundedRectangle(cornerRadius: 18))
                                        .overlay {
                                            RoundedRectangle(cornerRadius: 18)
                                                .stroke(
                                                    Color.secondary.opacity(0.3), lineWidth: 0.5)
                                        }
                                }
                            }
                        } header: {
                            Text(family.rawValue)
                                .font(.title2.bold())
                        }
                    }
                }
                .padding(20)
            }
            .background {
                switch appearance {
                case .light: Color.white
                case .dark: Color.black
                }
            }
            .preferredColorScheme(appearance.colorScheme)
        }
    }
}

private struct ControlField: Identifiable {
    let id: String
    let label: String
    let value: String?
    let glyph: String?

    static let examples = [
        ControlField(id: "model", label: "Model", value: "System", glyph: "cpu"),
        ControlField(id: "effort", label: "Effort", value: "Auto", glyph: "gauge.high"),
    ]
}

private struct CatalogComposer: View {
    let state: ComposerState
    let dictation: DictationState
    @State private var draft: MessageDraft

    init(
        state: ComposerState,
        draft: MessageDraft = MessageDraft(),
        dictation: DictationState = DictationState()
    ) {
        self.state = state
        self.dictation = dictation
        _draft = State(initialValue: draft)
    }

    var body: some View {
        ComposerBox(
            state: state,
            agent: "Agent",
            provider: CatalogFixtures.provider,
            draft: $draft,
            dictation: dictation,
            actions: { _ in })
    }
}

private struct CatalogFacts: View {
    let facts: ConversationFacts
    var children: [ChildRow] = []
    var open = false

    var body: some View {
        FactsStrip(
            facts: facts, children: children, open: open,
            grow: {}, openChild: { _ in }, unqueue: {})
    }
}

private struct CatalogBackdrop: View {
    @Environment(\.design) private var design

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Clear. Collapsing them now.")
                .designFont(.body, design)
            Divider().overlay(design.hairline.color)
            Text("spec-suite updated three assertions")
                .designFont(.detail, design)
            Divider().overlay(design.hairline.color)
            Text("Finished in 3.8s")
                .designFont(.monoSmall, design)
        }
        .foregroundStyle(design.inkFaint.color.opacity(0.22))
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .accessibilityHidden(true)
    }
}

@MainActor
private enum CatalogFixtures {
    static let provider = Sessions.codexProvider

    static func permission(scoped: Bool = false) -> AskPanel {
        if scoped { return Sessions.claudePermission.panel! }
        return AskPanel(
            id: "catalog-permission",
            address: .claude(ask: 7),
            kind: .permission(.init(
                headline: "Wants to run a command",
                subject: "cargo check -p amux-ui",
                literal: true,
                purpose: nil,
                scope: nil,
                unanswerable: nil)))
    }

    static let unanswerablePermission = AskPanel(
        id: "catalog-permission-unanswerable", address: .claude(ask: 8),
        kind: .permission(.init(
            headline: "Wants to use a tool", subject: "a tool",
            literal: false, purpose: nil, scope: nil,
            unanswerable: "This build cannot answer this menu. Attach to the session to answer it there.")))

    static let plan = Sessions.claudePlan.panel!
    static let question = Sessions.claudeQuestion.panel!
    static let approval = Sessions.codexPermission.panel!

    static var deniedDictation: DictationState {
        var state = DictationState()
        state.prepare(speech: .denied, microphone: .allowed, available: true)
        return state
    }

    static let tasks = Sessions.todos

    static let progressFacts = ConversationFacts(tasks: tasks, children: nil, queued: nil)

    static func queuedFacts(_ delivery: QueueDelivery) -> ConversationFacts {
        ConversationFacts(
            tasks: nil, children: nil,
            queued: QueuedMessage(
                draft: Sessions.heldMessage.draft,
                heldAt: Sessions.heldMessage.heldAt, delivery: delivery))
    }

    static let sending = QueueDelivery.sending(
        op: OpId(UUID(uuidString: "A110CA7A-1000-4000-8000-000000000001")!))

    static let children = [
        ChildRow(
            id: "catalog-child", name: "snapshot-tests", needs: .question,
            state: "waiting", place: .insideThisSession),
        ChildRow(
            id: "catalog-internal", name: "layout-check", needs: nil,
            state: "running", place: .insideThisSession),
    ]

    static let combinedFacts = ConversationFacts(
        tasks: tasks,
        children: .init(count: children.count, needs: .question),
        queued: Sessions.heldMessage)
}

#Preview("Component Catalog") {
    ComponentCatalogGallery()
}
