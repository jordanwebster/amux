import AmuxApp
import AmuxCore
import Foundation

/// Writes store-backed states through the bridge's seeding entry points and
/// reads them back the way a launch does.
///
/// Debug builds only: the entry points exist in the driving bridge alone.
@MainActor
enum RememberedStoreBridge {
    /// Where store-backed states keep their stores, apart from the app's own.
    static let cache = FileManager.default.temporaryDirectory
        .appendingPathComponent("remembered-stores", isDirectory: true)

    static func install() {
        RememberedStores.read = { remembered, chat in read(remembered, chat: chat) }
    }

    static func read(_ remembered: Remembered, chat: AgentId?) -> [Event] {
        let account = RememberedStores.account
        guard seed(remembered, in: cache, for: account) else { return [] }
        guard var events = try? Bridge.cachedFleet(in: cache, for: account) else { return [] }
        if let chat,
           let opened = call({ amux_app_cached_chat(cache.path, account.value, chat.description) }) {
            // The fleet a launch draws is the cached one above; the
            // conversation contributes only what it projects about itself.
            events += opened.filter { if case .fleet = $0 { false } else { true } }
        }
        return events
    }

    /// Replaces the account's store under `cache` with one remembering
    /// `remembered`, answering whether it was written.
    static func seed(_ remembered: Remembered, in cache: URL, for account: AccountId) -> Bool {
        guard let json = try? remembered.json() else { return false }
        let text = String(decoding: json, as: UTF8.self)
        return call({ amux_app_seed_store(cache.path, account.value, text) }) != nil
    }

    /// One call returning `{"events":[…]}`, `{"ok":true}` or `{"error":"…"}`:
    /// the events, an empty list for success without any, or nothing.
    private static func call(_ body: () -> UnsafeMutablePointer<CChar>?) -> [Event]? {
        guard let owned = body() else { return nil }
        defer { amux_app_free(owned) }
        struct Reply: Decodable {
            var events: [Event]?
            var ok: Bool?
            var error: String?
        }
        guard let reply = try? AmuxJSON.decoder.decode(
            Reply.self, from: Data(String(cString: owned).utf8)),
            reply.error == nil
        else { return nil }
        return reply.events ?? []
    }
}
