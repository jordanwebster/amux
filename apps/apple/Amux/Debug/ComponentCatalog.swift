import AmuxCore
import AmuxDesign
import AmuxFeatures
import SwiftUI
import UIKit

/// One deterministic production-component state shared by previews and
/// app-hosted snapshot tests.
///
/// The catalogue lives in the app's Debug sources so its invented content is
/// never linked into a build somebody installs. Its views are the production
/// views themselves, drawn from typed values and a scripted chat.
@MainActor
struct ComponentExample: Identifiable {
    enum Family: String, CaseIterable {
        case rows = "Rows"
        case asks = "Asks"
        case composer = "Composer"
        case chat = "Chat"
        case review = "Review"
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
        canvas: CGSize = CGSize(width: 390, height: 120),
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

/// The single inventory consumed by previews and snapshot tests: every chat
/// row kind, every ask card, the composer's states and the chat's own.
@MainActor
enum ComponentCatalog {
    static let examples: [ComponentExample] = rows + asks + composer + chat + review

    static func example(id: String) -> ComponentExample? {
        examples.first { $0.id == id }
    }

    // MARK: - Rows

    private static func row(
        _ id: String, height: CGFloat = 110, expanded: Bool = false,
        readiness: Bool = false, _ rows: Row...
    ) -> ComponentExample {
        ComponentExample(
            id: "row.\(id)", family: .rows, canvas: CGSize(width: 390, height: height),
            readinessIdentifier: readiness ? "chat.prose.render" : nil,
            readinessValue: readiness ? "rendered" : nil
        ) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                    ChatRowView(
                        row: row, expanded: expanded,
                        rail: RailJoin.of(row, next: index + 1 < rows.count ? rows[index + 1] : nil),
                        bytes: CatalogFixtures.bytes)
                }
            }
        }
    }

    private static let rows: [ComponentExample] = {
        typealias F = CatalogFixtures
        func r(
            _ id: String, _ order: UInt64, _ kind: RowKind, attention: Bool = false,
            decision: Decision? = nil, run: RunInfo? = nil, parent: String? = nil
        ) -> Row {
            ScriptedChat.row(id, order, kind, attention: attention, decision: decision, run: run, parent: parent)
        }
        return [
            row("prompt", r("p1", 1, .prompt(text: [.text("Collapse the pairing errors onto one string.")], steered: false))),
            row("prompt-attachments", height: 190, r("p2", 2, .prompt(
                text: [.text("Here is the screen and the log."), .attachment(.image(F.photo)),
                       .attachment(.file(F.log)), .attachment(.text(name: "Pasted text", lines: 240))],
                steered: false))),
            row("prompt-steered", r("p3", 3, .prompt(text: [.text("Use the new error table instead.")], steered: true))),
            row("prose", height: 330, readiness: true, r("s1", 4, .prose(
                text: [.text(F.markdown)], streaming: false, workingNote: false))),
            row("prose-working-note", height: 120, readiness: true, r("s2", 5, .prose(
                text: [.text("Checking which crates read the table first.")], streaming: true,
                workingNote: true))),
            row("thinking", r("t1", 6, .thinking(text: "", open: false, durationMs: 8_000))),
            row("thinking-open", height: 150, expanded: true, r("t2", 7, .thinking(
                text: "The three arms differ only in wording, so one arm with one string is enough.",
                open: false, durationMs: 4_200))),
            row("tool-call", r("c1", 8, .toolCall(
                server: "github", tool: "create_issue", fact: "#482", state: .succeeded, result: "Created #482"))),
            row("tool-call-asking", r("c2", 9, .toolCall(
                server: "github", tool: "merge_pull_request", fact: "", state: .running, result: ""),
                attention: true)),
            row("file-change", height: 170, r("f1", 10, .fileChange(files: [
                FileRow(path: "crates/amux-ui/src/pairing.rs", change: .edited, added: 9, removed: 14),
                FileRow(path: "crates/amux-ui/spec/pairing_copy.rs", change: .created(lines: 38), added: 38, removed: 0),
                FileRow(path: "crates/amux-ui/src/old.rs", change: .moved(to: "crates/amux-ui/src/codes.rs"), added: 0, removed: 0),
                FileRow(path: "crates/amux-ui/src/unused.rs", change: .deleted, added: 0, removed: 12),
            ], state: .succeeded))),
            row("command", height: 150, r("x1", 11, .command(
                command: "cargo test -p amux-ui", state: .succeeded,
                outputHead: ["running 42 tests", "test pairing::copy ok", "test result: ok. 42 passed"],
                moreLines: 118, durationMs: 4_200, exitCode: 0))),
            row("command-failed", height: 150, r("x2", 12, .command(
                command: "cargo check -p amux-ui", state: .failed,
                outputHead: ["error[E0308]: mismatched types"], moreLines: 214, durationMs: 4_200,
                exitCode: 101))),
            row("command-denied", r("x3", 13, .command(
                command: "rm -rf target", state: .denied, outputHead: [], moreLines: 0, durationMs: nil,
                exitCode: nil),
                decision: Decision(outcome: .denied, elsewhere: false, note: "Use cargo clean instead", scope: nil))),
            row("command-allowed", r("x4", 14, .command(
                command: "cargo test -p amux-ui", state: .succeeded, outputHead: [], moreLines: 0,
                durationMs: 12_000, exitCode: 0),
                decision: Decision(outcome: .allowed, elsewhere: false, note: nil, scope: "this session"))),
            row("command-elsewhere", r("x5", 15, .toolCall(
                server: "github", tool: "create_issue", fact: "", state: .succeeded, result: ""),
                decision: Decision(outcome: .allowed, elsewhere: true, note: nil, scope: nil))),
            row("command-asking", r("x6", 16, .command(
                command: "cargo test -p amux-ui", state: .pending, outputHead: [], moreLines: 0,
                durationMs: nil, exitCode: nil), attention: true)),
            row("explore", r("e1", 17, .explore(verb: .read, subject: "crates/wire/src/codes.rs", state: .succeeded))),
            row("run", r("e2", 18, .explore(verb: .search, subject: "INVALID_PIN", state: .succeeded),
                run: RunInfo(newest: "e2", oldest: "e0", reads: 4, searches: 2, len: 6,
                             anchor: "crates/amux-ui/src/pairing.rs", isSummary: true, openBelow: false))),
            row("run-open-below", r("e3", 19, .explore(verb: .read, subject: "src/lib.rs", state: .succeeded),
                run: RunInfo(newest: "e3", oldest: "e0", reads: 40, searches: 0, len: 40,
                             anchor: "crates/node/src/lib.rs", isSummary: true, openBelow: true))),
            row("subagent-running", r("a1", 20, .subagent(
                description: "Find every INVALID_PIN reader", running: true, toolCount: 12,
                lastTool: "Read wire/src/codes.rs", answer: "", durationMs: 48_000))),
            row("subagent-done", height: 150, r("a2", 21, .subagent(
                description: "Find every INVALID_PIN reader", running: false, toolCount: 12, lastTool: "",
                answer: "Three readers: pairing.rs, codes.rs and the pairing spec. All read the same string.",
                durationMs: 51_000))),
            row("background", r("b1", 22, .background(command: "npm run dev", running: true))),
            row("image", height: 230, r("i1", 23, .image(path: "host-list-mock.png", generated: true, image: F.photo))),
            row("slash-output", height: 150, r("o1", 24, .slashOutput(
                command: "/cost", args: "", output: "Total cost: $0.42\nTotal duration: 18m 3s"))),
            row("turn-end", r("z1", 25, .turnEnd(failed: false, costUsd: 0.42, durationMs: 102_000))),
            row("stopped", r("z2", 26, .stopped)),
            row("compaction", r("z3", 27, .compaction(automatic: true, tokensAfter: 22_000, tokensBefore: 148_000))),
            row("error", height: 140, expanded: false, r("z4", 28, .error(
                errorKind: "Provider overloaded", message: "The API returned 529 ten times in a row.",
                attempts: 10, gaveUp: true))),
            row("model-switch", r("z5", 29, .modelSwitch(from: "opus", to: "sonnet 4.6", reason: "Opus declined this request"))),
            row("boundary", r("z6", 30, .boundary(kind: .resumed, cause: "after an update"))),
            row("agent-message", height: 130, r("m1", 31, .agentMessage(
                from: "worker-2", kind: .finished, text: "3 specs updated, all green.", to: "",
                sent: .unspecified, rejection: ""))),
            row("agent-message-rejected", height: 130, r("m2", 32, .agentMessage(
                from: "", kind: .message, text: "Please rebase onto main first.", to: "worker-3",
                sent: .rejected, rejection: "no agent by that name"))),
            row("auto-review", r("v1", 33, .autoReview(
                decision: "Auto-approved curl localhost:8080", risk: "low", rationale: "Local only.",
                subject: "x9"))),
            row("unrecognized", r("u1", 34, .unrecognized(what: "rate_limit_event", summary: "an event this build cannot read"))),
            row("subagent-steps", height: 200, expanded: true,
                r("a3", 35, .subagent(
                    description: "Explore the pairing code", running: true, toolCount: 2,
                    lastTool: "Grep INVALID_PIN", answer: "", durationMs: 9_000)),
                r("a4", 36, .explore(verb: .search, subject: "INVALID_PIN", state: .succeeded), parent: "a3"),
                r("a5", 37, .explore(verb: .read, subject: "wire/src/codes.rs", state: .succeeded), parent: "a3")),
            row("ask-question", height: 130, r("q1", 38, .ask(.question(
                questions: [F.redactionQuestion], answers: [AnswerView(picked: ["amux-core"], hidden: false, other: nil)],
                resolution: .answered, note: nil)))),
            row("ask-questions", height: 280, r("q2", 39, .ask(.questions(
                questions: F.threeQuestions,
                answers: [AnswerView(picked: ["macOS", "Linux"], hidden: false, other: nil),
                          AnswerView(picked: ["All at once"], hidden: false, other: nil),
                          AnswerView(picked: [], hidden: false, other: "Only failures, with the host id")],
                resolution: .answered, note: "keep the old strings for one release")))),
            row("ask-secret", height: 130, r("q3", 40, .ask(.questions(
                questions: [F.secretQuestion], answers: [AnswerView(picked: [], hidden: true, other: nil)],
                resolution: .answered, note: nil)))),
            row("ask-plan-approved", r("l1", 41, .ask(.plan(plan: F.plan, verdict: .approved, note: nil)))),
            row("ask-plan-sent-back", r("l2", 42, .ask(.plan(plan: F.plan, verdict: .sentBack, note: "Don’t touch the wire codes yet")))),
            row("ask-plan-open", height: 330, expanded: true, readiness: true,
                r("l3", 43, .ask(.plan(plan: F.plan, verdict: .open, note: nil)))),
            row("ask-form", r("g1", 44, .ask(.form(server: "github", message: "Create the issue", fields: ["repository", "labels", "assignee"], resolution: .answered)))),
            row("ask-link", r("g2", 45, .ask(.link(server: "linear", message: "Sign in to Linear", url: "https://linear.app/login", resolution: .answered)))),
            row("ask-grant", r("g3", 46, .ask(.grant(
                reason: "To run the integration tests against a local server.", read: [],
                write: ["~/src/amux/target"], network: false, hosts: [], resolution: .answered,
                granted: Granted(read: [], write: ["~/src/amux/target"], network: false, forSession: false))))),
            row("ask-unanswerable", r("g4", 47, .ask(.unanswerable(reason: "A tool server’s dialog", resolution: .cancelled)))),
        ]
    }()

    // MARK: - Asks

    private static func ask(
        _ id: String, height: CGFloat = 420, preset: AskPreset? = nil, _ card: AskCard
    ) -> ComponentExample {
        ComponentExample(
            id: "ask.\(id)", family: .asks, canvas: CGSize(width: 390, height: height),
            readinessIdentifier: nil, readinessValue: nil
        ) {
            AskCardView(card: card, preset: preset) { _ in }
        }
    }

    private static let asks: [ComponentExample] = {
        typealias F = CatalogFixtures
        return [
            ask("permission-command", height: 480, F.card(.claudeSdk, .command(
                command: "cargo test -p amux-ui", cwd: "~/src/amux", reason: "",
                description: "Run the spec suite"), F.sdkPermissionChoices)),
            ask("permission-deny-note", height: 380, preset: .noting(3), F.card(.claudeSdk, .command(
                command: "rm -rf target", cwd: "~/src/amux", reason: "", description: "Clean the build"),
                F.sdkPermissionChoices)),
            ask("permission-edit", height: 560, F.card(.claudeSdk, .edit(
                path: "crates/amux-ui/src/pairing.rs", files: 1, added: 1, removed: 2, diff: F.diff,
                reason: ""), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .allowAlways(subjects: [], directories: [], mode: "acceptEdits", scope: .session, label: ""), primary: false, takesNote: false),
                    Choice(outcome: .deny(stops: false), primary: false, takesNote: true),
                ])),
            ask("permission-tool", height: 360, F.card(.claudeSdk, .tool(
                server: "github", tool: "create_issue",
                arguments: "{\n  \"repo\": \"jlw/amux\",\n  \"title\": \"Pairing copy\"\n}"), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .deny(stops: false), primary: false, takesNote: true),
                ])),
            ask("permission-terminal", height: 380, F.card(.claudePty, .command(
                command: "cargo test -p amux-ui", cwd: "", reason: "", description: ""), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .allowAlways(subjects: ["cargo test"], directories: [], mode: "", scope: .project, label: ""), primary: false, takesNote: false),
                    Choice(outcome: .deny(stops: true), primary: false, takesNote: false),
                ])),
            ask("codex-command", height: 560, F.card(.codex, .command(
                command: "curl -s localhost:8080/health", cwd: "~/src/amux", reason: "Check the dev server",
                description: ""), [
                    Choice(outcome: .allowOnce, primary: true, takesNote: false),
                    Choice(outcome: .allowForSession, primary: false, takesNote: false),
                    Choice(outcome: .allowSimilar(prefix: ["curl", "-s"]), primary: false, takesNote: false),
                    Choice(outcome: .allowNetwork(hosts: ["localhost"]), primary: false, takesNote: false),
                    Choice(outcome: .deny(stops: false), primary: false, takesNote: false),
                    Choice(outcome: .denyAndStop, primary: false, takesNote: false),
                ], count: 2)),
            ask("question-single", height: 440, F.card(.claudeSdk, .question([F.redactionQuestion]), [])),
            ask("question-multi", height: 460, preset: .highlighted(0), F.card(.claudeSdk, .question([F.platformQuestion]), [])),
            ask("questions-step", height: 440, F.card(.codex, .question(F.threeQuestions), [])),
            ask("questions-review", height: 520, preset: .reviewing([.options([0, 1]), .options([1]), .other("Only failures, with the host id")]),
                F.card(.codex, .question(F.threeQuestions), [])),
            ask("question-previews", height: 520, preset: .highlighted(0), F.card(.claudeSdk, .question([F.previewQuestion]), [])),
            ask("question-other", height: 460, preset: .other("Put it in amux-wire next to the codes"),
                F.card(.claudeSdk, .question([F.redactionQuestion]), [])),
            ask("question-secret", height: 360, preset: .other("hunter2"), F.card(.codex, .question([F.secretQuestion]), [])),
            ask("plan", height: 480, preset: .autoAccept, F.card(.claudeSdk, .plan(plan: F.plan), [
                Choice(outcome: .approvePlan(autoAcceptEdits: true), primary: true, takesNote: false),
                Choice(outcome: .approvePlan(autoAcceptEdits: false), primary: false, takesNote: false),
                Choice(outcome: .sendBack, primary: false, takesNote: true),
            ])),
            ask("form", height: 520, F.card(.claudeSdk, .form(
                server: "github", message: "Create the issue in which repository?", schemaJson: F.formSchema), [
                    Choice(outcome: .submit, primary: true, takesNote: false),
                    Choice(outcome: .decline, primary: false, takesNote: false),
                ])),
            ask("link", height: 380, F.card(.codex, .link(
                server: "linear", message: "Sign in to Linear to continue.", url: "https://linear.app/login"), [
                    Choice(outcome: .openLink, primary: true, takesNote: false),
                    Choice(outcome: .decline, primary: false, takesNote: false),
                ])),
            ask("access", height: 420, F.card(.codex, .access(
                reason: "To run the integration tests against a local server.", read: [],
                write: ["~/src/amux/target"], network: true, hosts: []), [
                    Choice(outcome: .grantForTurn, primary: true, takesNote: false),
                    Choice(outcome: .grantForSession, primary: false, takesNote: false),
                    Choice(outcome: .decline, primary: false, takesNote: false),
                ])),
            ask("sending", height: 120, F.card(.claudeSdk, .command(
                command: "cargo test", cwd: "", reason: "", description: ""), [], state: .sending)),
            ask("rejected", height: 460, F.card(.claudeSdk, .command(
                command: "cargo test -p amux-ui", cwd: "~/src/amux", reason: "", description: ""),
                F.sdkPermissionChoices, state: .rejected("the ask was already answered in the terminal"))),
            ask("not-confirmed", height: 260, F.card(.claudeSdk, .command(
                command: "cargo test -p amux-ui", cwd: "", reason: "", description: ""),
                F.sdkPermissionChoices, state: .notConfirmed)),
            ask("dismissed", height: 180, F.card(.claudeSdk, .command(
                command: "cargo test -p amux-ui", cwd: "", reason: "", description: ""),
                F.sdkPermissionChoices, state: .dismissed)),
            ask("unanswerable", height: 300, F.card(.claudePty, .unanswerable(reason: ""), [])),
        ]
    }()

    // MARK: - Composer

    private static func composer(
        _ id: String, height: CGFloat = 260, rows: [Row] = [], frame: ChatFrame,
        strip: Strip = ScriptedChat.strip(model: "opus 4.6", effort: "high"), draft: String = "",
        settings: SettingsView? = nil, showing: ChatOverlay? = nil,
        subject: ChatSubject = CatalogFixtures.subject, setUp: @escaping @MainActor (ChatModel) -> Void = { _ in }
    ) -> ComponentExample {
        ComponentExample(
            id: "composer.\(id)", family: .composer, canvas: CGSize(width: 390, height: height)
        ) {
            CatalogChat(source: ScriptedChat(
                rows: rows, frame: frame, strip: strip, settings: settings, images: CatalogFixtures.images
            )) { model in
                ChatStanding(model: model, subject: subject, showing: .constant(showing))
                    .onAppear {
                        model.draft = draft
                        setUp(model)
                    }
            }
            .frame(maxHeight: .infinity, alignment: .bottom)
        }
    }

    private static let composer: [ComponentExample] = {
        typealias F = CatalogFixtures
        return [
            composer("empty", height: 160, frame: ScriptedChat.frame(phase: .idle)),
            composer("draft", height: 220, frame: ScriptedChat.frame(phase: .idle),
                     draft: "Please tighten the retry path and keep the error visible.") {
                $0.attach(F.photoData, name: "screen.jpg", mime: "image/jpeg", image: true)
            },
            composer("working", height: 190, rows: [F.runningCommand], frame: ScriptedChat.frame(
                phase: .working,
                activity: Activity(kind: .running(key: F.runningCommand.id), sinceMs: 0, elapsedMs: 12_000))),
            composer("thinking", height: 190, frame: ScriptedChat.frame(
                phase: .working, activity: Activity(kind: .thinking, sinceMs: 0, elapsedMs: 24_000)),
                draft: "Also check the Windows path."),
            composer("retrying", height: 190, frame: ScriptedChat.frame(
                phase: .working,
                activity: Activity(kind: .retrying(attempt: 2, maxAttempts: 10, retryAtMs: nil), sinceMs: 0, elapsedMs: 3_000))),
            composer("queued", height: 400, frame: ScriptedChat.frame(
                phase: .working, activity: Activity(kind: .working, sinceMs: 0, elapsedMs: 41_000),
                queue: [
                    QueuedRow(inputId: [1], text: [.text("Then run the Windows check.")], mine: true, steered: false, canWithdraw: true, canSendNow: true, fromAgent: nil),
                    QueuedRow(inputId: [2], text: [.text("Use the new error table instead.")], mine: true, steered: true, canWithdraw: false, canSendNow: false, fromAgent: nil),
                    QueuedRow(inputId: [3], text: [.text("worker-2 finished the specs.")], mine: false, steered: false, canWithdraw: false, canSendNow: true, fromAgent: "worker-2"),
                ])),
            composer("not-confirmed", height: 280, frame: ScriptedChat.frame(outbox: [
                OutboxRow(inputId: [4], text: [.text("Run the focused tests.")], state: .notConfirmed),
            ])),
            composer("rejected", height: 280, frame: ScriptedChat.frame(outbox: [
                OutboxRow(inputId: [5], text: [.text("Run the focused tests.")], state: .rejected("the agent is not taking prompts")),
            ])),
            composer("sending", height: 240, frame: ScriptedChat.frame(outbox: [
                OutboxRow(inputId: [6], text: [.text("Run the focused tests.")], state: .sending),
            ])),
            composer("resume", height: 200, frame: ScriptedChat.frame(phase: .exited(cause: "code 1"), mode: .resume),
                     draft: "Pick up where you left off and rerun the tests."),
            composer("detached", height: 180, frame: ScriptedChat.frame(mode: .disabled(.detached), caughtUp: false, waiting: .detached),
                     draft: "Also add a test for the new string."),
            composer("catching-up", height: 160, frame: ScriptedChat.frame(mode: .disabled(.catchingUp), caughtUp: false, waiting: .catchingUp)),
            composer("strip", height: 220, frame: ScriptedChat.frame(phase: .working, activity: Activity(kind: .working, sinceMs: 0, elapsedMs: 94_000)),
                     strip: ScriptedChat.strip(
                        tasks: TasksView(done: 3, total: 7, current: "Updating the pairing copy"),
                        context: ContextView(usedTokens: 168_000, inStrip: true, percent: 84, windowTokens: 200_000),
                        model: "opus 4.6", effort: "high", mode: "plan", background: 2)),
            // The strip once per provider kind, carrying the facts that
            // kind's interpreter reports: the renderer never sees the kind,
            // so these differ only in which facts are present.
            composer("strip-claude-pty", height: 220, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(
                tasks: TasksView(done: 1, total: 3, current: "Resuming from the live checklist"),
                context: ContextView(usedTokens: 171_000, inStrip: true, percent: 86, windowTokens: 200_000),
                model: "claude-sonnet-5", mode: "acceptEdits", background: 2)),
            composer("strip-claude-sdk", height: 220, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(
                context: ContextView(usedTokens: 30_513, inStrip: false, percent: 16, windowTokens: 200_000),
                model: "claude-opus-5-5", effort: "low", mode: "plan",
                usage: UsageView(blocked: false, windows: [UsageWindowView(name: "5h", usedPercent: 83, resetsAtMs: nil)], credits: nil),
                failedServers: [ServerView(name: "claude.ai Google Drive", error: "", needsAuth: true)],
                background: 3)),
            composer("strip-codex", height: 220, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(
                tasks: TasksView(done: 1, total: 3, current: "Split the lexer"),
                context: ContextView(usedTokens: 16_447, inStrip: false, percent: 6, windowTokens: 258_400),
                model: "gpt-5.6-luna", effort: "high", mode: "on-request",
                failedServers: [ServerView(name: "docs", error: "connection refused", needsAuth: false)])),
            composer("dictation-denied", height: 240, frame: ScriptedChat.frame(phase: .idle),
                     draft: "Also check the Windows path.") {
                $0.dictation.prepare(speech: .denied, microphone: .notAsked, available: true)
            },
            composer("dictation-listening", height: 220, frame: ScriptedChat.frame(phase: .idle)) { model in
                model.dictation.prepare(speech: .allowed, microphone: .allowed, available: true)
                model.dictation.began(draft: "Please")
                model.draft = "Please"
                model.heard("tighten the retry path")
            },
            composer("review-token", height: 200, frame: ScriptedChat.frame(phase: .idle),
                     draft: "Please address these before the next run.") {
                $0.attach(F.writtenReview())
            },
            composer("strip-trouble", height: 220, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(
                model: "gpt-5", usage: UsageView(blocked: false, windows: [UsageWindowView(name: "5h", usedPercent: 91, resetsAtMs: nil)], credits: nil),
                failedServers: [ServerView(name: "github", error: "exited", needsAuth: false)])),
            composer("sign-in", height: 200, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(
                signIn: SignInView(state: .expired, account: "ada@example.com", message: "Run claude login on Studio."))),
            // The settings card once per provider kind, from the settings
            // view each kind's interpreter state gives.
            composer("settings-claude-sdk", height: 640, frame: ScriptedChat.frame(kind: .claudeSdk),
                     strip: ScriptedChat.strip(model: "claude-opus-5-5", effort: "low", mode: "plan"),
                     settings: F.claudeSdkSettings(), showing: .settings),
            composer("settings-codex", height: 640, frame: ScriptedChat.frame(kind: .codex),
                     strip: ScriptedChat.strip(model: "gpt-6-astra", effort: "medium", mode: "on-request"),
                     settings: F.settingsCodex, showing: .settings),
            composer("settings-claude-pty", height: 420, frame: ScriptedChat.frame(kind: .claudePty),
                     strip: ScriptedChat.strip(model: "claude-sonnet-5", effort: "high", mode: "acceptEdits"),
                     settings: F.settingsClaudePty, showing: .settings),
            composer("chip-stops-asking", height: 160, frame: ScriptedChat.frame(kind: .claudeSdk),
                     strip: ScriptedChat.strip(model: "claude-opus-5-5", effort: "low", mode: "bypassPermissions"),
                     settings: F.claudeSdkSettings(mode: "bypassPermissions")),
            composer("slash", height: 460, frame: ScriptedChat.frame(kind: .claudeSdk), draft: "/c",
                     settings: F.withCommands),
            composer("pasted-text", height: 220, frame: ScriptedChat.frame(phase: .idle)) { model in
                model.type("Why does this fail? ")
                model.type("Why does this fail? " + (1...14).map { "error[E0308]: mismatched types at line \($0)" }.joined(separator: "\n"))
            },
            composer("usage-blocked", height: 200, frame: ScriptedChat.frame(), strip: ScriptedChat.strip(
                usage: UsageView(blocked: true, windows: [UsageWindowView(name: "weekly", usedPercent: 100, resetsAtMs: nil)], credits: "Resets Monday"))),
        ]
    }()

    // MARK: - Chat

    private static func chat(
        _ id: String, height: CGFloat = 844, readiness: (String, String)? = nil,
        chat: @escaping @MainActor () -> ScriptedChat, subject: ChatSubject = CatalogFixtures.subject,
        family: FamilyHeader? = nil, showing: ChatOverlay? = nil,
        setUp: @escaping @MainActor (ChatModel, ScriptedChat) -> Void = { _, _ in }
    ) -> ComponentExample {
        ComponentExample(
            id: "chat.\(id)", family: .chat, canvas: CGSize(width: 390, height: height),
            readinessIdentifier: readiness?.0, readinessValue: readiness?.1
        ) {
            let source = chat()
            CatalogChat(source: source) { model in
                ChatScreen(model: model, subject: subject, family: family, showing: showing) { _ in }
                    .onAppear { setUp(model, source) }
            }
        }
    }

    private static let chat: [ComponentExample] = {
        typealias F = CatalogFixtures
        return [
            chat("conversation", chat: { ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(phase: .idle), strip: ScriptedChat.strip(model: "opus 4.6")) }),
            chat("asking", chat: {
                ScriptedChat(
                    rows: F.conversation + [F.askedCommand], frame: ScriptedChat.frame(phase: .needsYou),
                    card: F.card(.claudeSdk, .command(command: "cargo test -p amux-ui", cwd: "~/src/amux", reason: "", description: "Run the spec suite"), F.sdkPermissionChoices))
            }),
            chat("detached", chat: {
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(mode: .disabled(.detached), caughtUp: false, waiting: .detached))
            }),
            chat("loading", readiness: ("chat.loading", "shown"), chat: {
                ScriptedChat(rows: [], frame: ScriptedChat.frame(mode: .disabled(.catchingUp), caughtUp: false, waiting: .catchingUp))
            }),
            chat("away", chat: {
                ScriptedChat(rows: [], frame: ScriptedChat.frame(mode: .disabled(.detached), caughtUp: false, waiting: .detached))
            }, subject: F.awaySubject),
            chat("new-activity", readiness: ("chat.newActivity", "shown"), chat: {
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(phase: .working))
            }) { model, source in
                model.reading(atNewest: false)
                source.append([ScriptedChat.row("n1", 100, .prose(text: [.text("Done. The three arms are one now.")], streaming: false, workingNote: false))])
                model.woke()
            },
            chat("paging-unreachable", readiness: ("chat.paging", "unreachable"), chat: {
                let source = ScriptedChat(rows: Array(F.conversation.prefix(3)), frame: ScriptedChat.frame(hasOlder: true))
                source.paged = .originUnreachable
                return source
            }),
            chat("family", chat: {
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(phase: .working))
            }, family: F.family),
            chat("changes", readiness: ("chat.changes", "shown"), chat: {
                let source = ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame(phase: .idle))
                source.working = F.review
                return source
            }),
            chat("rename", chat: { ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame()) }, showing: .rename),
            chat("delete", chat: { ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame()) }, showing: .delete),
            chat("delete-family", chat: {
                ScriptedChat(rows: F.conversation, frame: ScriptedChat.frame())
            }, family: F.family, showing: .delete),
            chat("settings", chat: {
                ScriptedChat(
                    rows: F.conversation, frame: ScriptedChat.frame(kind: .claudePty),
                    strip: ScriptedChat.strip(model: "claude-sonnet-5", effort: "high", mode: "acceptEdits"),
                    settings: F.settingsClaudePty)
            }, showing: .settings),
        ]
    }()
}

