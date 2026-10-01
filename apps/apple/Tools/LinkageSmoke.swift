import AmuxApp
import Foundation

// A bare Swift executable linking the shipping bridge, run on the simulator
// to prove the packaged framework links, starts and is the shipping build:
// its version carries no driving marker, an installation starts from it,
// and the entry a driving build alone answers is refused.

private func fail(_ code: Int, _ message: String) -> NSError {
    NSError(domain: "LinkageSmoke", code: code, userInfo: [NSLocalizedDescriptionKey: message])
}

/// One callback's answer, waited for on a condition.
private final class Answer: @unchecked Sendable {
    let condition = NSCondition()
    var json: String?

    func wait(_ what: String) throws -> Any {
        condition.lock()
        defer { condition.unlock() }
        let deadline = Date().addingTimeInterval(30)
        while json == nil {
            guard condition.wait(until: deadline) else { throw fail(6, "no answer to \(what) within 30 seconds") }
        }
        return try JSONSerialization.jsonObject(with: Data(json!.utf8), options: [.fragmentsAllowed])
    }
}

/// The runtime wakes its host on every change; this smoke reads snapshots
/// instead, so a wake has nothing to do.
private func woke(_ context: UnsafeMutableRawPointer?, _ chat: UInt64) {}

private func answered(_ context: UnsafeMutableRawPointer?, _ json: UnsafePointer<CChar>?) {
    guard let context, let json else { return }
    let answer = Unmanaged<Answer>.fromOpaque(context).takeRetainedValue()
    answer.condition.lock()
    answer.json = String(cString: json)
    answer.condition.broadcast()
    answer.condition.unlock()
}

private func read(_ bytes: UnsafeMutablePointer<CChar>?) throws -> Any {
    guard let bytes else { throw fail(4, "the bridge answered nothing") }
    defer { amux_string_free(bytes) }
    return try JSONSerialization.jsonObject(with: Data(String(cString: bytes).utf8), options: [.fragmentsAllowed])
}

private func json(_ value: Any) -> String {
    String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.fragmentsAllowed, .sortedKeys]), as: UTF8.self)
}

private func smoke() throws {
    let version = String(cString: amux_version())
    guard !version.isEmpty else { throw fail(1, "the bridge has no version") }
    guard !version.contains("+debug-tools") else { throw fail(1, "the packaged bridge is a driving build: \(version)") }
    print("amux_version=\(version)")

    let root = FileManager.default.temporaryDirectory.appendingPathComponent("amux-linkage-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: root) }
    // Nothing listens: the smoke proves the library, not the network.
    let config: [String: Any] = [
        "data_dir": root.appendingPathComponent("data").path,
        "log_path": root.appendingPathComponent("amux.log").path,
        "device_name": "simulator-linkage",
        "lan": false,
    ]
    var error: UnsafeMutablePointer<CChar>?
    guard let runtime = json(config).withCString({ amux_runtime_start($0, woke, nil, &error) }) else {
        let reason = error.map { String(cString: $0) } ?? "no reason"
        if let error { amux_string_free(error) }
        throw fail(3, "the runtime did not start: \(reason)")
    }
    defer { amux_runtime_stop(runtime) }
    let listed = try read(amux_runtime_profiles(runtime)) as? [[String: Any]] ?? []
    guard let id = listed.first?["id"] as? String else { throw fail(3, "the installation lists no profile") }
    print("installation started by the shipping mobile library")

    // Only a driving build offers a pairing code of its own.
    let answer = Answer()
    id.withCString { amux_runtime_offer_pairing(runtime, $0, answered, Unmanaged.passRetained(answer).toOpaque()) }
    let reply = try answer.wait("offering pairing")
    guard let result = reply as? [String: Any], result["Err"] != nil else {
        throw fail(5, "the shipping bridge offered pairing: \(reply)")
    }
    print("pairing offer refused by the shipping mobile library")
}

do {
    try smoke()
} catch {
    FileHandle.standardError.write(Data("\(error.localizedDescription)\n".utf8))
    exit(Int32((error as NSError).code))
}
