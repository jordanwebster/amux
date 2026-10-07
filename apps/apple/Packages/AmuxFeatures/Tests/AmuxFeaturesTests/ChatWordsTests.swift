import AmuxCore
import XCTest

@testable import AmuxFeatures

final class ChatWordsTests: XCTestCase {
    private func row(_ kind: RowKind, attention: Bool = false, decision: Decision? = nil) -> Row {
        Row(
            id: "k", order: 1, atMs: 0, kind: kind, collapsed: false, attention: attention,
            decision: decision, parent: nil, run: nil)
    }

    func testDurationsAndCountsReadTheWayTheVocabularyWritesThem() {
        XCTAssertEqual(ChatWords.duration(102_000), "1m 42s")
        XCTAssertEqual(ChatWords.duration(8_000), "8s")
        XCTAssertEqual(ChatWords.duration(4_200), "4.2s")
        XCTAssertEqual(ChatWords.tokens(148_000), "148k")
        XCTAssertEqual(
            ChatWords.compaction(before: 148_000, after: 22_000, automatic: false),
            "Compacted · 148k → 22k")
        XCTAssertEqual(ChatWords.turnEnd(durationMs: 102_000, costUsd: nil, failed: false), "Worked 1m 42s")
        XCTAssertEqual(ChatWords.thinking(open: false, durationMs: 8_000), "Thought for 8s")
    }

    func testARunSaysWhatItDidAndThatItContinuesBelow() {
        let run = Run(
            first: "a", last: "b", steps: 6, live: false, openBelow: false,
            unresolvedFailure: false, counts: RunCounts(commands: 0, edits: 0, reads: 4, searches: 2, subagents: 0, other: 0),
            recent: nil)
        XCTAssertEqual(ChatWords.run(run), "4 reads · 2 searches")
        let open = Run(
            first: "a", last: "b", steps: 41, live: false, openBelow: true,
            unresolvedFailure: false, counts: RunCounts(commands: 0, edits: 0, reads: 40, searches: 0, subagents: 0, other: 1),
            recent: nil)
        XCTAssertEqual(ChatWords.run(open), "40+ reads · 1+ more")
    }

    func testACallsVerbSaysWhetherItWaitsRunsWasRefusedOrRan() {
        let words = { (state: ToolStateView, row: Row) in
            ChatWords.verb(state, row, wants: "Wants to run", doing: "Running", done: "Ran")
        }
        XCTAssertEqual(words(.pending, row(.stopped)), "Wants to run")
        XCTAssertEqual(words(.running, row(.stopped, attention: true)), "Wants to run")
        XCTAssertEqual(words(.running, row(.stopped)), "Running")
        XCTAssertEqual(words(.succeeded, row(.stopped)), "Ran")
        let denied = Decision(outcome: .denied, elsewhere: false, granted: nil, note: "Use cargo clean")
        XCTAssertEqual(words(.succeeded, row(.stopped, decision: denied)), "Denied")
        let allowed = Decision(outcome: .allowed, elsewhere: false, granted: .session, note: nil)
        XCTAssertEqual(words(.succeeded, row(.stopped, decision: allowed)), "Allowed")
        XCTAssertEqual(words(.running, row(.stopped, decision: allowed)), "Running")
        XCTAssertEqual(words(.failed, row(.stopped, decision: allowed)), "Ran")
    }

    func testAVerbThatNamesTheOutcomeLeavesItOutOfTheMeta() {
        let allowed = Decision(outcome: .allowed, elsewhere: false, granted: .session, note: nil)
        XCTAssertEqual(
            ChatWords.meta(["12s"], row(.stopped, decision: allowed), verb: "Allowed"),
            "12s · this session")
        let denied = Decision(outcome: .denied, elsewhere: false, granted: nil, note: "Use cargo clean")
        XCTAssertEqual(
            ChatWords.meta([], row(.stopped, decision: denied), verb: "Denied", note: false), "")
    }

    func testTheRailRunsBetweenGridRowsAndBreaksAtProse() {
        let tool = row(.background(command: "npm run dev", running: true, durationMs: nil))
        let prose = row(.prose(text: [.text("Done.")], streaming: false, workingNote: false))
        XCTAssertEqual(RailJoin.of(tool, next: tool), RailJoin(continues: true))
        XCTAssertEqual(RailJoin.of(tool, next: prose), RailJoin(continues: false))
        XCTAssertEqual(RailJoin.of(tool, next: nil), RailJoin(continues: false))
        XCTAssertEqual(RailJoin.of(prose, next: tool), .none)
        var step = tool
        step.parent = "agent"
        XCTAssertEqual(RailJoin.of(step, next: tool), RailJoin(continues: true, nested: true))
    }