extension ComponentCatalog {
    // MARK: - Review

    private static func review(
        _ id: String, height: CGFloat = 844, writing: Bool = false,
        setUp: @escaping @MainActor (ReviewModel) -> Void = { _ in }
    ) -> ComponentExample {
        ComponentExample(id: "review.\(id)", family: .review, canvas: CGSize(width: 390, height: height)) {
            CatalogReview(setUp: setUp) { model in
                ReviewPage(model: model, agent: "refactor-auth", writing: writing) { _ in }
            }
        }
    }

    fileprivate static let review: [ComponentExample] = {
        typealias F = CatalogFixtures
        return [
            review("page") { F.comment($0) },
            review("selection") { model in
                model.begin(at: ReviewLine(file: 0, hunk: 0, line: 2))
                model.extend(to: ReviewLine(file: 0, hunk: 0, line: 4))
            },
            review("comment", writing: true) { model in
                model.begin(at: ReviewLine(file: 0, hunk: 0, line: 2))
                model.extend(to: ReviewLine(file: 0, hunk: 0, line: 4))
            },
            review("folded") { model in
                F.comment(model)
                model.toggle(file: 0)
                model.toggle(file: 1)
            },
            ComponentExample(id: "review.files", family: .review, canvas: CGSize(width: 390, height: 420)) {
                CatalogReview(setUp: F.comment) { model in ReviewFileList(model: model) { _ in } }
            },
        ]
    }()
}

