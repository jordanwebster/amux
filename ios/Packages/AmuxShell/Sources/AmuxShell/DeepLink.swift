import AmuxCore
import Foundation

/// A pairing invitation exactly as it arrived, and nothing more.
///
/// It names the machine that offered it and carries the one-shot secret that
/// proves possession of the offer. Holding it is not pairing: nothing is
/// trusted until the person on this phone looks at the machine's fingerprint
/// and says yes.
public struct PairingInvitation: Hashable, Sendable, CustomStringConvertible {
    public let host: HostId
    public let cloudURL: String
    public let secret: [UInt8]

    public init(host: HostId, cloudURL: String, secret: [UInt8]) {
        self.host = host
        self.cloudURL = cloudURL
        self.secret = secret
    }

    /// The invitation is a secret, so it prints as the machine it came from
    /// and no more; a description that carried the secret would put it into
    /// every log and report that ever mentioned this value.
    public var description: String { "invitation from \(host)" }
}

/// A link the app was opened with.
///
/// Parsing one is not acting on one. A pairing link becomes a page asking the
/// person to confirm; a sign-in callback is not navigation at all and is
/// handed back to whoever started the sign-in.
public enum DeepLink: Hashable, Sendable {
    case pair(PairingInvitation)
    case signInCallback(URL)

    /// The scheme both the CLI's pairing links and the sign-in callback use.
    public static let scheme = "amux"

    /// Reads a link, or refuses it.
    ///
    /// Everything about the link is checked here rather than on the page it
    /// leads to: a malformed invitation must not become a confirmation screen
    /// that cannot say who it is confirming.
    public init?(_ url: URL) {
        guard url.scheme == Self.scheme else { return nil }
        switch url.host ?? "" {
        case "pair":
            guard let payload = URLComponents(url: url, resolvingAgainstBaseURL: false)?
                .queryItems?.first(where: { $0.name == "payload" })?.value,
                let invitation = PairingInvitation(payload: payload)
            else { return nil }
            self = .pair(invitation)
        case "callback":
            self = .signInCallback(url)
        default:
            return nil
        }
    }
}

extension PairingInvitation {
    /// Reads the payload the `amux pair --qr` link carries: the JSON the host
    /// wrote, in URL-safe base64 without padding.
    init?(payload: String) {
        guard let json = Data(base64URLEncoded: payload),
            let wire = try? JSONDecoder().decode(Wire.self, from: json),
            let host = HostId(wire.hostID)
        else { return nil }
        self.init(host: host, cloudURL: wire.cloudURL, secret: wire.secret)
    }

    private struct Wire: Decodable {
        let hostID: String
        let cloudURL: String
        let secret: [UInt8]

        enum CodingKeys: String, CodingKey {
            case hostID = "host_id"
            case cloudURL = "cloud_url"
            case secret
        }
    }
}

extension Data {
    /// Base64 as a URL carries it: the two substituted characters put back and
    /// the padding the encoder dropped restored.
    init?(base64URLEncoded text: String) {
        var standard = text.replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let remainder = standard.count % 4
        if remainder > 0 { standard += String(repeating: "=", count: 4 - remainder) }
        guard let data = Data(base64Encoded: standard) else { return nil }
        self = data
    }
}
