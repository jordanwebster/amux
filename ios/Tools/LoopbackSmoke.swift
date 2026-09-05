import AmuxMobile
import Foundation

// Rust owns callback delivery. The main thread waits for a complete observed
// host set; all shared state is protected by this condition.
private final class Observation: @unchecked Sendable {
    let condition = NSCondition()
    let expected: Set<String>
    var hosts: [String: String] = [:]
    var connected = false
    var failure: String?

    init(expected: Set<String>) { self.expected = expected }

    func receive(_ bytes: UnsafePointer<CChar>) {
        condition.lock()
        defer { condition.broadcast(); condition.unlock() }
        do {
            let events = try JSONSerialization.jsonObject(with: Data(String(cString: bytes).utf8)) as! [[String: Any]]
            for event in events {
                if let invariant = event["Invariant"] { failure = "Bridge invariant: \(invariant)" }
                if let connection = event["Connection"] as? [String: Any] {
                    connected = connection["state"] as? String == "connected"
                }
                if let fleet = event["Fleet"] as? [String: Any], let rows = fleet["hosts"] as? [[String: Any]] {
                    hosts.removeAll()
                    for row in rows {
                        guard let entry = row["entry"] as? [String: Any],
                              let id = entry["id"] as? String, expected.contains(id),
                              entry["online"] as? Bool == true,
                              let name = entry["name"] as? String else { continue }
                        hosts[id] = name
                    }
                }
            }
        } catch { failure = "Invalid callback JSON: \(error)" }
    }

    func wait() throws -> [String: String] {
        condition.lock()
        defer { condition.unlock() }
        let deadline = Date().addingTimeInterval(30)
        while failure == nil && !(connected && Set(hosts.keys) == expected) {
            if !condition.wait(until: deadline) { break }
        }
        guard failure == nil, connected, !hosts.isEmpty, Set(hosts.keys) == expected else {
            throw NSError(domain: "LoopbackSmoke", code: 1, userInfo: [NSLocalizedDescriptionKey:
                failure ?? "Relay did not deliver all daemon identities within 30 seconds: \(hosts)"])
        }
        return hosts
    }
}

private func receive(_ bytes: UnsafePointer<CChar>?, _ context: UnsafeMutableRawPointer?) {
    guard let bytes, let context else { return }
    Unmanaged<Observation>.fromOpaque(context).takeUnretainedValue().receive(bytes)
}

private func smoke() throws {
    let arguments = CommandLine.arguments
    guard arguments.count >= 4 else {
        throw NSError(domain: "LoopbackSmoke", code: 2, userInfo: [NSLocalizedDescriptionKey: "Expected relay, token and daemon UUIDs"])
    }
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("amux-loopback-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: root) }
    let config: [String: Any] = [
        "data_dir": root.appendingPathComponent("data").path,
        "cache_dir": root.appendingPathComponent("cache").path,
        "log_path": root.appendingPathComponent("amux.log").path,
        "device_name": "simulator-loopback",
        "relay": ["url": "http://\(arguments[1])", "tls": "PlainLoopback", "token": ["Static": arguments[2]]]
    ]
    let json = String(decoding: try JSONSerialization.data(withJSONObject: config), as: UTF8.self)
    let observation = Observation(expected: Set(arguments.dropFirst(3)))
    let context = Unmanaged.passRetained(observation)
    defer { context.release() }
    guard let handle = json.withCString({ amux_mobile_start($0, receive, context.toOpaque()) }) else {
        throw NSError(domain: "LoopbackSmoke", code: 3, userInfo: [NSLocalizedDescriptionKey: "Bridge rejected loopback configuration"])
    }
    defer { amux_mobile_stop(handle) }
    let hosts = try observation.wait()
    let output = String(decoding: try JSONSerialization.data(withJSONObject: hosts, options: [.sortedKeys]), as: UTF8.self)
    print("daemon_names=\(output)")
}

do {
    try smoke()
    print("mobile worker stopped")
} catch {
    FileHandle.standardError.write(Data("\(error.localizedDescription)\n".utf8))
    exit(1)
}