/// Holds one review's model for as long as the example is drawn.
private struct CatalogReview<Content: View>: View {
    @State private var model = ReviewModel(review: CatalogFixtures.review)
    let setUp: @MainActor (ReviewModel) -> Void
    let content: (ReviewModel) -> Content

    init(setUp: @escaping @MainActor (ReviewModel) -> Void, @ViewBuilder content: @escaping (ReviewModel) -> Content) {
        self.setUp = setUp
        self.content = content
        setUp(_model.wrappedValue)
    }

    var body: some View { content(model) }
}

/// Holds one scripted chat's model for as long as the example is drawn.
private struct CatalogChat<Content: View>: View {
    @State private var model: ChatModel
    let content: (ChatModel) -> Content

    init(source: ScriptedChat, @ViewBuilder content: @escaping (ChatModel) -> Content) {
        _model = State(initialValue: ChatModel(source: source, loadingHintAfter: .zero))
        self.content = content
    }

    var body: some View { content(model) }
}

/// The catalogue's invented chat.
@MainActor
enum CatalogFixtures {
    // MARK: - Settings

    private static let claudeModes = ["default", "acceptEdits", "plan", "auto", "bypassPermissions"]

    static func claudeSdkSettings(mode: String = "plan") -> SettingsView {
        let efforts = ["low", "medium", "high", "xhigh", "max"]
        return SettingsView(
            models: [
                ModelChoice(value: "default", displayName: "Default (recommended)", description: "Opus 5.5 · Most capable for complex work", efforts: efforts, current: true, reported: false, defaultEffort: "high"),
                ModelChoice(value: "sonnet", displayName: "Sonnet", description: "Sonnet 5 · Best for everyday tasks", efforts: efforts, current: false, reported: false, defaultEffort: "high"),
                ModelChoice(value: "haiku", displayName: "Haiku", description: "Haiku 4.5 · Fastest for quick answers", efforts: [], current: false, reported: false, defaultEffort: nil),
            ],
            efforts: [EffortChoice(value: "low", current: true, default: false, reported: false)],
            modes: claudeModes.map {
                ModeChoice(value: .claude($0), current: $0 == mode, reported: false, stopsAsking: $0 == "bypassPermissions")
            },
            cycleMode: false, commands: [], changeByTyping: nil,
            effortRefusal: "Claude takes its effort when the agent starts and keeps it until it restarts.",
            modeRefusal: nil, modelRefusal: nil)
    }

