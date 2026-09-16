import AmuxCore
import Foundation

extension ReportFreeze {
    /// The freeze a build with the driving tools takes: everything every build
    /// takes, and the recording of what the views were doing beside it.
    ///
    /// The recording is only written where it can be put back. A replay walks
    /// the trail the door kept and rebuilds the stores from the facts at the
    /// end of it, and both of those live in the driving tools.
    static func driven(
        route: @escaping () -> String? = { DoorHost.shared.screen?.rawValue },
        place: @escaping () -> Place? = {
            DoorHost.shared.screen.map { Place.screen($0.rawValue) }
        },
        account: @escaping () -> AccountEntry? = { DoorHost.shared.accountOnScreen },
        ordered: @escaping () -> Date = { DoorHost.shared.stores.fleet.orderedAt },
        drafts: @escaping () -> [AgentId: MessageDraft] = {
            DoorHost.shared.stores.conversations.compactMapValues {
                $0.draft.isEmpty ? nil : $0.draft
            }
        },
        runtimeFailure: @escaping () -> String? = { nil }
    ) -> ReportFreeze {
        ReportFreeze(
            route: route,
            trace: {
                traceLines(
                    place: place(), account: account(), ordered: ordered(), drafts: drafts())
            },
            runtimeFailure: runtimeFailure)
    }

    /// The view-state recording a bundle carries: what has been done to the
    /// view since launch, ending with the place the freeze happened in and the
    /// two facts that place was standing on — the clock and the account.
    ///
    /// The place on show is written even when nothing has changed the view.
    /// Somebody who opens the app and photographs the first thing they see has
    /// changed nothing, and a trace declared present but empty says "nothing
    /// was recorded" and "nothing happened" in the same breath — while a
    /// replay of it puts back no screen at all.
    ///
    /// What was open, what was half written and where each transcript was being
    /// read go after the place, because each of them is a state of a screen
    /// rather than a step on the way to one — and because putting the open card
    /// back needs the place it was open over to have been decided first. They
    /// are written in agent order so two freezes of the same screen produce the
    /// same file.
    ///
    /// The clock and the account go last because they are what the frozen
    /// screen was reading, not something that happened: a replay builds its
    /// stores from them before it folds a single message, and without them it
    /// rebuilds the right rows under the wrong name with the wrong ages on
    /// them.
    private static func traceLines(
        place: Place?, account: AccountEntry?, ordered: Date,
        drafts written: [AgentId: MessageDraft]
    ) -> Result<String, PartAbsent> {
        var events = DoorHost.shared.traceEvents
        if let place, events.last != .route(place) {
            events.append(.route(place))
        }
        // Where the recording ends, read off the trail rather than asked for
        // again. A freeze asked for through the driving door is not told where
        // the app is — it is given no page to ask — and the trail is the one
        // account of it that is right either way.
        let ended: Place? = events.reversed().compactMap {
            if case .route(let place) = $0 { return place }
            return nil
        }.first
        if case .conversation(let agent)? = ended {
            events.append(.sheet(DoorHost.shared.panels[agent]))
        }
        let readings = DoorHost.shared.readings
        for agent in readings.keys.sorted(by: { $0.description < $1.description }) {
            events.append(.reading(agent, readings[agent]!))
        }
        for agent in written.keys.sorted(by: { $0.description < $1.description }) {
            events.append(.draft(agent, written[agent]!))
        }
        for agent in DoorHost.shared.asides.sorted(by: { $0.description < $1.description }) {
            events.append(.setAside(agent))
        }
        events.append(.frozen(at: Date(), ordered: ordered))
        events.append(.account(account))
        do { return .success(try Trace.lines(events)) } catch {
            return .failure(PartAbsent("the view-state recording could not be written: \(error)"))
        }
    }
}