    func testADecisionIsMetaWithScopeNoteAndWhereItWasAnswered() {
        let allowed = Decision(outcome: .allowed, elsewhere: false, granted: .session, note: nil)
        XCTAssertEqual(ChatWords.meta(["12s"], row(.stopped, decision: allowed)), "12s · allowed · this session")
        let denied = Decision(outcome: .denied, elsewhere: false, granted: nil, note: "Use cargo clean\nplease")
        XCTAssertEqual(
            ChatWords.meta(["denied"], row(.stopped, decision: denied), verb: "Denied"),
            "“Use cargo clean”")
        let elsewhere = Decision(outcome: .allowed, elsewhere: true, granted: nil, note: nil)
        XCTAssertEqual(ChatWords.decision(elsewhere), "allowed · in the terminal")
    }

    func testADecidedPermissionSaysWhatWasGranted() {
        let always = Decision(
            outcome: .allowed, elsewhere: false,
            granted: .claude(subjects: ["cargo test"], directories: [], mode: "", modeName: "", savedTo: .project),
            note: nil)
        XCTAssertEqual(ChatWords.decision(always), "allowed · always cargo test in this project")
        let switched = Decision(
            outcome: .allowed, elsewhere: false,
            granted: .claude(subjects: [], directories: [], mode: "acceptEdits", modeName: "Accept edits", savedTo: .session),
            note: nil)
        XCTAssertEqual(ChatWords.decision(switched), "allowed · switched to Accept edits")
        let prefix = Decision(outcome: .allowed, elsewhere: false, granted: .commandPrefix(words: ["curl", "-s"]), note: nil)
        XCTAssertEqual(ChatWords.decision(prefix), "allowed · commands starting with curl -s")
        let hosts = Decision(outcome: .allowed, elsewhere: false, granted: .networkHosts(hosts: ["example.com"]), note: nil)
        XCTAssertEqual(ChatWords.decision(hosts), "allowed · network access to example.com")
    }

    func testPermissionsAndModesAreWordedFromTheCatalogue() {
        func permission(_ value: String, _ name: String, reported: Bool = false, neverAsks: Bool = false, settable: Bool = true) -> PermissionChoice {
            PermissionChoice(
                value: value, displayName: name, current: false, reported: reported, normal: false,
                neverAsks: neverAsks, settable: settable)
        }
        XCTAssertEqual(ChatWords.permission(permission("acceptEdits", "Accept edits")), "Accept edits")
        XCTAssertEqual(ChatWords.permission(permission("", "")), "Custom")
        XCTAssertEqual(ChatWords.permissionDetail(permission("auto", "Auto")), "")
        XCTAssertEqual(ChatWords.permissionDetail(permission("full", "Full", neverAsks: true)), "Acts without asking")
        XCTAssertEqual(ChatWords.permissionDetail(permission("odd", "", reported: true)), "Reported by the agent")
        XCTAssertEqual(ChatWords.permissionDetail(permission("auto", "Auto", settable: false)), "Can’t be picked from here")
        XCTAssertEqual(
            ChatWords.mode(ModeChoice(value: "plan", displayName: "Plan", current: true, reported: false, normal: false, settable: true)),
            "Plan")
        let switching = Choice(
            outcome: .allowAlways(subjects: [], directories: [], mode: "acceptEdits", modeName: "Accept edits", scope: .session, label: ""),
            primary: false, takesNote: false)
        XCTAssertEqual(ChatWords.choice(switching), "Switch to Accept edits")
        XCTAssertEqual(ChatWords.scopeRow(switching).title, "Switch to Accept edits")
    }

    func testChoicesAreStatedAsOutcomes() {
        let always = Choice(
            outcome: .allowAlways(subjects: ["cargo test"], directories: [], mode: "", modeName: "", scope: .project, label: ""),
            primary: false, takesNote: false)
        XCTAssertEqual(ChatWords.choice(always), "Always allow cargo test in this project")
        XCTAssertEqual(
            ChatWords.choice(Choice(outcome: .allowSimilar(prefix: ["curl", "-s"]), primary: false, takesNote: false)),
            "Allow commands starting with curl -s")
        XCTAssertEqual(
            ChatWords.choice(Choice(outcome: .deny(stops: false), primary: false, takesNote: true)), "Deny…")
        XCTAssertEqual(
            ChatWords.choice(Choice(outcome: .allowNetwork(hosts: ["localhost"]), primary: false, takesNote: false)),
            "Allow network access to localhost")
    }

