import AmuxCore
import Foundation

/// The conversation every talking screen opens, written in the Claude PTY
/// layer's own row vocabulary rather than a shape invented for the phone.
public enum Transcript {
    public static func prompt(_ id: Int, seq: Int, text: String) -> FeedEntry {
        row(id, seq, .object([
            "entry": .string("prompt"),
            "text": .string(text),
            "content": .array([.object(["segment": .string("prose"), "value": .string(text)])]),
            "source": .object(["source": .string("typed")]),
            "prompt_id": .string("prompt-\(id)"),
        ]))
    }

    public static func message(
        _ id: Int, seq: Int, text: String, final: Bool = true, interrupted: Bool = false
    ) -> FeedEntry {
        let finality: JSONValue = if interrupted {
            .object(["finality": .string("interrupted")])
        } else if final {
            .object(["finality": .string("final"), "stop_reason": .string("end_turn")])
        } else {
            .object(["finality": .string("open")])
        }
        return row(id, seq, .object([
            "entry": .string("message"),
            "message_id": .string("msg_\(id)"),
            "segments": .array([.string(text)]),
            "content": .array([.object(["segment": .string("prose"), "value": .string(text)])]),
            "finality": finality,
        ]))
    }

    public static func thinking(_ id: Int, seq: Int, seconds: Int) -> FeedEntry {
        row(id, seq, .object([
            "entry": .string("thinking"),
            "duration_ms": .int(seconds * 1000),
            "redacted": .bool(false),
        ]))
    }

    public static func read(_ id: Int, seq: Int, path: String, grouped: Bool = false) -> FeedEntry {
        tool(id, seq, name: "Read",
             invocation: .object(["tool": .string("read"), "file_path": .string(path)]),
             outcome: .object([
                "outcome": .string("success"),
                "facts": .object(["facts": .string("output"), "head": .string("140 lines"),
                                  "truncated": .bool(false)]),
             ]),
             grouped: grouped)
    }

    public static func search(_ id: Int, seq: Int, query: String, grouped: Bool = false) -> FeedEntry {
        tool(id, seq, name: "Grep",
             invocation: .object(["tool": .string("query"), "text": .string(query)]),
             outcome: .object([
                "outcome": .string("success"),
                "facts": .object(["facts": .string("output"), "head": .string("12 matches"),
                                  "truncated": .bool(false)]),
             ]),
             grouped: grouped)
    }

    public static func ran(
        _ id: Int, seq: Int, command: String, output: String, truncated: Bool = false
    ) -> FeedEntry {
        tool(id, seq, name: "Bash",
             invocation: .object(["tool": .string("bash"), "command": .string(command),
                                  "description": .null]),
             outcome: .object([
                "outcome": .string("success"),
                "facts": .object(["facts": .string("output"), "head": .string(output),
                                  "truncated": .bool(truncated)]),
             ]))
    }

    public static func edit(
        _ id: Int, seq: Int, path: String, added: Int, removed: Int, lines: [String]
    ) -> FeedEntry {
        tool(id, seq, name: "Edit",
             invocation: .object(["tool": .string("edit"), "file_path": .string(path),
                                  "replace_all": .bool(false)]),
             outcome: .object([
                "outcome": .string("success"),
                "facts": .object([
                    "facts": .string("edit"),
                    "file_path": .string(path),
                    "added": .int(added),
                    "removed": .int(removed),
                    "document": .object([
                        "numbering": .string("absolute"),
                        "hunks": .array([.object([
                            "old_start": .int(118),
                            "new_start": .int(118),
                            "header": .string("@@ -118,14 +118,9 @@"),
                            "lines": .array(lines.map { .string($0) }),
                        ])]),
                        "truncated": .bool(false),
                    ]),
                ]),
             ]))
    }

    /// A command the agent asked to run and was refused. The denial is a typed
    /// fact from the transcript, never a guess made from an error string.
    public static func denied(_ id: Int, seq: Int, command: String) -> FeedEntry {
        tool(id, seq, name: "Bash",
             invocation: .object(["tool": .string("bash"), "command": .string(command),
                                  "description": .null]),
             outcome: .object(["outcome": .string("denied"), "kind": .string("user_reject")]))
    }

    public static func failed(_ id: Int, seq: Int, command: String, message: String) -> FeedEntry {
        tool(id, seq, name: "Bash",
             invocation: .object(["tool": .string("bash"), "command": .string(command),
                                  "description": .null]),
             outcome: .object(["outcome": .string("failed"), "message": .string(message)]))
    }

