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

    /// A new agent's settings from what its host offers and what is chosen
    /// so far, as the shared settings view builds them.
    public static func newAgentSettings(_ catalogue: Catalogue, _ chosen: NewAgentChoices) -> SettingsView? {
        Bridge.json(catalogue).withCString { catalogue in
            Bridge.json(chosen).withCString { chosen in
                Bridge.read(SettingsView.self, amux_new_agent_settings(catalogue, chosen))
            }
        }
    }

    /// What is chosen for a new agent after `pick`, with the rules a pick
    /// carries: another model starts at its default effort and gives up a
    /// permission it does not take.
    public static func newAgentPick(
        _ catalogue: Catalogue, _ chosen: NewAgentChoices, _ pick: NewAgentPick
    ) -> NewAgentChoices? {
        Bridge.json(catalogue).withCString { catalogue in
            Bridge.json(chosen).withCString { chosen in
                Bridge.json(pick).withCString { pick in
                    Bridge.read(NewAgentChoices.self, amux_new_agent_pick(catalogue, chosen, pick))
                }
            }
        }
    }
}
