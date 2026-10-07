import AmuxApp
import AmuxValues
import Foundation

extension Bridge {
    /// The version of the Rust library this build linked. A library built
    /// with the driving tools says so after a `+`; a release binary is
    /// checked for not carrying that suffix, so it is spelled nowhere here.
    public static var version: String {
        guard let version = amux_version() else { return "" }
        return String(cString: version)
    }

    /// Why `name` cannot name an agent, or nil when it can: the rule every
    /// host holds a create or a rename to.
    public static func agentNameProblem(_ name: String) -> AgentNameProblem? {
        name.withCString { Bridge.read(AgentNameProblem.self, amux_agent_name_problem($0)) }
    }
}
