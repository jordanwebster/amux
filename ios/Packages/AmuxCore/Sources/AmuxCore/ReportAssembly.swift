import Foundation

/// Turns a frozen capture and what somebody wrote about it into the bundle the
/// daemon tooling reads and the cloud stores.
///
/// The layout is the one `amux debug report` already writes and replays:
/// `report.json` names every part as present or absent-with-a-reason, and the
/// payloads sit beside it under the names it uses. Nothing here invents a
/// second shape for the phone — a report written on a phone is opened, shown
/// and replayed by exactly the commands that open one written in a terminal.
public enum ReportAssembly {
    /// The layout version this app writes. It is the version the daemon
    /// tooling and the account service both read; a bundle at any other
    /// version is refused rather than guessed at.
    public static let schemaVersion = 2

    public static let reportFile = "report.json"
    public static let frameFile = "frame.png"
    public static let traceFile = "trace.jsonl"
    public static let messagesFile = "msgs.jsonl"
    public static let daemonFile = "daemon.json"
    public static let logFile = "log.txt"

    /// Everything a report is, ready to go.
    ///
    /// `report.json` is built last and first in the list, because what it says
    /// about the other parts has to be what the other parts actually are: the
    /// account service refuses a bundle whose declarations and whose files
    /// disagree, and it is right to.
    public static func bundle(
        from capture: ReportCapture,
        draft: ReportDraft,
        build: String,
        gitSHA: String = "",
        createdAt: Date = Date(),
        log: Result<String, PartAbsent>
    ) -> ReportBundle {
        let recording = RuntimeRecording.split(capture.snapshot)
        var parts: [ReportPart] = []

        parts.append(ReportPart(name: frameFile, data: capture.frame.png))
        parts.append(part(
            traceFile, capture.trace.map { Data($0.utf8) },
            absent: capture.traceAbsent ?? "the view-state recording was not captured"))
        parts.append(part(
            messagesFile, recording.messages.map { Data($0.utf8) },
            absent: capture.snapshotAbsent ?? recording.messagesAbsent
                ?? "the runtime recording was not captured"))
        parts.append(part(
            daemonFile, recording.daemon.map { Data($0.utf8) },
            absent: capture.snapshotAbsent ?? recording.daemonAbsent
                ?? "the embedded daemon did not answer"))
        switch log {
        case .success(let text): parts.append(ReportPart(name: logFile, data: Data(text.utf8)))
        case .failure(let absent):
            parts.append(ReportPart(name: logFile, absenceReason: absent.why))
        }

        let header = self.header(
            capture: capture, draft: draft, build: build, gitSHA: gitSHA,
            createdAt: createdAt, parts: parts)
        return ReportBundle(parts: [ReportPart(name: reportFile, data: header)] + parts)
    }

    private static func part(_ name: String, _ data: Data?, absent: String) -> ReportPart {
        data.map { ReportPart(name: name, data: $0) }
            ?? ReportPart(name: name, absenceReason: absent)
    }