    func testTheActivityLineNamesWhatIsHappeningAndForHowLong() {
        XCTAssertEqual(
            ChatWords.activity(.running(key: "k"), elapsedMs: 12_400, subject: "cargo test"),
            "Running cargo test · 12s")
        XCTAssertEqual(ChatWords.activity(.working, elapsedMs: 24_000, subject: nil), "Working · 24s")
        XCTAssertEqual(ChatWords.activity(.thinking, elapsedMs: 24_000, subject: nil), "Thinking")
        XCTAssertEqual(ChatWords.activity(.subagents(count: 3), elapsedMs: 5_000, subject: nil), "3 subagents working")
        XCTAssertEqual(ChatWords.activity(.compacting, elapsedMs: 5_000, subject: nil), "Compacting")
        XCTAssertEqual(
            ChatWords.activity(.retrying(attempt: 2, maxAttempts: 10, retryAtMs: nil), elapsedMs: 0, subject: nil),
            "Retrying · attempt 2 of 10")
    }

    /// The line draws its subject in mono and its time at the right end, so
    /// the parts come apart.
    func testTheActivityLineComesInWordsSubjectAndTime() {
        let parts = ChatWords.activityParts(.running(key: "k"), elapsedMs: 12_400, subject: "cargo test")
        XCTAssertEqual(parts.words, "Running")
        XCTAssertEqual(parts.subject, "cargo test")
        XCTAssertEqual(parts.time, "12s")
        XCTAssertNil(ChatWords.activityParts(.running(key: "k"), elapsedMs: 0, subject: "").subject)
    }

    /// The dock's head names the task in progress, else the next one to do.
    func testTheTaskChipNamesTheTaskInProgress() {
        let tasks = TasksView(done: 1, total: 3, current: "Splitting the lexer", entries: [
            TaskLine(subject: "Read the parser", mark: .done),
            TaskLine(subject: "Split the lexer", mark: .current),
            TaskLine(subject: "Run the tests", mark: .todo),
        ])
        XCTAssertEqual(ChatWords.headTask(tasks), "Split the lexer")
        var waiting = tasks
        waiting.entries[1].mark = .done
        XCTAssertEqual(ChatWords.headTask(waiting), "Run the tests")
    }

    func testTheEmptyFieldSaysWhoAMessageGoesToOrWhySendingWaits() {
        XCTAssertEqual(
            ChatWords.placeholder(.send, agent: "refactor-auth", host: "Studio", away: nil, working: false),
            "Message refactor-auth")
        XCTAssertEqual(
            ChatWords.placeholder(.send, agent: "refactor-auth", host: "Studio", away: nil, working: true),
            "Queue a message")
        XCTAssertEqual(
            ChatWords.placeholder(.disabled(.detached), agent: "a", host: "Studio", away: .plain, working: false),
            "Studio is out of reach · your draft is kept")
        XCTAssertEqual(
            ChatWords.placeholder(.resume, agent: "a", host: "Studio", away: nil, working: false),
            "a has exited · a message resumes it")
    }

    func testAReachedLimitIsNamedWithItsResetAboveAComposerThatStillSends() {
        let now = Date(milliseconds: 1_800_000_000_000)
        let nowMs: Int64 = 1_800_000_000_000
        let usage = UsageView(blocked: true, windows: [
            UsageWindowView(label: .fiveHour, usedPercent: 100, state: .blocked, resetsAtMs: nowMs + 7_200_000),
            UsageWindowView(label: .weekly(model: nil), usedPercent: 64, state: .ok, resetsAtMs: nowMs + 259_200_000),
        ], credits: nil)
        let reached = ChatWords.limitReached(usage, now: now)
        XCTAssertTrue(reached.hasPrefix("5-hour limit reached · resets "), reached)
        let overview = Overview(jobs: [], failedServers: [], changes: nil, tasks: nil, usageNearLimit: usage)
        XCTAssertEqual(ChatWords.strip(context: nil, overview: overview, now: now).first?.text, reached)
        XCTAssertEqual(ChatWords.strip(context: nil, overview: overview, now: now).first?.warn, true)
        var past = usage
        past.windows[0].resetsAtMs = nowMs - 1
        XCTAssertEqual(ChatWords.limitReached(past, now: now), "5-hour limit reached", "a reset already past is not promised")
        XCTAssertEqual(
            ChatWords.limitReached(UsageView(blocked: true, windows: [], credits: nil), now: now), "Usage limit reached")
    }

