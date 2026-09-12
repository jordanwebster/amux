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
        // the same offset out longhand means the same instant. Keep the
        // accepted grammar narrow while using Foundation's value parser:
        // DateFormatter serialises every parse and made a fleet confirmation
        // spend most of its time reading timestamps it had already seen.
        guard text.hasSuffix("Z") || text.hasSuffix("+00:00") else { return nil }
        let style = text.contains(".") ? fractionalTimestamp : wholeTimestamp
        return try? style.parse(text)
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

    private static let wholeTimestamp = Date.ISO8601FormatStyle(includingFractionalSeconds: false)
    private static let fractionalTimestamp = Date.ISO8601FormatStyle(includingFractionalSeconds: true)
}