    /// `report.json`: the small, self-describing entry point.
    ///
    /// Written by hand rather than through `Codable`, because the reader is a
    /// Rust type whose field names and enum spellings are the contract. A
    /// synthesised encoding would follow this app's property names, and the
    /// first rename on this side would silently stop producing bundles the
    /// daemon tooling could open.
    private static func header(
        capture: ReportCapture, draft: ReportDraft, build: String, gitSHA: String,
        createdAt: Date, parts: [ReportPart]
    ) -> Data {
        func declared(_ name: String) -> Any {
            guard let part = parts.first(where: { $0.name == name }) else {
                return ["absent": ["reason": "the app did not consider this part"]]
            }
            if part.present { return "present" }
            return ["absent": ["reason": part.absenceReason ?? "not captured"]]
        }

        var declarations: [String: Any] = [
            "frame": declared(frameFile),
            "trace": declared(traceFile),
            "msgs": declared(messagesFile),
            "daemon": declared(daemonFile),
            "log": declared(logFile),
        ]
        // The recorder that made the trace, named only when there is a trace
        // to name it for. What made it decides where it can be replayed: this
        // one came from a native view, so a terminal cannot put it back.
        if parts.first(where: { $0.name == traceFile })?.present == true {
            declarations["trace_kind"] = "native_view"
        }

        let header: [String: Any] = [
            "schema_version": schemaVersion,
            "build": build,
            "git_sha": gitSHA,
            "created_at": rfc3339(createdAt),
            "stamp": stamp(createdAt),
            // Everything written on a phone is a bug report. The terminal's
            // flow asks bug-or-tweak first; the phone's starts from a
            // screenshot of something that looked wrong, and asking somebody
            // to classify it before they have said what it is would be a
            // question in front of the answer.
            "kind": "bug",
            "status": "open",
            "detail": capture.route as Any? ?? NSNull(),
            "note": draft.note,
            "marks": draft.marks.map {
                ["x": $0.x, "y": $0.y, "width": $0.width, "height": $0.height, "note": $0.note]
            },
            // A terminal's viewport is cells. This frame has none: how big it
            // was is `image_frame`, in the points its rectangles are measured
            // in, with the scale its pixels were drawn at.
            "viewport": NSNull(),
            "image_frame": [
                "width_pt": capture.frame.width,
                "height_pt": capture.frame.height,
                "scale": Int(capture.frame.scale.rounded()),
            ],
            "parts": declarations,
            // Nothing on the phone has replayed this. A native trace replays
            // on the platform that drew it, which is a Mac running the app's
            // own replay recipe, so the phone says so rather than claiming a
            // verdict it did not reach.
            "replay": "unchecked",
        ]
        return (try? JSONSerialization.data(
            withJSONObject: header, options: [.sortedKeys, .prettyPrinted])) ?? Data("{}".utf8)
    }

    /// The name this capture would have had on disk: when it happened, to the
    /// millisecond, and something to tell two in the same one apart.
    private static func stamp(_ createdAt: Date) -> String {
        let millis = Int((createdAt.timeIntervalSince1970 * 1000).rounded())
        return "\(millis)-\(Int.random(in: 10000..<100000))"
    }

    /// The instant, written the way the daemon tooling reads it: UTC, to the
    /// millisecond. Built per call rather than kept, because a formatter is
    /// mutable and one shared across threads is a data race waiting for a
    /// second report.
    private static func rfc3339(_ instant: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        return formatter.string(from: instant)
    }
}

/// The shared runtime's frozen recording, taken apart into the two files a
/// bundle carries.
///
/// The runtime answers one JSON object holding a checkpoint, the message lines
/// it folded after it, and the embedded daemon's own dump. On disk those are
/// two files: `msgs.jsonl` is the checkpoint as a header line with the
/// messages under it, and `daemon.json` is the dump. Splitting it here rather
/// than at the writer means the phone's report and the phone's debug recording
/// are assembled from one reading of the same shape.
public enum RuntimeRecording {
    public struct Split: Sendable, Equatable {
        /// `msgs.jsonl`, header line and all, or nothing.
        public var messages: String?
        public var messagesAbsent: String?
        /// `daemon.json`, or nothing.
        public var daemon: String?
        public var daemonAbsent: String?
    }

    public static func split(_ json: String?) -> Split {
        guard let json else {
            return Split(
                messagesAbsent: "nothing was connected, so there was no recording to freeze",
                daemonAbsent: "nothing was connected, so no daemon was asked")
        }
        guard
            let object = try? JSONSerialization.jsonObject(with: Data(json.utf8)),
            let report = object as? [String: Any]
        else {
            return Split(
                messagesAbsent: "the runtime's recording could not be read",
                daemonAbsent: "the runtime's recording could not be read")
        }

        var split = Split()
        if let recording = report["msgs"] as? [String: Any],
           let version = recording["format_version"],
           let checkpoint = recording["checkpoint"],
           let messages = recording["msgs"] as? [String],
           let header = try? JSONSerialization.data(
               withJSONObject: ["format_version": version, "checkpoint": checkpoint]) {
            let lines = [String(decoding: header, as: UTF8.self)] + messages
            split.messages = lines.joined(separator: "\n") + "\n"
        } else {
            split.messagesAbsent = "the runtime answered without a recording in it"
        }

        if let daemon = report["daemon"] as? String {
            split.daemon = daemon
        } else {
            split.daemonAbsent = report["daemon_absent_reason"] as? String
                ?? "the embedded daemon did not answer"
        }
        return split
    }
}
