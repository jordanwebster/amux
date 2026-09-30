//! The terminal client over a scenario's world: the same `App` the `amux`
//! binary runs, constructed over the lab's client instead of the daemon's
//! socket.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use client::SystemClock;
use tui::{App, Theme, TuiConfig};
use ui_runtime::Fleet;

use crate::place::Place;
use crate::scenario::Scenario;
use crate::world::{LabClient, World};

pub struct Booted {
    pub world: Arc<World>,
    pub app: App,
}

pub async fn boot(
    scenario: &Scenario,
    place: Option<&Place>,
    instant: bool,
    theme: Theme,
) -> Result<Booted> {
    let fired = place.map_or(0, |p| p.fired);
    let world = World::new(scenario.clone(), fired, instant);
    let client = Arc::new(LabClient(world.clone()));
    let fleet = Fleet::open(client.clone(), SystemClock).await?;
    let open = place
        .map(|p| p.open.clone())
        .unwrap_or_else(|| scenario.open.clone());
    let config = TuiConfig {
        working_dir: scenario.cwd.clone().into(),
        leader: 'a',
        theme,
        initial_chat: open.map(String::into_bytes),
        attach: false,
        version: "lab".into(),
        local_host: world.local_host(),
    };
    let mut app = App::new(client, fleet, config);
    // The fleet has already caught up, so no change will arrive to open the
    // first chat; take the inventory in now.
    app.fleet_changed();
    let selected = place.and_then(|p| p.selected.clone());
    if let Some(id) = selected {
        let key = app
            .fleet
            .state()
            .find(id.as_bytes())
            .map(ui_state::agent_key);
        if let Some(key) = key {
            app.fleet_view.select(key);
        }
    }
    if let Some(place) = place {
        tui::variant::set(place.variant);
    }
    Ok(Booted { world, app })
}

/// Puts the saved draft and scroll anchor back once the saved chat is open;
/// true when done (or when there is nothing to do).
pub fn restore_chat(app: &mut App, place: &Place) -> bool {
    let Some(open) = &place.open else {
        return true;
    };
    let Some(chat) = app.chat.as_mut() else {
        return false;
    };
    if chat.view.agent_id != open.as_bytes() {
        return true;
    }
    if !place.draft.is_empty() && chat.view.editor.text().is_empty() {
        chat.view.editor.insert_str(&place.draft);
    }
    if let Some((key, offset)) = &place.anchor {
        let held = chat.session.state().transcript().get(key).is_some();
        if held {
            chat.view.anchor = tui::chat::layout::Anchor::Top {
                key: key.clone(),
                offset: *offset,
            };
            if let Some(following) = chat.view.following_moved() {
                chat.session.follow(following);
            }
        }
    }
    true
}

/// Lets the app take in everything already on its way: finished tasks, the
/// fleet and the open chat. Returns once nothing has arrived for `quiet`.
pub async fn settle(app: &mut App, quiet: Duration) {
    let mut fleet_changed = app.fleet.changed();
    loop {
        app.housekeep();
        let mut chat = app.chat.as_ref().map(|chat| chat.session.changed());
        tokio::select! {
            Some(event) = app.receiver.recv() => {
                app.event(event);
            }
            changed = fleet_changed.changed() => {
                if changed.is_err() {
                    return;
                }
                app.fleet_changed();
            }
            _ = async {
                match chat.as_mut() {
                    Some(changed) => { let _ = changed.changed().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {}
            _ = tokio::time::sleep(quiet) => return,
        }
    }
}
