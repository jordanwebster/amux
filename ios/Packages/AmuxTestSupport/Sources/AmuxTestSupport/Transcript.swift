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

    /// A message another amux agent wrote into this one's transcript.
    public static func fromAgent(_ id: Int, seq: Int, from: String, text: String) -> FeedEntry {
        row(id, seq, .object([
            "entry": .string("agent_message"),
            "id": .string("envelope-\(id)"),
            "context": .null,
            "from": .string(from),
            "kind": .string("message"),
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

    /// The same conversation with the turn still open.
    public static var live: [FeedEntry] {
        Array(pairingCopy.dropLast()) + [message(7, seq: 8, text: "Running the spec suite now", final: false)]
    }

    /// Every row kind at once, including the ones this build cannot read.
    public static var everyKind: [FeedEntry] {
        pairingCopy + [
            denied(8, seq: 9, command: "rm -rf target"),
            failed(9, seq: 10, command: "cargo test --workspace", message: "3 tests failed"),
            fromAgent(10, seq: 11, from: "spec-fixer/studio", text: "The three spec tests are updated."),
            interrupted(11, seq: 12),
            unrecognized(12, seq: 13, label: "checkpoint"),
            turnEnd(13, seq: 14, milliseconds: 184_000),
        ]
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
