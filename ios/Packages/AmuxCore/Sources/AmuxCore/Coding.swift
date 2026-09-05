import Foundation

/// The one coder that speaks the bridge's JSON.
///
/// Timestamps arrive as RFC 3339 in UTC, with anything from no fractional
/// seconds to nine digits of them. Foundation's built-in ISO 8601 strategy
/// accepts only one of those shapes, so the seconds are parsed here instead of
/// letting a nanosecond timestamp fail a whole batch.
public enum AmuxJSON {
    public static var decoder: JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let text = try decoder.singleValueContainer().decode(String.self)
            guard let date = timestamp(text) else {
                throw DecodingError.dataCorrupted(.init(
                    codingPath: decoder.codingPath,
                    debugDescription: "not an RFC 3339 timestamp: \(text)"))
            }
            return date
        }
        return decoder
    }

    public static var encoder: JSONEncoder {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .custom { date, encoder in
            var container = encoder.singleValueContainer()
            try container.encode(text(date))
        }
        return encoder
    }

    static func timestamp(_ text: String) -> Date? {
        // Chrono writes UTC as a trailing `Z`; a relayed timestamp that spells
        // the same offset out longhand means the same instant.
        var body = text.hasSuffix("+00:00") ? String(text.dropLast(6)) + "Z" : text
        var fraction: TimeInterval = 0
        if let dot = body.firstIndex(of: ".") {
            let after = body.index(after: dot)
            let digits = body[after...].prefix { $0.isNumber }
            fraction = Double("0.\(digits)") ?? 0
            body.removeSubrange(dot..<body.index(after, offsetBy: digits.count))
        }
        guard let whole = wholeSeconds.date(from: body) else { return nil }
        return whole.addingTimeInterval(fraction)
    }

    static func text(_ date: Date) -> String {
        let whole = date.timeIntervalSince1970.rounded(.down)
        let fraction = date.timeIntervalSince1970 - whole
        var stamp = wholeSeconds.string(from: Date(timeIntervalSince1970: whole))
        guard fraction > 0.0000000005 else { return stamp }
        var digits = String(format: "%.9f", fraction).dropFirst(2)
        while digits.hasSuffix("0") { digits = digits.dropLast() }
        stamp.removeLast()
        return "\(stamp).\(digits)Z"
    }

    private static let wholeSeconds: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        formatter.dateFormat = "yyyy-MM-dd'T'HH:mm:ss'Z'"
        return formatter
    }()
}