    func testSigningInIsAskedForOnTheAgentsHost() {
        XCTAssertEqual(ChatWords.needsSignIn(.claudeSdk), "Claude needs you to sign in")
        XCTAssertEqual(ChatWords.signInSteps(.claudeSdk, host: "desk"), "Run claude and sign in with /login on desk.")
        XCTAssertEqual(ChatWords.needsSignIn(.codex), "Codex needs you to sign in")
        XCTAssertEqual(ChatWords.signInSteps(.codex, host: "desk"), "Run codex login on desk.")
    }

    func testAQueuedPromptSaysWhetherItWasSteered() {
        let queued = QueuedRow(
            inputId: [1], text: [], steered: false, canWithdraw: true, canSendNow: true,
            fromAgent: nil)
        XCTAssertEqual(ChatWords.queued(queued), "queued")
        var steered = queued
        steered.steered = true
        XCTAssertEqual(ChatWords.queued(steered), "steered")
        XCTAssertEqual(ChatWords.underway(.mayNotHaveArrived, host: "Studio"), "may not have arrived")
        XCTAssertEqual(ChatWords.underway(.sending(waiting: true), host: "Studio"), "waiting for Studio…")
    }

    func testASecretAnswerIsNeverShown() {
        XCTAssertEqual(ChatWords.answer(AnswerView(picked: [], hidden: true, note: nil, other: nil)), "answered (hidden)")
        XCTAssertEqual(
            ChatWords.answer(AnswerView(picked: ["macOS"], hidden: false, note: nil, other: "BSD")), "macOS, “BSD”")
    }

    func testAQuestionLeftUnansweredReadsSkipped() {
        let left = AnswerView(picked: [], hidden: false, note: "later", other: nil)
        XCTAssertTrue(ChatWords.skipped(left))
        XCTAssertEqual(ChatWords.answer(left), "Skipped")
        XCTAssertFalse(ChatWords.skipped(AnswerView(picked: [], hidden: false, note: nil, other: "BSD")))
        XCTAssertFalse(ChatWords.skipped(AnswerView(picked: [], hidden: true, note: nil, other: nil)))
    }

    private func field(
        _ name: String, _ kind: FormFieldKind, required: Bool = false, initial: String = ""
    ) -> FormField {
        FormField(name: name, title: name, description: "", required: required, kind: kind, initial: initial)
    }

    private var asked: [FormField] {
        [
            field("repo", .text, required: true),
            field("count", .number(integer: true)),
            field("assign", .toggle, initial: "true"),
        ]
    }