    /// A file written whole rather than edited.
    public static func wrote(_ id: Int, seq: Int, path: String, lines: Int) -> FeedEntry {
        tool(id, seq, name: "Write",
             invocation: .object(["tool": .string("write"), "file_path": .string(path)]),
             outcome: .object([
                "outcome": .string("success"),
                "facts": .object(["facts": .string("output"),
                                  "head": .string("\(lines) lines"),
                                  "truncated": .bool(false)]),
             ]))
    }

    /// A subagent this one started.
    public static func subagent(
        _ id: Int, seq: Int, description: String, kind: String
    ) -> FeedEntry {
        tool(id, seq, name: "Task",
             invocation: .object(["tool": .string("task"),
                                  "description": .string(description),
                                  "subagent_type": .string(kind),
                                  "background": .bool(false)]),
             outcome: .object(["outcome": .string("pending")]))
    }

    /// A message this agent sent to another one.
    public static func toAgent(_ id: Int, seq: Int, to: String, text: String) -> FeedEntry {
        tool(id, seq, name: "mcp__amux__send",
             invocation: .object(["tool": .string("amux_send"), "to": .string(to),
                                  "text": .string(text)]),
             outcome: .object(["outcome": .string("success"),
                               "facts": .object(["facts": .string("none")])]))
    }

    /// The provider itself failed, which is not the agent failing.
    public static func providerError(_ id: Int, seq: Int, message: String) -> FeedEntry {
        row(id, seq, .object([
            "entry": .string("api_error"),
            "error": .string("server_error"),
            "text": .string(message),
        ]))
    }

    /// History being compacted away, with what it cost.
    public static func compaction(_ id: Int, seq: Int, before: Int, after: Int) -> FeedEntry {
        row(id, seq, .object([
            "entry": .string("compaction"),
            "trigger": .string("auto"),
            "pre_tokens": .int(before),
            "post_tokens": .int(after),
        ]))
    }

    /// A subagent that finished on its own and said so.
    public static func subagentFinished(_ id: Int, seq: Int, text: String) -> FeedEntry {
        row(id, seq, .object([
            "entry": .string("task_notification"),
            "text": .string(text),
        ]))
    }

    /// A command still running: the tool has no result yet.
    public static func running(_ id: Int, seq: Int, command: String) -> FeedEntry {
        tool(id, seq, name: "Bash",
             invocation: .object(["tool": .string("bash"), "command": .string(command),
                                  "description": .null]),
             outcome: .object(["outcome": .string("pending")]))
    }

    /// Another agent's session ending, as this agent's transcript recorded it.
    public static func exited(_ id: Int, seq: Int, agent: String) -> FeedEntry {
        row(id, seq, .object([
            "entry": .string("agent_message"),
            "id": .string("envelope-\(id)"),
            "context": .null,
            "from": .string(agent),
            "kind": .object(["message_kind": .string("exited")]),
            "text": .string(""),
        ]))
    }

    /// A message another amux agent wrote into this one's transcript.
    public static func fromAgent(_ id: Int, seq: Int, from: String, text: String) -> FeedEntry {
        row(id, seq, .object([
            "entry": .string("agent_message"),
            "id": .string("envelope-\(id)"),
            "context": .null,
            "from": .string(from),
            "kind": .object(["message_kind": .string("message")]),
            "text": .string(text),
        ]))
    }

    public static func turnEnd(_ id: Int, seq: Int, milliseconds: Int) -> FeedEntry {
        row(id, seq, .object([
            "entry": .string("turn"),
            "duration": .object(["duration": .string("measured"), "ms": .int(milliseconds)]),
            "message_count": .int(14),
            "pending_background_agents": .null,
        ]))
    }

    public static func interrupted(_ id: Int, seq: Int) -> FeedEntry {
        row(id, seq, .object([
            "entry": .string("interruption"),
            "kind": .string("user"),
            "interrupted_message_id": .null,
        ]))
    }

    /// A row shape this build does not know. It is kept and shown as itself
    /// rather than dropped, because a transcript that quietly loses rows is
    /// worse than one that admits it saw something it cannot read.
    public static func unrecognized(_ id: Int, seq: Int, label: String) -> FeedEntry {
        row(id, seq, .object(["entry": .string("unrecognized"), "label": .string(label)]))
    }

