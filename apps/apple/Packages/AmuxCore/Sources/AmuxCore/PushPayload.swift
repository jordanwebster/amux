import AmuxValues
import Foundation

/// What a "needs you" notification names: the agent, and the host it runs
/// on, each as its UUID under the payload's `amux` key:
///
///     {"aps": {"content-available": 1, "alert": {…}},
///      "amux": {"host": "<uuid>", "agent": "<uuid>"}}
///
/// The account service builds the payload from what the agent's daemon
/// hands it; the phone reads nothing else from it.
public enum PushPayload {
    public static func agent(_ payload: [AnyHashable: Any]) -> AgentKey? {
        guard let amux = payload["amux"] as? [String: Any],
              let host = (amux["host"] as? String).flatMap(HostId.init),
              let agent = (amux["agent"] as? String).flatMap(UUID.init(uuidString:))
        else { return nil }
        return AgentKey(host: host, agent: agent)
    }
}
