import AmuxCore

extension Transcript {
    /// The selected design's representative conversation, expressed as the
    /// same feed entries the production projection receives.
    public static let representativeTurn: [FeedEntry] = [
        prompt(
            20, seq: 1,
            text: "Collapse the pairing errors onto one string, and make sure nothing reads INVALID_PIN by name."),
        read(21, seq: 2, path: "crates/amux-ui/src/pairing.rs"),
        read(22, seq: 3, path: "crates/amux-ui/src/model.rs", grouped: true),
        search(23, seq: 4, query: "\"INVALID_PIN\"", grouped: true),
        read(24, seq: 5, path: "crates/amux/src/pairing/mod.rs", grouped: true),
        search(25, seq: 6, query: "\"Code::Unauthenticated\"", grouped: true),
        read(26, seq: 7, path: "crates/amux-ui/tests/spec/pairing.rs", grouped: true),
        edit(
            27, seq: 8, path: "crates/amux-ui/src/pairing.rs", added: 9, removed: 14,
            lines: [
                "  let message = match status {",
                "-   Code::NotFound => \"no such host\",",
                "+   _ => \"Pairing failed. Check the code\",",
                "  };",
            ]),
        ran(
            28, seq: 9, command: "cargo check -p amux-ui",
            output: "error[E0308]: mismatched types", truncated: true,
            meta: "4.2s", hidden: 214),
        read(29, seq: 10, path: "crates/amux-ui/src/effect.rs"),
        edit(
            30, seq: 11, path: "crates/amux-ui/src/effect.rs", added: 2, removed: 2,
            lines: [
                "- case .invalidPin: return oldMessage",
                "+ case .pairingFailed: return message",
            ]),
        ran(31, seq: 12, command: "cargo check -p amux-ui", output: "", meta: "3.8s"),
        wrote(32, seq: 13, path: "crates/amux-ui/tests/spec/pairing_copy.rs", lines: 38),
        denied(33, seq: 14, command: "rm -rf target", kind: "permission_denied"),
        message(34, seq: 15, text: """
            Done. The three status arms are one arm now, and the new test asserts on the \
            single string rather than on which one it was.
            """),
    ]

    /// The plan source deliberately keeps only the opening work behind the
    /// decision panel, so the proposed work remains legible in context.
    public static let representativePlanContext: [FeedEntry] = [
        prompt(
            40, seq: 1,
            text: "Collapse the pairing errors onto one string, and make sure nothing reads INVALID_PIN by name."),
        read(41, seq: 2, path: "crates/amux-ui/src/pairing.rs"),
        search(42, seq: 3, query: "\"INVALID_PIN\"", grouped: true),
        read(43, seq: 4, path: "crates/amux/src/pairing/mod.rs", grouped: true),
        edit(
            44, seq: 5, path: "crates/amux-ui/src/pairing.rs", added: 9, removed: 14,
            lines: [
                "  let message = match status {",
                "-   Code::NotFound => \"no such host\",",
                "+   _ => \"Pairing failed. Check the code\",",
                "  };",
            ]),
        ran(45, seq: 6, command: "cargo check -p amux-ui", output: "", meta: "4.2s"),
    ]
}