    static let withCommands: SettingsView = {
        var view = claudeSdkSettings()
        view.commands = [
            CommandView(name: "clear", description: "Clear conversation history and free up context", argumentHint: "", source: ""),
            CommandView(name: "compact", description: "Clear history but keep a summary in context", argumentHint: "<instructions>", source: ""),
            CommandView(name: "context", description: "Show current context usage", argumentHint: "", source: ""),
            CommandView(name: "cost", description: "Show the total cost and duration of the session", argumentHint: "", source: ""),
            CommandView(name: "code-review:review", description: "Review the current diff for correctness bugs", argumentHint: "[level]", source: "code-review"),
            CommandView(name: "init", description: "Initialize a new CLAUDE.md file", argumentHint: "", source: ""),
        ]
        return view
    }()

    static let settingsCodex: SettingsView = {
        let efforts = ["low", "medium", "high", "xhigh", "max", "ultra"]
        let presets: [(String, String, String)] = [
            ("read-only", "on-request", "read-only"),
            ("auto", "on-request", "workspace-write"),
            ("full-access", "never", "danger-full-access"),
        ]
        return SettingsView(
            models: [
                ModelChoice(value: "gpt-6-astra", displayName: "GPT-6-Astra", description: "Frontier agentic coding model.", efforts: efforts, current: true, reported: false, defaultEffort: "medium"),
                ModelChoice(value: "gpt-6-sol", displayName: "GPT-6-Sol", description: "Smaller, faster and cheaper.", efforts: efforts, current: false, reported: false, defaultEffort: "medium"),
            ],
            efforts: efforts.map { EffortChoice(value: $0, current: $0 == "medium", default: $0 == "medium", reported: false) },
            modes: presets.map { preset, approval, sandbox in
                ModeChoice(
                    value: .codex(approvalPolicy: approval, sandbox: sandbox, preset: preset),
                    current: preset == "auto", reported: false, stopsAsking: approval == "never")
            },
            cycleMode: false, commands: [], changeByTyping: nil, effortRefusal: nil, modeRefusal: nil,
            modelRefusal: nil)
    }()

