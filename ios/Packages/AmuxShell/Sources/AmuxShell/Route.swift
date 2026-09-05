import AmuxCore
import Foundation

/// The three places the app is, as the tab bar names them.
public enum Tab: String, Hashable, Sendable, CaseIterable, Codable {
    case agents
    case hosts
    case you

    public var title: String {
        switch self {
        case .agents: "Agents"
        case .hosts: "Hosts"
        case .you: "You"
        }
    }

    public var symbol: String {
        switch self {
        case .agents: "square.stack"
        case .hosts: "desktopcomputer"
        case .you: "person"
        }
    }
}

/// Somewhere the app can be pushed to, and everything needed to draw it there.
///
/// A route carries what it takes to show the page and nothing that has to be
/// fetched first: an agent's page is its identity, and what the agent has said
/// arrives afterwards. That is what lets a tap push on the frame it happened
/// in rather than after a round trip to the host.
public enum Route: Hashable, Sendable {
    case conversation(AgentId)
    /// The changes one agent has made, opened from its conversation.
    case changes(AgentId)
    case newAgent
    case pairByCode
    /// A pairing invitation that arrived as a link, waiting to be confirmed or
    /// abandoned. Arriving here pairs with nobody.
    case pairConfirmation(PairingInvitation)
    case host(HostId)
    case accounts
    case appearance
    case help

    /// Which tab this page belongs under. A route opened from elsewhere takes
    /// its tab with it, so the tab bar always agrees with what is on screen.
    public var tab: Tab {
        switch self {
        case .conversation, .changes: .agents
        case .newAgent, .pairByCode, .pairConfirmation, .host: .hosts
        case .accounts, .appearance, .help: .you
        }
    }

    /// What this page is called where a page is named by a word: a trace of
    /// what somebody was looking at, a journey's assertion, a report.
    public var name: String {
        switch self {
        case .conversation: "conversation"
        case .changes: "changes"
        case .newAgent: "new-agent"
        case .pairByCode: "pin"
        case .pairConfirmation: "pair-confirm"
        case .host: "host"
        case .accounts: "profiles"
        case .appearance: "appearance"
        case .help: "help"
        }
    }
}
