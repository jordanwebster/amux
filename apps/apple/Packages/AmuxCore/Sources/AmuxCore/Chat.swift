import AmuxApp
import AmuxValues
import Foundation

/// One open chat on the shared runtime.
///
/// Rows are handed out by item key, which never moves: the owner holds a
/// sequence of keys that only grows at its two edges, fetches the rows an
/// update changed, and reads the keys again only when a change batch says
/// the sequence was reloaded. Close it before the runtime stops.
public final class Chat: ChatSource, @unchecked Sendable {
    private let handle: OpaquePointer
    /// Held across every call into the library, so stopping waits for the
    /// calls in flight and none starts after it. Recursive, because a call
    /// may be made from inside another on the same thread.
    private let lock = NSRecursiveLock()
    private var open = true
    /// What the owner's wake names this chat by.
    public let id: UInt64

    init(handle: OpaquePointer) {
        self.handle = handle
        self.id = amux_session_id(handle)
    }

    public func close() {
        let wasOpen = lock.withLock {
            defer { open = false }
            return open
        }
        guard wasOpen else { return }
        amux_session_close(handle)
    }

    deinit { close() }

    /// Runs a call on the open chat, or answers `closed` once it is not.
    private func call<T>(_ closed: T, _ body: (OpaquePointer) -> T) -> T {
        lock.withLock { open ? body(handle) : closed }
    }

    public func keys() -> [String] {
        call([]) { live in
            Bridge.read([String].self, amux_session_keys(live)) ?? []
        }
    }

    /// Keys newer than `newest`, or nil when it is no longer held.
    public func keys(above newest: String) -> [String]? {
        call(nil) { live in
            newest.withCString {
                Bridge.read([String]?.self, amux_session_new_keys_above(live, $0))
            } ?? nil
        }
    }

    /// Keys older than `oldest`, or nil when it is no longer held.
    public func keys(below oldest: String) -> [String]? {
        call(nil) { live in
            oldest.withCString {
                Bridge.read([String]?.self, amux_session_new_keys_below(live, $0))
            } ?? nil
        }
    }

    public func rows(for keys: [String], options: RowOptions? = nil) -> [Row] {
        call([]) { live in
            Bridge.json(keys).withCString { keys in
                if let options {
                    return Bridge.json(options).withCString {
                        Bridge.read([Row].self, amux_session_rows_for(live, keys, $0))
                    }
                }
                return Bridge.read([Row].self, amux_session_rows_for(live, keys, nil))
            } ?? []
        }
    }

    public func askCard() -> AskCard? {
        call(nil) { live in
            Bridge.read(AskCard?.self, amux_session_ask_card(live)) ?? nil
        }
    }

    public func strip() -> Strip? {
        call(nil) { live in
            Bridge.read(Strip.self, amux_session_strip(live))
        }
    }

    public func settings() -> SettingsView? {
        call(nil) { live in
            Bridge.read(SettingsView.self, amux_session_settings(live))
        }
    }

    public func frame() -> ChatFrame? {
        call(nil) { live in
            Bridge.read(ChatFrame.self, amux_session_frame(live))
        }
    }

    /// What changed since the last take; the next change wakes the owner.
    public func takeChanges() -> ChatChanges {
        let none = ChatChanges(keys: [], reloaded: false, session: false)
        return call(none) { live in
            Bridge.read(ChatChanges.self, amux_session_take_changes(live)) ?? none
        }
    }

    // MARK: - Acts

    public func send(_ draft: Draft) async -> Result<SendOutcome, RuntimeFailure> {
        await act(SendOutcome.self) { live, callback, context in
            Bridge.json(draft).withCString { amux_session_send(live, $0, callback, context) }
        }
    }

    /// Answers the head ask by the position of a choice on its card.
    public func answer(
        _ ask: String, choice: Int, note: String? = nil
    ) async -> ActOutcome? {
        await value(ActOutcome.self) { live, callback, context in
            ask.withCString { ask in
                if let note {
                    note.withCString {
                        amux_session_answer(live, ask, UInt32(choice), $0, callback, context)
                    }
                } else {
                    amux_session_answer(live, ask, UInt32(choice), nil, callback, context)
                }
            }
        }
    }

    /// Answers a question card with one pick per question.
    public func answer(_ ask: String, picks: [Pick], note: String? = nil) async -> ActOutcome? {
        await value(ActOutcome.self) { live, callback, context in
            ask.withCString { ask in
                Bridge.json(picks).withCString { picks in
                    if let note {
                        note.withCString {
                            amux_session_answer_questions(live, ask, picks, $0, callback, context)
                        }
                    } else {
                        amux_session_answer_questions(live, ask, picks, nil, callback, context)
                    }
                }
            }
        }
    }

