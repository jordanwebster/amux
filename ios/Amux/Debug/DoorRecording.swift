import AmuxCore
import AmuxMobile
import Foundation

/// Writing what this app has been through into a bundle, and rebuilding a
/// screen from one somebody else wrote.
///
/// A bug on a phone is reported as two recordings side by side in one
/// directory. `msgs.jsonl` is the shared runtime's own: the reducer model it
/// had checkpointed and every message it folded after that. `trace.jsonl` is
/// what the person was looking at while those messages arrived. Rebuilding
/// means folding the first back into a model, projecting that model into the
/// same events a live connection would have delivered, and then applying the
/// second — so the screen comes back without the phone, the relay, the host
/// or any of the work the recording originally asked for.
///
/// This is a driving tool. The calls it makes exist only in the library built
/// with the driving tools compiled in, so it must never be reachable from a
/// build a person could install.
enum DoorRecording {
    enum Failure: Error, CustomStringConvertible {
        case notRunning
        case unreadable(String)
        case refused(String)

        var description: String {
            switch self {
            case .notRunning: "nothing is connected, so there is no recording to write"
            case .unreadable(let what): "the bundle's \(what) could not be read"
            case .refused(let why): "the recording could not be replayed: \(why)"
            }
        }
    }

    /// Writes the runtime's recording and the view-state trace into a
    /// directory, and answers the files it left there.
    ///
    /// The runtime hands its recording back as a checkpoint and a list of
    /// message lines rather than as a file; the header line and the lines
    /// under it are assembled here, in the shape `replay` reads back.
    static func write(
        _ directory: URL, runtime: BridgeClient, trace: [TraceEvent]
    ) throws -> [String] {
        guard let json = runtime.withRuntime({ handle -> String? in
            guard let owned = amux_mobile_report_snapshot(handle) else { return nil }
            defer { amux_mobile_free(owned) }
            return String(cString: owned)
        }) ?? nil else { throw Failure.notRunning }
        guard
            let snapshot = try? JSONSerialization.jsonObject(with: Data(json.utf8)),
            let report = snapshot as? [String: Any],
            let recording = report["msgs"] as? [String: Any],
            let version = recording["format_version"],
            let checkpoint = recording["checkpoint"],
            let messages = recording["msgs"] as? [String]
        else { throw Failure.unreadable(Trace.messagesFile) }

        try FileManager.default.createDirectory(
            at: directory, withIntermediateDirectories: true)
        let header = try JSONSerialization.data(
            withJSONObject: ["format_version": version, "checkpoint": checkpoint])
        var lines = [String(decoding: header, as: UTF8.self)]
        lines.append(contentsOf: messages)
        try (lines.joined(separator: "\n") + "\n").write(
            to: directory.appendingPathComponent(Trace.messagesFile),
            atomically: true, encoding: .utf8)
        try Trace.lines(trace).write(
            to: directory.appendingPathComponent(Trace.traceFile),
            atomically: true, encoding: .utf8)
        var parts = [Trace.messagesFile, Trace.traceFile]
        // The embedded daemon's own dump, when it answered. Its absence is
        // recorded by the runtime rather than hidden, and a bundle without it
        // still replays: the screen comes from the two recordings above.
        if let daemon = report["daemon"] as? String {
            try daemon.write(
                to: directory.appendingPathComponent("daemon.json"),
                atomically: true, encoding: .utf8)
            parts.append("daemon.json")
        }
        return parts
    }

    /// Folds a bundle's runtime recording into fresh stores.
    ///
    /// Nothing is started and nothing is sent: the shared reducer folds the
    /// recorded messages and its projection is read, which is the same read
    /// surface a live connection delivers. The effects the recording once
    /// asked for are not carried out — they were carried out on the phone
    /// that wrote it.
    @MainActor
    static func replay(_ directory: URL, into stores: StoreBundle) throws -> [Event] {
        let messages = directory.appendingPathComponent(Trace.messagesFile)
        let json = messages.path.withCString { path -> String? in
            guard let owned = amux_mobile_replay_report(path) else { return nil }
            defer { amux_mobile_free(owned) }
            return String(cString: owned)
        }
        guard let json else { throw Failure.unreadable(Trace.messagesFile) }
        struct Replayed: Decodable {
            var events: [Event]?
            var error: String?
        }
        guard let replayed = try? AmuxJSON.decoder.decode(Replayed.self, from: Data(json.utf8))
        else { throw Failure.unreadable(Trace.messagesFile) }
        if let error = replayed.error { throw Failure.refused(error) }
        guard let events = replayed.events else { throw Failure.unreadable(Trace.messagesFile) }
        stores.apply(events)
        return events
    }

    /// The view-state recording beside it, or nothing when the bundle has none.
    static func trace(_ directory: URL) throws -> [TraceEvent] {
        let path = directory.appendingPathComponent(Trace.traceFile)
        guard let text = try? String(contentsOf: path, encoding: .utf8) else { return [] }
        guard let events = try? Trace.events(text) else { throw Failure.unreadable(Trace.traceFile) }
        return events
    }
}
