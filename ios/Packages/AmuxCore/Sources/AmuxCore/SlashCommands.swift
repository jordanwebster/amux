import Foundation

/// What the composer offers while somebody is typing a command.
///
/// A command is not an attachment and not a word: it is what the *message is*,
/// and the core takes it as its own segment of the draft, first and alone. So
/// the offer only stands while the thing being typed could still become that
/// segment — from the very start of the draft, with nothing before it and no
/// space yet after it. A slash in the middle of a sentence is a slash in the
/// middle of a sentence.
public struct SlashCommands: Sendable, Equatable {
    /// How many are raised at once. Five is what the design draws, and it is a
    /// number rather than "as many as fit" because the rows sit over the
    /// conversation you are writing about: a list long enough to scroll would
    /// take the screen to save a few keystrokes.
    public static let most = 5

    /// What has been typed after the slash. It is what the rows are filtered
    /// by and it is what the picked command replaces.
    public let typed: String
    /// The commands still matching, in the order the session reported them,
    /// capped at `most`.
    public let rows: [ProviderCommand]

    /// The rows to raise for a draft, or nothing.
    ///
    /// Nothing is offered where the layer cannot dispatch a command at all. A
    /// Claude session driven over a PTY is one such: the core refuses a
    /// command token on it outright, and offering a menu of things that will
    /// be refused is worse than offering none.
    public static func offered(
        for draft: MessageDraft, facts: SessionFacts, provider: ProviderFacts
    ) -> SlashCommands? {
        guard facts.takesCommands, let typed = draft.typedCommand else { return nil }
        // Terminal-only commands are dropped rather than shown and refused:
        // they are the ones that do something to a terminal, and this is not
        // one. The core refuses them for the same reason.
        let matching = provider.commands
            .filter { !$0.terminalOnly && $0.matches(typed) }
            .prefix(most)
        guard !matching.isEmpty else { return nil }
        return SlashCommands(typed: typed, rows: Array(matching))
    }
}

extension SessionFacts {
    /// Whether this layer takes a command as a command.
    ///
    /// Codex and the Claude SDK do. A Claude session driven over a PTY does
    /// not — the core answers "provider commands are unavailable for this
    /// agent" — and a session whose layer is unknown is not guessed at.
    public var takesCommands: Bool {
        switch self {
        case .codex: true
        case .claudeSdk: true
        case .claudePty, .unavailable: false
        }
    }
}

extension ProviderCommand {
    /// Whether what has been typed is the start of this command's name.
    ///
    /// A plugin's command is named for its plugin first — `stripe:connect-`
    /// `recommend` — and nobody typing `/co` is thinking of the plugin. So the
    /// name after the namespace counts as a start of its own; anywhere else
    /// inside a word does not, because a list that matched the middle of every
    /// name would rank a command by nothing the typist can see.
    func matches(_ typed: String) -> Bool {
        if name.hasPrefix(typed) { return true }
        guard let colon = name.lastIndex(of: ":") else { return false }
        return name[name.index(after: colon)...].hasPrefix(typed)
    }

    /// Where this command comes from, in words.
    ///
    /// Two sessions can both offer `/compact` and mean different things by it,
    /// and one of them can come from a plugin somebody installed. The row says
    /// which, because picking the wrong one is not a mistake the app can
    /// undo for you.
    public var origin: String {
        if let word = source.stringValue { return word.capitalizedFirst }
        if case .object(let fields) = source,
           let plugin = fields["plugin"]?.stringValue {
            return plugin
        }
        return ""
    }
}

extension MessageDraft {
    /// The command being typed at the front of the draft, without its slash.
    ///
    /// Only from the very start, and only while it is still one word: the
    /// space that ends the word is where the command stops being typed and
    /// its arguments begin, and by then it has either been picked or it is
    /// prose.
    public var typedCommand: String? {
        guard command == nil, body.hasPrefix("/") else { return nil }
        let typed = body.dropFirst().prefix { !$0.isWhitespace }
        guard typed.count == body.count - 1 else { return nil }
        return String(typed)
    }

    /// Takes the typed command and puts the command itself in its place.
    ///
    /// What was typed goes; a token stands where it was; and the caret lands
    /// after it, so what is written next is the command's arguments. One
    /// backspace from there takes the whole command, the way one backspace
    /// takes any other token.
    public mutating func pick(_ command: ProviderCommand) {
        guard let typed = typedCommand else { return }
        body = String(body.dropFirst(typed.count + 1))
        place(caret: 0)
        insert(DraftToken(
            kind: .command, label: command.name, element: "", attachment: nil))
    }
}
