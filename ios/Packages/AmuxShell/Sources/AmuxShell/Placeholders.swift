import AmuxCore
import AmuxDesign
import AmuxFeatures
import SwiftUI

// The shell is built before the screens that hang off it, so each tab and each
// page is stood up here as the plainest thing that reads the right store and
// emits the right action. They are not the design: a screen replaces the one
// named for it as it lands, and nothing else about the shell changes when it
// does. Nothing here is a golden's subject; captures open a screen by name.

/// The Agents tab until the home screen lands.
struct FleetPlaceholder: View {
    @Environment(\.design) private var design
    let router: Router
    let stores: StoreBundle

    var body: some View {
        List {
            Section {
                ForEach(stores.fleet.rows) { row in
                    Button {
                        router.open(.conversation(row.id))
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(row.name)
                                .designFont(.identifier, design)
                                .foregroundStyle(design.ink.color)
                            Text(row.workingDirectory)
                                .designFont(.caption, design)
                                .foregroundStyle(design.inkMuted.color)
                        }
                    }
                    .identified("fleet.row.\(row.id)", label: row.name, value: row.workingDirectory)
                }
            } header: {
                Text(stores.fleet.subtitle)
                    .identified("fleet.subtitle", value: stores.fleet.subtitle)
            } footer: {
                if let exceptions = stores.fleet.exceptions {
                    Text(exceptions).identified("fleet.exceptions", value: exceptions)
                }
            }
        }
        .identified("fleet")
    }
}

/// The Hosts tab until the hosts screen lands.
struct HostsPlaceholder: View {
    @Environment(\.design) private var design
    let router: Router
    let stores: StoreBundle

    var body: some View {
        List {
            ForEach(stores.hosts.hosts) { host in
                Button {
                    router.open(.host(host.id))
                } label: {
                    Text(host.name)
                        .designFont(.identifier, design)
                        .foregroundStyle(design.ink.color)
                }
                .identified("hosts.row.\(host.id)", label: host.name,
                            value: host.online ? "online" : "offline")
            }
            Button("Pair by Code") { router.open(.pairByCode) }
                .identified("hosts.pairByCode", label: "Pair by Code")
            Button("New Agent") { router.open(.newAgent) }
                .identified("hosts.newAgent", label: "New Agent")
        }
        .identified("hosts")
    }
}

/// The You tab until its screens land.
struct YouPlaceholder: View {
    let router: Router
    let accounts: AccountRegistry
    let actions: @MainActor (ShellAction) -> Void

    var body: some View {
        List {
            if accounts.accounts.isEmpty {
                Button("Sign In") { actions(.signIn) }
                    .identified("you.signIn", label: "Sign In")
                Button("Subscribe") { actions(.subscribe) }
                    .identified("you.subscribe", label: "Subscribe")
            }
            Button("Accounts") { router.open(.accounts) }
                .identified("you.accounts", label: "Accounts")
            Button("Appearance") { router.open(.appearance) }
                .identified("you.appearance", label: "Appearance")
            Button("Help") { router.open(.help) }
                .identified("you.help", label: "Help")
        }
        .identified("you")
    }
}

/// One agent's page until the conversation lands. It shows what the store has
/// for that agent, which on the frame the page is pushed is nothing: the page
/// is up and the transcript arrives into it.
struct ConversationPlaceholder: View {
    @Environment(\.design) private var design
    let agent: AgentId
    let router: Router
    let stores: StoreBundle

    var body: some View {
        let conversation = stores.conversation(agent)
        VStack(alignment: .leading, spacing: design.metrics.feedGap) {
            Text(name)
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .identified("conversation.name", value: name)
            Text("\(conversation.entries.count) entries")
                .designFont(.caption, design)
                .foregroundStyle(design.inkMuted.color)
                .identified("conversation.entries", value: "\(conversation.entries.count)")
            Button("Changes") { router.open(.changes(agent)) }
                .identified("conversation.changes", label: "Changes")
            Spacer()
        }
        .padding(design.metrics.gutter)
        .frame(maxWidth: .infinity, alignment: .leading)
        .identified("conversation", value: agent.description)
    }

    /// The fleet knows the name; a conversation opened before the fleet has
    /// arrived shows the identity it was opened with rather than nothing.
    private var name: String {
        stores.fleet.rows.first { $0.id == agent }?.name ?? agent.description
    }
}

/// A route whose screen has not been built. It says which one, because a page
/// that silently showed nothing would look like a screen that failed to load.
struct UnbuiltPage: View {
    @Environment(\.design) private var design
    let route: Route

    var body: some View {
        Text(route.name)
            .designFont(.body, design)
            .foregroundStyle(design.inkMuted.color)
            .identified("page.\(route.name)", value: route.name)
    }
}