    private static func tool(
        _ id: Int, _ seq: Int, name: String, invocation: JSONValue, outcome: JSONValue,
        grouped: Bool = false
    ) -> FeedEntry {
        row(id, seq, .object([
            "entry": .string("tool"),
            "tool_use_id": .string("toolu_\(id)"),
            "name": .string(name),
            "invocation": invocation,
            "outcome": outcome,
            "message_final": .bool(true),
            "group_with_previous": .bool(grouped),
            "message_id": .string("msg_\(id)"),
        ]))
    }

    private static func row(_ id: Int, _ seq: Int, _ kind: JSONValue) -> FeedEntry {
        FeedEntry(layer: .claudePty, row: .object([
            "id": .int(id), "seq": .int(seq), "kind": kind,
        ]))
    }

    /// The conversation as the design tells it: a prompt about pairing copy,
    /// a look around the code, a change, a check that passes, and a request to
    /// run the suite before calling it done.
    public static let pairingCopy: [FeedEntry] = [
        prompt(0, seq: 1, text: """
            Make the pairing failures indistinguishable in the client too — the error copy \
            currently leaks which host rejected us.
            """),
        thinking(1, seq: 2, seconds: 8),
        read(2, seq: 3, path: "crates/amux-ui/src/pairing.rs"),
        search(3, seq: 4, query: "\"INVALID_PIN\"", grouped: true),
        message(4, seq: 5, text: """
            Found it. The client maps three distinct gRPC statuses onto three different \
            strings, so the copy leaks exactly what the protocol deliberately hides. I've \
            collapsed them onto one message and deleted the branch.
            """),
        edit(5, seq: 6, path: "crates/amux-ui/src/pairing.rs", added: 9, removed: 14, lines: [
            "  let message = match status {",
            "-   Code::NotFound => \"no such host\",",
            "-   Code::Unauthenticated => \"wrong PIN\",",
            "-   Code::DeadlineExceeded => \"expired\",",
            "+   // One string for every failure: the",
            "+   // protocol refuses to tell them apart,",
            "+   // and so must we.",
            "+   _ => \"Pairing failed. Check the code\",",
            "  };",
        ]),
        ran(6, seq: 7, command: "cargo check -p amux-ui", output: "Finished in 4.2s"),
        message(7, seq: 8, text: """
            `cargo check` is clean. Before I call it done I'd like to run the spec suite — \
            three tests assert on the old error strings and I want to see them fail loudly \
            rather than guess.
            """),
    ]

    /// The same conversation with the turn still open: the person has asked
    /// for the suite and the command is still running.
    public static var live: [FeedEntry] {
        pairingCopy + [
            prompt(8, seq: 9, text: "Good. Now run the whole suite and tell me what breaks."),
            running(9, seq: 10, command: "cargo test --workspace"),
        ]
    }

    /// Everything an agent can write, including the shapes this build cannot
    /// read and the voices that are not the agent's own.
    ///
    /// It opens on a compaction rule because a long conversation does, and it
    /// ends with the other agent's session closing: between those two the
    /// transcript has to carry a second agent's voice in both directions, a
    /// subagent, a refusal, a failure, an interruption, a provider error and a
    /// row shape nobody has taught it yet, without any of them reading as the
    /// agent's own prose. Ordered so that a single screenful holds every one of
    /// them; the markdown the agent writes is proved by the same screen's prose.
    public static var everyKind: [FeedEntry] {
        [
            compaction(0, seq: 1, before: 148_000, after: 22_000),
            prompt(1, seq: 2, text: """
                Check with relay-cleanup before you collapse the errors \u{2014} it's in \
                that file too.
                """),
            read(2, seq: 3, path: "crates/amux-ui/src/pairing.rs"),
            search(3, seq: 4, query: "\"INVALID_PIN\"", grouped: true),
            toAgent(4, seq: 5, to: "relay-cleanup/mini", text: """
                I am about to collapse the three pairing error arms onto one string in \
                amux-ui. Are you holding anything that matches on the error name?
                """),
            read(5, seq: 6, path: "crates/amux-ui/tests/spec/pairing.rs"),
            message(6, seq: 7, text: markdown),
            fromAgent(7, seq: 8, from: "relay-cleanup/mini", text: """
                Nothing here matches on it \u{2014} I only construct them. Go ahead, and I \
                will rebase onto whatever you land.
                """),
            edit(8, seq: 9, path: "crates/amux-ui/src/pairing.rs", added: 9, removed: 14, lines: [
                "  let message = match status {",
                "-   Code::NotFound => \"no such host\",",
                "+   _ => \"Pairing failed. Check the code\",",
                "  };",
            ]),
            ran(9, seq: 10, command: "cargo check -p amux-ui", output: """
                Checking amux-ui v0.1.0
                Finished in 3.8s
                warning: unused import
                """),
            wrote(10, seq: 11, path: "crates/amux-ui/src/pairing_copy.rs", lines: 38),
            denied(11, seq: 12, command: "rm -rf target"),
            failed(12, seq: 13, command: "cargo test --workspace", message: "3 tests failed"),
            interrupted(13, seq: 14),
            providerError(14, seq: 15, message: "The provider was overloaded; it retried."),
            subagent(15, seq: 16, description: "spec-suite", kind: "general-purpose"),
            subagentFinished(16, seq: 17, text: "spec-suite updated three assertions"),
            unrecognized(17, seq: 18, label: "checkpoint"),
            fromAgent(18, seq: 19, from: "relay-cleanup/mini", text: """
                Moved it. The shared crate no longer exports the three constants, so \
                nothing downstream can name them.
                """),
            exited(19, seq: 20, agent: "relay-cleanup/mini"),
            turnEnd(20, seq: 21, milliseconds: 184_000),
        ]
    }

