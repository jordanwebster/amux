import AmuxMobile
import Foundation

// Rust owns callback delivery. Snapshots run on the main thread without holding
// this condition, so a worker callback cannot deadlock a snapshot request.
private final class Observation: @unchecked Sendable {
    let condition = NSCondition()
    let expected: Set<String>
    var connected = false
    var reconciled = false
    var connectionDescription = "no connection callback"
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
                    connectionDescription = String(describing: connection)
                }
                if let fleet = event["Fleet"] as? [String: Any], let rows = fleet["hosts"] as? [[String: Any]] {
                    for row in rows {
                        guard let entry = row["entry"] as? [String: Any], let id = entry["id"] as? String else {
                            failure = "Invalid Fleet host: \(row)"
                            continue
                        }
                        if expected.contains(id) || entry["trust_status"] as? String != "trusted" {
                            failure = "Unpaired relay host appeared in Fleet: \(entry)"
                        }
                    }
                    reconciled = fleet["reconciled"] as? Bool == true
                }
            }
        } catch { failure = "Invalid callback JSON: \(error)" }
    }

    func wait(handle: OpaquePointer) throws -> [String: String] {
        let deadline = Date().addingTimeInterval(30)
        var hosts: [String: String] = [:]
        while Date() < deadline {
            // Discovery includes online peers before pairing; the displayed
            // Fleet intentionally includes only trusted hosts. Polling here is
            // bounded test observation, not an application refresh loop.
            hosts = try discoveredHosts(handle: handle)
            condition.lock()
            let complete = connected && reconciled && !hosts.isEmpty && Set(hosts.keys) == expected
            let failure = failure
            if failure == nil && !complete {
                _ = condition.wait(until: min(deadline, Date().addingTimeInterval(0.02)))
            }
            condition.unlock()
            if let failure {
                throw NSError(domain: "LoopbackSmoke", code: 1, userInfo: [NSLocalizedDescriptionKey: failure])
            }
            if complete { return hosts }
        }
        condition.lock()
        defer { condition.unlock() }
        throw NSError(domain: "LoopbackSmoke", code: 1, userInfo: [NSLocalizedDescriptionKey:
            failure ?? "Relay did not deliver all daemon identities within 30 seconds: \(hosts); \(connectionDescription); Fleet reconciled=\(reconciled)"])
    }

    private func discoveredHosts(handle: OpaquePointer) throws -> [String: String] {
        guard let bytes = amux_mobile_snapshot(handle) else {
            throw NSError(domain: "LoopbackSmoke", code: 4, userInfo: [NSLocalizedDescriptionKey: "Bridge snapshot unavailable"])
        }
        defer { amux_mobile_free(bytes) }
        let model = try JSONSerialization.jsonObject(with: Data(String(cString: bytes).utf8)) as! [String: Any]
        let rows = model["hosts"] as! [String: [String: Any]]
        var hosts: [String: String] = [:]
        for row in rows.values {
            guard let entry = row["entry"] as? [String: Any],
                  let id = entry["id"] as? String, expected.contains(id),
                  entry["online"] as? Bool == true,
                  let name = entry["name"] as? String else { continue }
            guard entry["trust_status"] as? String == "untrusted_but_online" else {
                throw NSError(domain: "LoopbackSmoke", code: 5, userInfo: [NSLocalizedDescriptionKey: "Discovery unexpectedly granted trust to \(id)"])
            }
            hosts[id] = name
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
    let hosts = try observation.wait(handle: handle)
    let output = String(decoding: try JSONSerialization.data(withJSONObject: hosts, options: [.sortedKeys]), as: UTF8.self)
    print("daemon_names=\(output)")
    print("unpaired relay hosts excluded from Fleet; discovery verified through snapshot")
}

do {
    try smoke()
    print("mobile worker stopped")
} catch {
    FileHandle.standardError.write(Data("\(error.localizedDescription)\n".utf8))
    exit(1)
}