    func testRequiredFieldsGateSubmitAndTheContentIsTyped() {
        let entries = FormEntry.entries(asked)
        XCTAssertEqual(entries.map(\.name), ["repo", "count", "assign"])
        XCTAssertFalse(entries[0].valid)
        var filled = entries
        filled[0].value = "jlw/amux"
        filled[1].value = "3"
        XCTAssertTrue(filled.allSatisfy(\.valid))
        XCTAssertEqual(FormEntry.content(filled), #"{"assign":true,"count":3,"repo":"jlw\/amux"}"#)
    }

    func testAFieldOfSeveralPicksAnswersWithAnArray() {
        var entries = FormEntry.entries([field("labels", .many(options: ["bug", "ios", "docs"]), required: true)])
        XCTAssertFalse(entries[0].valid, "a required field with nothing picked")
        XCTAssertEqual(FormEntry.content(entries), "{}")
        entries[0].value = "bug\ndocs"
        XCTAssertTrue(entries[0].valid)
        XCTAssertEqual(FormEntry.content(entries), #"{"labels":["bug","docs"]}"#)
    }

    func testAFormDrawnAgainRestoresTheValuesItKept() {
        var typed = FormEntry.entries(asked)
        typed[0].value = "jlw/amux"
        typed[2].value = "false"
        let kept = FormEntry.values(typed)
        XCTAssertEqual(kept, ["repo": "jlw/amux", "count": "", "assign": "false"])
        XCTAssertEqual(FormEntry.entries(asked, kept: kept), typed, "drawn again, it holds what was typed")
        XCTAssertEqual(FormEntry.entries(asked, kept: nil), FormEntry.entries(asked), "a new form starts as written")
        XCTAssertEqual(
            FormEntry.entries(asked, kept: ["repo": "jlw/amux", "gone": "x"]).map(\.value), ["jlw/amux", "", "true"],
            "a kept name the form lacks is ignored, and a field never typed keeps its default")
    }

    func testEveryNameProblemHasWords() {
        for problem in AgentNameProblem.allCases {
            XCTAssertFalse(ChatWords.nameProblem(problem).isEmpty, "\(problem)")
        }
    }

    func testTheReviewSaysHowMuchChangedAndWhatAttachingCarries() {
        XCTAssertEqual(ChatWords.changes(files: 1, added: 18, removed: 2), "1 file, 18 added, 2 removed")
        XCTAssertEqual(ChatWords.changes(files: 4, added: 0, removed: 28), "4 files, 0 added, 28 removed")
        XCTAssertEqual(ChatWords.attachReview(1), "Attach Review · 1 comment")
        XCTAssertEqual(ChatWords.attachReview(3), "Attach Review · 3 comments")
        XCTAssertEqual(ChatWords.commentOn(lines: 1), "Comment on 1 line")
        XCTAssertEqual(ChatWords.commentOn(lines: 5), "Comment on 5 lines")
        XCTAssertEqual(ChatWords.chip(.review(comments: 2, patch: nil)), "Review · 2 comments")
        let added = DiffLine(kind: .added, text: "    Refused(String),", comments: [], newLine: 11, oldLine: nil)
        XCTAssertEqual(ChatWords.spoken(added), "Added line 11, Refused(String),")
        let removed = DiffLine(kind: .removed, text: "    Busy,", comments: [], newLine: nil, oldLine: 13)
        XCTAssertEqual(ChatWords.spoken(removed), "Removed line 13, Busy,")
    }

    /// The chip names the model the way the settings card lists it: by the
    /// offered display name, else the id the agent reports.
    func testTheModelChipReadsTheNameTheSettingsCardGivesTheModel() {
        let frame = ChatFrame(
            agent: AgentKey(host: [1], agent: [2]), name: "a", kind: .claudeSdk, phase: .idle,
            composer: ComposerView(mode: .send, activity: nil), connection: .live, caughtUp: true,
            hasOlder: false, arrivalsHeld: false, queue: [], underway: [], refused: [], askInput: nil,
            context: nil, effort: nil, ended: nil, git: nil, model: "claude-sonnet-5",
            permission: "acceptEdits", signIn: nil, waiting: nil)
        let permission = PermissionChoice(
            value: "acceptEdits", displayName: "Accept edits", current: true, reported: false,
            normal: false, neverAsks: false, settable: true)
        func settings(_ models: [ModelChoice]) -> SettingsView {
            SettingsView(
                models: models, efforts: [], permissions: [permission], modes: [],
                cyclePermission: false, commands: [], changeByTyping: nil, effortRefusal: nil,
                modelRefusal: nil, permissionRefusal: nil)
        }
        let offered = settings([
            ModelChoice(
                value: "opus", displayName: "Opus 5.5", description: "", efforts: [], current: false,
                reported: false, defaultEffort: nil),
            ModelChoice(
                value: "sonnet", displayName: "Sonnet 5", description: "", efforts: [], current: true,
                reported: false, defaultEffort: nil),
        ])
        let chip = ChatWords.chip(frame, offered)
        XCTAssertEqual(chip?.model, "Sonnet 5")
        XCTAssertEqual(chip?.detail, "Accept edits")
        var effort = frame
        effort.effort = "high"
        XCTAssertEqual(ChatWords.chip(effort, offered)?.detail, "high", "the effort, when reported, stands for the mode")

        let reported = settings([
            ModelChoice(
                value: "claude-sonnet-5", displayName: "", description: "", efforts: [], current: true,
                reported: true, defaultEffort: nil),
        ])
        XCTAssertEqual(ChatWords.chip(frame, reported)?.model, "claude-sonnet-5")
        XCTAssertEqual(ChatWords.chip(frame, nil)?.model, "claude-sonnet-5")
    }
}