    /// Submits a form ask by the position of its Submit on the card, with
    /// the person's field values as a JSON object.
    public func answerForm(_ ask: String, choice: Int, content: String) async -> ActOutcome? {
        await value(ActOutcome.self) { live, callback, context in
            ask.withCString { ask in
                content.withCString {
                    amux_session_answer_form(live, ask, UInt32(choice), $0, callback, context)
                }
            }
        }
    }

    public func withdraw(_ input: [UInt8]) async -> ActOutcome? {
        await value(ActOutcome.self) { live, callback, context in
            Bridge.json(input).withCString { amux_session_withdraw(live, $0, callback, context) }
        }
    }

    public func sendNow(_ input: [UInt8]) async -> ActOutcome? {
        await value(ActOutcome.self) { live, callback, context in
            Bridge.json(input).withCString { amux_session_send_now(live, $0, callback, context) }
        }
    }

    public func resend(_ input: [UInt8]) async -> SendOutcome? {
        await value(SendOutcome?.self) { live, callback, context in
            Bridge.json(input).withCString { amux_session_resend(live, $0, callback, context) }
        } ?? nil
    }

    public func discard(_ input: [UInt8]) {
        call(()) { live in Bridge.json(input).withCString { amux_session_discard(live, $0) } }
    }

    public func interrupt() async -> ActOutcome? {
        await value(ActOutcome.self) { live, callback, context in
            amux_session_interrupt(live, callback, context)
        }
    }

    /// Sends a pick from the settings view in the agent's kind.
    public func change(_ setting: SettingChange) async -> ActOutcome? {
        await value(ActOutcome.self) { live, callback, context in
            Bridge.json(setting).withCString { amux_session_change_setting(live, $0, callback, context) }
        }
    }

    /// Starts an exited agent again with the composer's draft.
    public func resume(with draft: Draft) async -> ActOutcome? {
        await value(ActOutcome.self) { live, callback, context in
            Bridge.json(draft).withCString { amux_session_resume(live, $0, callback, context) }
        }
    }

    public func pageOlder(_ rows: UInt32) async -> PageOutcome? {
        await value(PageOutcome.self) { live, callback, context in
            amux_session_page_older(live, rows, callback, context)
        }
    }

    /// The agent's working-tree diff as its host froze it, with its patch.
    public func review() async -> Result<FrozenReview, RuntimeFailure> {
        await act(FrozenReview.self) { live, callback, context in
            amux_session_review(live, callback, context)
        }
    }

    /// Stores an attachment's bytes, answering the reference a draft carries.
    public func putBlob(
        _ data: Data, name: String, mime: String
    ) async -> Result<BlobRef, RuntimeFailure> {
        await act(BlobRef.self) { live, callback, context in
            data.withUnsafeBytes { bytes in
                name.withCString { name in
                    mime.withCString { mime in
                        amux_session_put_blob(
                            live, bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count,
                            name, mime, callback, context)
                    }
                }
            }
        }
    }

    /// An attachment's bytes, where this phone holds them.
    public func blob(_ hash: [UInt8]) -> Data? {
        let bytes = call(AmuxBytes(data: nil, len: 0)) { live in
            Bridge.json(hash).withCString { amux_session_blob(live, $0) }
        }
        guard let data = bytes.data else { return nil }
        defer { amux_bytes_free(bytes) }
        return Data(bytes: data, count: bytes.len)
    }

    private func act<T: Decodable & Sendable>(
        _ type: T.Type,
        _ body: @escaping (OpaquePointer, AmuxCallback, UnsafeMutableRawPointer) -> Void
    ) async -> Result<T, RuntimeFailure> {
        await Bridge.answer(T.self) { callback, context in
            let ran = self.call(false) { live in
                body(live, callback, context)
                return true
            }
            if !ran { Bridge.stopped(callback, context) }
        }
    }

    private func value<T: Decodable & Sendable>(
        _ type: T.Type,
        _ body: @escaping (OpaquePointer, AmuxCallback, UnsafeMutableRawPointer) -> Void
    ) async -> T? {
        await Bridge.value(T.self) { callback, context in
            let ran = self.call(false) { live in
                body(live, callback, context)
                return true
            }
            if !ran { Bridge.stopped(callback, context) }
        }
    }
}