    /// One agent message written in every markdown construct the transcript
    /// promises to render, kept short because a screen is the only place that
    /// promise can be checked and the rest of the screen has rows to prove too.
    private static let markdown = """
        Asked **relay-cleanup** and started reading the tests. Nothing in `amux-ui` \
        matches on the name itself.

        ## What the arms become

        | Status | Copy |
        | --- | --- |
        | NotFound | Pairing failed |

        1. Collapse the match to one arm
        2. Fix the [spec tests](https://example.com/spec) that assert on it

        ```rust
        _ => "Pairing failed. Check the code and try again.",
        ```

        > The protocol refuses to tell them apart, and so must we.
        """

    /// A short Codex turn, in Codex's own row vocabulary.
    ///
    /// A Codex conversation is not a Claude conversation with the names
    /// changed: its rows arrive under different keys and its work is one kind
    /// of entry with a state on it. So the state a Codex approval is read in
    /// is built from Codex's rows rather than borrowing the other layer's.
    public static let codexTurn: [FeedEntry] = [
        codexRow(0, seq: 1, kind: .object([
            "entry": .string("prompt"),
            "content": .array([.object([
                "kind": .string("text"),
                "value": .string("Run the spec suite and tell me what breaks."),
            ])]),
        ])),
        codexRow(1, seq: 2, kind: .object([
            "entry": .string("reasoning"),
            "summary": .array([.string("Reading the suite's own runner first.")]),
            "finality": .string("final"),
        ])),
        codexRow(2, seq: 3, kind: .object([
            "entry": .string("work"),
            "kind": .object(["work": .string("command"), "command": .string("cargo check")]),
            "state": .object(["state": .string("completed")]),
            "exit_code": .int(0),
        ])),
        codexRow(3, seq: 4, kind: .object([
            "entry": .string("message"),
            "text": .string(
                "The workspace checks clean. Running the suite needs to leave the sandbox."),
            "finality": .string("final"),
        ])),
    ]

    private static func codexRow(_ id: Int, seq: Int, kind: JSONValue) -> FeedEntry {
        FeedEntry(layer: .codex, row: .object([
            "id": .int(id), "seq": .int(seq), "kind": kind,
        ]))
    }

    /// The changes a finished turn offers to show.
    public static let changes = DiffDocument(
        numbering: .absolute,
        hunks: [
            DiffHunk(oldStart: 118, newStart: 118, header: "@@ -118,14 +118,9 @@", lines: [
                "  let message = match status {",
                "-   Code::NotFound => \"no such host\",",
                "-   Code::Unauthenticated => \"wrong PIN\",",
                "-   Code::DeadlineExceeded => \"expired\",",
                "+   // One string for every failure: the protocol",
                "+   // refuses to tell them apart, and so must we.",
                "+   _ => \"Pairing failed. Check the code\",",
                "  };",
            ]),
            DiffHunk(oldStart: 4, newStart: 4, header: "@@ -4,6 +4,3 @@", lines: [
                "- pub const INVALID_PIN: &str = \"wrong PIN\";",
                "- pub const NO_SUCH_HOST: &str = \"no such host\";",
                "- pub const EXPIRED: &str = \"expired\";",
                "  pub const PAIRING_FAILED: &str =",
            ]),
        ],
        truncated: false)
}