    static let settingsClaudePty = SettingsView(
        models: [ModelChoice(value: "claude-sonnet-5", displayName: "", description: "", efforts: [], current: true, reported: true, defaultEffort: nil)],
        efforts: [EffortChoice(value: "high", current: true, default: false, reported: true)],
        modes: [ModeChoice(value: .claude("acceptEdits"), current: true, reported: false, stopsAsking: false)],
        cycleMode: true, commands: [],
        changeByTyping: "To change the model or effort, type /model <name> or /effort <level> in the composer.",
        effortRefusal: nil, modeRefusal: "Terminal Claude changes mode only by cycling through its modes.",
        modelRefusal: nil)

    static let photoData: Data = {
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: 120, height: 80))
        return renderer.jpegData(withCompressionQuality: 0.9) { context in
            UIColor(red: 0.35, green: 0.55, blue: 0.75, alpha: 1).setFill()
            context.fill(CGRect(x: 0, y: 0, width: 120, height: 80))
            UIColor(red: 0.95, green: 0.85, blue: 0.45, alpha: 1).setFill()
            context.fill(CGRect(x: 18, y: 16, width: 36, height: 36))
        }
    }()

    /// An agent's working tree: a changed file, another of the same name,
    /// and a new file.
    static let reviewPatch = """
        diff --git a/crates/amux-ui/src/pairing/errors.rs b/crates/amux-ui/src/pairing/errors.rs
        index 1111111..2222222 100644
        --- a/crates/amux-ui/src/pairing/errors.rs
        +++ b/crates/amux-ui/src/pairing/errors.rs
        @@ -8,11 +8,10 @@ use crate::codes::Code;
         /// Why a pairing did not happen.
         pub enum PairError {
             Expired,
        -    Refused,
        -    Unknown,
        -    Busy,
        +    Refused(String),
        +    Busy { retry_after_ms: u64 },
         }
         
         impl PairError {
        -    pub fn message(&self) -> &str {
        +    pub fn message(&self) -> String {
                 match self {
        diff --git a/crates/amux-ui/tests/errors.rs b/crates/amux-ui/tests/errors.rs
        index 3333333..4444444 100644
        --- a/crates/amux-ui/tests/errors.rs
        +++ b/crates/amux-ui/tests/errors.rs
        @@ -1,4 +1,5 @@
         use amux_ui::pairing::PairError;
        +use amux_ui::pairing::describe;
         
         #[test]
         fn every_error_has_one_string() {
        diff --git a/docs/PAIRING.md b/docs/PAIRING.md
        new file mode 100644
        index 0000000..5555555
        --- /dev/null
        +++ b/docs/PAIRING.md
        @@ -0,0 +1,3 @@
        +# Pairing
        +
        +A refused pairing now says why.

        """

    static let review = FrozenReview(
        diff: Diff(
            head: "4f2a9c1", base: DiffBase(base: .workingTree(Empty())), mergeBase: nil,
            patch: BlobRef(hash: [5, 5, 5], name: "patch", mime: "text/x-diff", size: UInt64(reviewPatch.utf8.count))),
        patch: reviewPatch)

    /// Two comments: one on a changed line, one on a new file's line.
    static func comment(_ model: ReviewModel) {
        model.begin(at: ReviewLine(file: 0, hunk: 0, line: 5))
        model.comment("Say what refused it, not only that it was refused.")
        model.begin(at: ReviewLine(file: 2, hunk: 0, line: 2))
        model.comment("Link the error table here.")
    }

    static func writtenReview() -> ReviewModel {
        let model = ReviewModel(review: review)
        comment(model)
        return model
    }

    static let photo = BlobRef(hash: [7, 7, 7], name: "screen.jpg", mime: "image/jpeg", size: 184_320)
    static let log = BlobRef(hash: [8, 8, 8], name: "pairing.log", mime: "text/plain", size: 12_288)
    static let images: [[UInt8]: Data] = [photo.hash: photoData, Array("screen.jpg".utf8): photoData]

    static func bytes(_ blob: BlobRef) -> Data? { images[blob.hash] }

    static let subject = ChatSubject(
        name: "refactor-auth", host: "Studio", directory: "/Users/ada/src/amux", presence: .online,
        away: nil)
    static let awaySubject = ChatSubject(
        name: "refactor-auth", host: "Studio", directory: "/Users/ada/src/amux", presence: .away,
        away: .plain)

    static let markdown = """
        Done. The three arms are **one** now:

        - `Code::NotFound` and `Code::InvalidPin` share one string
        - the new test asserts on it

        ```rust
        _ => "could not pair, check the code",
        ```
        """

    static let plan = """
        # Collapse the pairing failures onto one message

        The client maps gRPC statuses onto distinct strings in three places. The protocol refuses \
        to distinguish them, so the client must not either.

        1. One arm, one string
        2. Update the three specs
        """

    static let diff = """
        @@ -12,6 +12,5 @@
           match status {
        -      Code::NotFound => "no such host",
        -      Code::InvalidPin => "wrong PIN",
        +      _ => "could not pair, check the code",
           }
        """

    static let formSchema = """
        {"type":"object","required":["repository"],"properties":{
          "repository":{"type":"string","title":"Repository","default":"jlw/amux"},
          "labels":{"type":"string","title":"Labels","enum":["bug","ios","docs"]},
          "assign":{"type":"boolean","title":"Assign to me","default":true}}}
        """

    static let redactionQuestion = QuestionView(
        header: "Owner", question: "Which crate should own the redaction table?", multiSelect: false,
        options: [
            OptionView(label: "amux-core", description: "Every client already depends on it", preview: "", recommended: true),
            OptionView(label: "amux-ui", description: "Closest to where the copy is used", preview: "", recommended: false),
            OptionView(label: "A new crate", description: "", preview: "", recommended: false),
        ], allowOther: true, secret: false)

    static let platformQuestion = QuestionView(
        header: "Platforms", question: "Which platforms should the release build?", multiSelect: true,
        options: [
            OptionView(label: "macOS", description: "", preview: "", recommended: false),
            OptionView(label: "Linux", description: "x86_64 and arm64", preview: "", recommended: false),
            OptionView(label: "Windows", description: "", preview: "", recommended: false),
        ], allowOther: true, secret: false)

    static let previewQuestion = QuestionView(
        header: "Layout", question: "Which layout for the host list?", multiSelect: false,
        options: [
            OptionView(label: "Cards", description: "One card per host", preview: """
                ┌──────────────────────┐
                │ Studio        ● live │
                │ ~/src/amux  3 agents │
                ├──────────────────────┤
                │ Laptop       ○ away  │
                └──────────────────────┘
                """, recommended: false),
            OptionView(label: "Grouped list", description: "", preview: "Studio · live\nLaptop · away", recommended: false),
        ], allowOther: false, secret: false)

    static let secretQuestion = QuestionView(
        header: "Token", question: "Paste the deploy token", multiSelect: false, options: [],
        allowOther: true, secret: true)

    static let threeQuestions = [
        platformQuestion,
        QuestionView(
            header: "Rollout", question: "How should the migration roll out?", multiSelect: false,
            options: [
                OptionView(label: "Behind a flag", description: "Off by default for one release", preview: "", recommended: false),
                OptionView(label: "All at once", description: "", preview: "", recommended: false),
            ], allowOther: true, secret: false),
        QuestionView(
            header: "Logging", question: "What should be logged?", multiSelect: false,
            options: [
                OptionView(label: "Everything", description: "", preview: "", recommended: false),
                OptionView(label: "Nothing", description: "", preview: "", recommended: false),
            ], allowOther: true, secret: false),
    ]

    static let sdkPermissionChoices = [
        Choice(outcome: .allowOnce, primary: true, takesNote: false),
        Choice(outcome: .allowAlways(subjects: ["cargo test"], directories: [], mode: "", scope: .project, label: ""), primary: false, takesNote: false),
        Choice(outcome: .allowForSession, primary: false, takesNote: false),
        Choice(outcome: .deny(stops: false), primary: false, takesNote: true),
        Choice(outcome: .denyAndStop, primary: false, takesNote: true),
    ]

    static func card(
        _ kind: Kind, _ body: AskBody, _ choices: [Choice], state: CardState = .open, count: UInt = 1
    ) -> AskCard {
        AskCard(
            kind: kind, key: "ask-1", itemKey: "x6", position: 1, count: count, body: body,
            choices: choices, questionNote: kind != .claudePty, state: state)
    }

    static let runningCommand = ScriptedChat.row("rc", 50, .command(
        command: "cargo test -p amux-ui", state: .running, outputHead: [], moreLines: 0,
        durationMs: nil, exitCode: nil))

    static let askedCommand = ScriptedChat.row("x6", 60, .command(
        command: "cargo test -p amux-ui", state: .pending, outputHead: [], moreLines: 0,
        durationMs: nil, exitCode: nil), attention: true)

    static let conversation: [Row] = {
        func r(
            _ id: String, _ order: UInt64, _ kind: RowKind, attention: Bool = false,
            decision: Decision? = nil, run: RunInfo? = nil, parent: String? = nil
        ) -> Row {
            ScriptedChat.row(id, order, kind, attention: attention, decision: decision, run: run, parent: parent)
        }
        return [
            r("c01", 1, .boundary(kind: .started, cause: "")),
            r("c02", 2, .prompt(text: [.text("Collapse the pairing errors onto one string.")], steered: false)),
            r("c03", 3, .thinking(text: "", open: false, durationMs: 8_000)),
            r("c04", 4, .explore(verb: .search, subject: "INVALID_PIN", state: .succeeded),
              run: RunInfo(newest: "c04", oldest: "c04", reads: 4, searches: 2, len: 6, anchor: "crates/amux-ui/src/pairing.rs", isSummary: true, openBelow: false)),
            r("c05", 5, .fileChange(files: [FileRow(path: "crates/amux-ui/src/pairing.rs", change: .edited, added: 9, removed: 14)], state: .succeeded)),
            r("c06", 6, .command(command: "cargo check -p amux-ui", state: .failed, outputHead: ["error[E0308]: mismatched types"], moreLines: 214, durationMs: 4_200, exitCode: 101)),
            r("c07", 7, .command(command: "cargo test -p amux-ui", state: .succeeded, outputHead: [], moreLines: 0, durationMs: 12_000, exitCode: 0),
              decision: Decision(outcome: .allowed, elsewhere: false, note: nil, scope: "this session")),
            r("c08", 8, .fileChange(files: [FileRow(path: "crates/amux-ui/tests/spec/pairing_copy.rs", change: .created(lines: 38), added: 38, removed: 0)], state: .succeeded)),
            r("c09", 9, .command(command: "rm -rf target", state: .denied, outputHead: [], moreLines: 0, durationMs: nil, exitCode: nil),
              decision: Decision(outcome: .denied, elsewhere: false, note: "Use cargo clean instead", scope: nil)),
            r("c10", 10, .prose(text: [.text("Done. The three arms are one now, and the new test asserts on the single string.")], streaming: false, workingNote: false)),
            r("c11", 11, .turnEnd(failed: false, costUsd: nil, durationMs: 102_000)),
        ]
    }()

    static let family = FamilyHeader(
        children: [
            FleetCard(agent: AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: 3, count: 16)),
                      name: "worker-2", kind: .claudeSdk, attention: .needsYou, cwd: "", lastActivityMs: 0,
                      host: "Laptop", hostPresence: .online, children: 0, familyAttention: .needsYou,
                      members: 1, membersNeedYou: 1, exitCause: nil, workingOn: nil),
            FleetCard(agent: AgentKey(host: Array(repeating: 1, count: 16), agent: Array(repeating: 4, count: 16)),
                      name: "worker-3", kind: .codex, attention: .working, cwd: "", lastActivityMs: 0,
                      host: "Studio", hostPresence: .online, children: 0, familyAttention: .working,
                      members: 1, membersNeedYou: 0, exitCause: nil, workingOn: nil),
        ],
        attention: .needsYou, parent: nil)
}

/// A component on a fixed, production-coloured canvas.
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
            example.build()
                .padding(example.family == .chat ? 0 : 18)
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
/// in-process reporting path.
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
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 24) {
                ForEach(ComponentCatalog.examples) { example in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(example.id).font(.headline.monospaced())
                        ComponentExampleView(example: example, appearance: appearance.colorScheme)
                    }
                }
            }
            .padding(20)
        }
    }
}

#Preview("Component Catalog") {
    ComponentCatalogGallery()
}
