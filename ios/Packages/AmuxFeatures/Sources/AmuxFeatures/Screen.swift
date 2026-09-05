import Foundation

/// Every screen the app can be asked to show by name, by a golden run or by a
/// journey. The names are the design's own, so a capture, a fixture and a
/// conversation about the design all say the same word.
public enum Screen: String, Sendable, CaseIterable, Codable {
    /// Not a screen of the app: the harness's own target for proving that a
    /// capture, a diff and a token change are all working.
    case probe

    // 1 · Opening the app
    case home
    case homeQuiet = "home-quiet"
    case drawer

    // 2 · A conversation
    case run
    case runLive = "run-live"
    case voices
    case reviewCta = "review-cta"

    // 3 · When it needs you
    case askPermission = "ask-permission"
    case askQuestion = "ask-question"
    case plan
    case diff
    case comment

    // 4 · Writing to it
    case typing
    case plus
    case settings
    case slashTyping = "slash-typing"
    case working
    case queued
    case overflow
    case agentDelete = "agent-delete"

    // 5 · Machines and agents
    case hosts
    case pin
    case newAgent = "new-agent"
    case offline
    case exited

    // 6 · You
    case profiles
    case you
    case delete
    case firstRun = "first-run"
    case signIn = "sign-in"
    case firstRunPaid = "first-run-paid"
    case paywall

    // 7 · When it goes wrong
    case shake
    case dump
}
