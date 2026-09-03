//! The chrome: the one place renderer state changes.
//!
//! Everything that can move the fleet or a chat — a keypress, a paste, a
//! wheel, a resize, a draw, a dispatched op, a chat opening — is a
//! [`TraceEvent`], and [`Chrome::step`] is the only function that applies
//! one. The live loop in `run.rs` converts terminal events into
//! `TraceEvent`s and steps them; a replay reads the same events back from
//! a recorded trace and steps them the same way. That is the whole point
//! of the split: if the two paths shared only "the key handlers", a
//! replay would still have to re-derive the order of reconciles, chat
//! openings and draws around them, and any drift there is invisible until
//! a replayed frame quietly disagrees with the captured one.
//!
//! Step returns [`ShellEffect`]s rather than performing them. A replay has
//! no runtime to dispatch to, no terminal to write OSC 52 into and no
//! agent to attach to, so it drops them; the live loop performs them and
//! records what came back (an op id) as the next event.
//!
//! Draws are events because drawing mutates state: chat key handling reads
//! feed metrics the previous paint cached, so replaying inputs without the
//! draws between them diverges. `Chrome::step` builds the frame's lines
//! and holds them for the caller to paint — the caller owns the buffer,
//! the chrome owns everything that changed on the way there.

use amux_ui::{AgentId, Command, HostId, Model, Msg, OpId};
use chrono::{DateTime, Utc};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MediaKeyCode,
    ModifierKeyCode, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::text::Line;
use serde::{Deserialize, Serialize};

use crate::render::{FrameContext, Theme, build_lines, list_capacity};
use crate::view::{Notice, UiAction, ViewState};

/// A key event in a form that survives a round trip through JSON.
///
/// crossterm ships serde impls behind a feature this crate does not enable
/// — and would not want to, since the wire shape of a diagnostic recording
/// should not be a dependency's private choice. The modifier and state
/// flags travel as their bits: the flag set is crossterm's contract, and
/// naming each flag here would only restate it in a second place that can
/// fall behind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRecord {
    pub code: KeyCodeRecord,
    pub modifiers: u8,
    pub kind: KeyKindRecord,
    pub state: u8,
}

impl KeyRecord {
    pub fn from_event(key: KeyEvent) -> Self {
        Self {
            code: KeyCodeRecord::from_code(key.code),
            modifiers: key.modifiers.bits(),
            kind: KeyKindRecord::from_kind(key.kind),
            state: key.state.bits(),
        }
    }

    pub fn to_event(&self) -> KeyEvent {
        KeyEvent {
            code: self.code.to_code(),
            modifiers: KeyModifiers::from_bits_truncate(self.modifiers),
            kind: self.kind.to_kind(),
            state: KeyEventState::from_bits_truncate(self.state),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyKindRecord {
    Press,
    Repeat,
    Release,
}

impl KeyKindRecord {
    fn from_kind(kind: KeyEventKind) -> Self {
        match kind {
            KeyEventKind::Press => Self::Press,
            KeyEventKind::Repeat => Self::Repeat,
            KeyEventKind::Release => Self::Release,
        }
    }

    fn to_kind(self) -> KeyEventKind {
        match self {
            Self::Press => KeyEventKind::Press,
            Self::Repeat => KeyEventKind::Repeat,
            Self::Release => KeyEventKind::Release,
        }
    }
}

/// Every key crossterm can deliver. Exhaustive on purpose: a crossterm
/// upgrade that adds a key breaks this match instead of silently recording
/// a keypress as something else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyCodeRecord {
    Backspace,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Delete,
    Insert,
    F(u8),
    Char(char),
    Null,
    Esc,
    CapsLock,
    ScrollLock,
    NumLock,
    PrintScreen,
    Pause,
    Menu,
    KeypadBegin,
    Media(MediaRecord),
    Modifier(ModifierRecord),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaRecord {
    Play,
    Pause,
    PlayPause,
    Reverse,
    Stop,
    FastForward,
    Rewind,
    TrackNext,
    TrackPrevious,
    Record,
    LowerVolume,
    RaiseVolume,
    MuteVolume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModifierRecord {
    LeftShift,
    LeftControl,
    LeftAlt,
    LeftSuper,
    LeftHyper,
    LeftMeta,
    RightShift,
    RightControl,
    RightAlt,
    RightSuper,
    RightHyper,
    RightMeta,
    IsoLevel3Shift,
    IsoLevel5Shift,
}

impl KeyCodeRecord {
    fn from_code(code: KeyCode) -> Self {
        match code {
            KeyCode::Backspace => Self::Backspace,
            KeyCode::Enter => Self::Enter,
            KeyCode::Left => Self::Left,
            KeyCode::Right => Self::Right,
            KeyCode::Up => Self::Up,
            KeyCode::Down => Self::Down,
            KeyCode::Home => Self::Home,
            KeyCode::End => Self::End,
            KeyCode::PageUp => Self::PageUp,
            KeyCode::PageDown => Self::PageDown,
            KeyCode::Tab => Self::Tab,
            KeyCode::BackTab => Self::BackTab,
            KeyCode::Delete => Self::Delete,
            KeyCode::Insert => Self::Insert,
            KeyCode::F(n) => Self::F(n),
            KeyCode::Char(c) => Self::Char(c),
            KeyCode::Null => Self::Null,
            KeyCode::Esc => Self::Esc,
            KeyCode::CapsLock => Self::CapsLock,
            KeyCode::ScrollLock => Self::ScrollLock,
            KeyCode::NumLock => Self::NumLock,
            KeyCode::PrintScreen => Self::PrintScreen,
            KeyCode::Pause => Self::Pause,
            KeyCode::Menu => Self::Menu,
            KeyCode::KeypadBegin => Self::KeypadBegin,
            KeyCode::Media(media) => Self::Media(match media {
                MediaKeyCode::Play => MediaRecord::Play,
                MediaKeyCode::Pause => MediaRecord::Pause,
                MediaKeyCode::PlayPause => MediaRecord::PlayPause,
                MediaKeyCode::Reverse => MediaRecord::Reverse,
                MediaKeyCode::Stop => MediaRecord::Stop,
                MediaKeyCode::FastForward => MediaRecord::FastForward,
                MediaKeyCode::Rewind => MediaRecord::Rewind,
                MediaKeyCode::TrackNext => MediaRecord::TrackNext,
                MediaKeyCode::TrackPrevious => MediaRecord::TrackPrevious,
                MediaKeyCode::Record => MediaRecord::Record,
                MediaKeyCode::LowerVolume => MediaRecord::LowerVolume,
                MediaKeyCode::RaiseVolume => MediaRecord::RaiseVolume,
                MediaKeyCode::MuteVolume => MediaRecord::MuteVolume,
            }),
            KeyCode::Modifier(modifier) => Self::Modifier(match modifier {
                ModifierKeyCode::LeftShift => ModifierRecord::LeftShift,
                ModifierKeyCode::LeftControl => ModifierRecord::LeftControl,
                ModifierKeyCode::LeftAlt => ModifierRecord::LeftAlt,
                ModifierKeyCode::LeftSuper => ModifierRecord::LeftSuper,
                ModifierKeyCode::LeftHyper => ModifierRecord::LeftHyper,
                ModifierKeyCode::LeftMeta => ModifierRecord::LeftMeta,
                ModifierKeyCode::RightShift => ModifierRecord::RightShift,
                ModifierKeyCode::RightControl => ModifierRecord::RightControl,
                ModifierKeyCode::RightAlt => ModifierRecord::RightAlt,
                ModifierKeyCode::RightSuper => ModifierRecord::RightSuper,
                ModifierKeyCode::RightHyper => ModifierRecord::RightHyper,
                ModifierKeyCode::RightMeta => ModifierRecord::RightMeta,
                ModifierKeyCode::IsoLevel3Shift => ModifierRecord::IsoLevel3Shift,
                ModifierKeyCode::IsoLevel5Shift => ModifierRecord::IsoLevel5Shift,
            }),
        }
    }

    fn to_code(self) -> KeyCode {
        match self {
            Self::Backspace => KeyCode::Backspace,
            Self::Enter => KeyCode::Enter,
            Self::Left => KeyCode::Left,
            Self::Right => KeyCode::Right,
            Self::Up => KeyCode::Up,
            Self::Down => KeyCode::Down,
            Self::Home => KeyCode::Home,
            Self::End => KeyCode::End,
            Self::PageUp => KeyCode::PageUp,
            Self::PageDown => KeyCode::PageDown,
            Self::Tab => KeyCode::Tab,
            Self::BackTab => KeyCode::BackTab,
            Self::Delete => KeyCode::Delete,
            Self::Insert => KeyCode::Insert,
            Self::F(n) => KeyCode::F(n),
            Self::Char(c) => KeyCode::Char(c),
            Self::Null => KeyCode::Null,
            Self::Esc => KeyCode::Esc,
            Self::CapsLock => KeyCode::CapsLock,
            Self::ScrollLock => KeyCode::ScrollLock,
            Self::NumLock => KeyCode::NumLock,
            Self::PrintScreen => KeyCode::PrintScreen,
            Self::Pause => KeyCode::Pause,
            Self::Menu => KeyCode::Menu,
            Self::KeypadBegin => KeyCode::KeypadBegin,
            Self::Media(media) => KeyCode::Media(match media {
                MediaRecord::Play => MediaKeyCode::Play,
                MediaRecord::Pause => MediaKeyCode::Pause,
                MediaRecord::PlayPause => MediaKeyCode::PlayPause,
                MediaRecord::Reverse => MediaKeyCode::Reverse,
                MediaRecord::Stop => MediaKeyCode::Stop,
                MediaRecord::FastForward => MediaKeyCode::FastForward,
                MediaRecord::Rewind => MediaKeyCode::Rewind,
                MediaRecord::TrackNext => MediaKeyCode::TrackNext,
                MediaRecord::TrackPrevious => MediaKeyCode::TrackPrevious,
                MediaRecord::Record => MediaKeyCode::Record,
                MediaRecord::LowerVolume => MediaKeyCode::LowerVolume,
                MediaRecord::RaiseVolume => MediaKeyCode::RaiseVolume,
                MediaRecord::MuteVolume => MediaKeyCode::MuteVolume,
            }),
            Self::Modifier(modifier) => KeyCode::Modifier(match modifier {
                ModifierRecord::LeftShift => ModifierKeyCode::LeftShift,
                ModifierRecord::LeftControl => ModifierKeyCode::LeftControl,
                ModifierRecord::LeftAlt => ModifierKeyCode::LeftAlt,
                ModifierRecord::LeftSuper => ModifierKeyCode::LeftSuper,
                ModifierRecord::LeftHyper => ModifierKeyCode::LeftHyper,
                ModifierRecord::LeftMeta => ModifierKeyCode::LeftMeta,
                ModifierRecord::RightShift => ModifierKeyCode::RightShift,
                ModifierRecord::RightControl => ModifierKeyCode::RightControl,
                ModifierRecord::RightAlt => ModifierKeyCode::RightAlt,
                ModifierRecord::RightSuper => ModifierKeyCode::RightSuper,
                ModifierRecord::RightHyper => ModifierKeyCode::RightHyper,
                ModifierRecord::RightMeta => ModifierKeyCode::RightMeta,
                ModifierRecord::IsoLevel3Shift => ModifierKeyCode::IsoLevel3Shift,
                ModifierRecord::IsoLevel5Shift => ModifierKeyCode::IsoLevel5Shift,
            }),
        }
    }
}

/// A mouse event in recordable form. Cell coordinates, not pixels — the
/// only thing the chat hit-tests against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseRecord {
    pub kind: MouseKindRecord,
    pub column: u16,
    pub row: u16,
    pub modifiers: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseKindRecord {
    Down(ButtonRecord),
    Up(ButtonRecord),
    Drag(ButtonRecord),
    Moved,
    ScrollDown,
    ScrollUp,
    ScrollLeft,
    ScrollRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ButtonRecord {
    Left,
    Right,
    Middle,
}

impl MouseRecord {
    pub fn from_event(mouse: MouseEvent) -> Self {
        fn button(button: MouseButton) -> ButtonRecord {
            match button {
                MouseButton::Left => ButtonRecord::Left,
                MouseButton::Right => ButtonRecord::Right,
                MouseButton::Middle => ButtonRecord::Middle,
            }
        }
        Self {
            kind: match mouse.kind {
                MouseEventKind::Down(b) => MouseKindRecord::Down(button(b)),
                MouseEventKind::Up(b) => MouseKindRecord::Up(button(b)),
                MouseEventKind::Drag(b) => MouseKindRecord::Drag(button(b)),
                MouseEventKind::Moved => MouseKindRecord::Moved,
                MouseEventKind::ScrollDown => MouseKindRecord::ScrollDown,
                MouseEventKind::ScrollUp => MouseKindRecord::ScrollUp,
                MouseEventKind::ScrollLeft => MouseKindRecord::ScrollLeft,
                MouseEventKind::ScrollRight => MouseKindRecord::ScrollRight,
            },
            column: mouse.column,
            row: mouse.row,
            modifiers: mouse.modifiers.bits(),
        }
    }

    pub fn to_event(&self) -> MouseEvent {
        fn button(button: ButtonRecord) -> MouseButton {
            match button {
                ButtonRecord::Left => MouseButton::Left,
                ButtonRecord::Right => MouseButton::Right,
                ButtonRecord::Middle => MouseButton::Middle,
            }
        }
        MouseEvent {
            kind: match self.kind {
                MouseKindRecord::Down(b) => MouseEventKind::Down(button(b)),
                MouseKindRecord::Up(b) => MouseEventKind::Up(button(b)),
                MouseKindRecord::Drag(b) => MouseEventKind::Drag(button(b)),
                MouseKindRecord::Moved => MouseEventKind::Moved,
                MouseKindRecord::ScrollDown => MouseEventKind::ScrollDown,
                MouseKindRecord::ScrollUp => MouseEventKind::ScrollUp,
                MouseKindRecord::ScrollLeft => MouseEventKind::ScrollLeft,
                MouseKindRecord::ScrollRight => MouseEventKind::ScrollRight,
            },
            column: self.column,
            row: self.row,
            modifiers: KeyModifiers::from_bits_truncate(self.modifiers),
        }
    }
}

/// The terminal events the chrome acts on. Focus changes are not among
/// them: nothing on screen reads focus, so recording them would only add
/// noise a replay has to skip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    Key(KeyRecord),
    Mouse(MouseRecord),
    Paste(String),
    Resize(u16, u16),
}

impl InputEvent {
    /// The terminal event this chrome cares about, or `None` for one it
    /// ignores entirely.
    pub fn from_terminal(event: &Event) -> Option<Self> {
        match event {
            Event::Key(key) => Some(Self::Key(KeyRecord::from_event(*key))),
            Event::Mouse(mouse) => Some(Self::Mouse(MouseRecord::from_event(*mouse))),
            Event::Paste(text) => Some(Self::Paste(text.clone())),
            Event::Resize(width, height) => Some(Self::Resize(*width, *height)),
            Event::FocusGained | Event::FocusLost => None,
        }
    }
}

/// Everything that mutates chrome state, in the order it happened.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TraceEvent {
    /// One folded runtime message. The caller folds it into the Model
    /// before stepping — the chrome never owns the Model — so stepping it
    /// only marks the screen stale.
    Msg(Msg),
    /// The runtime's pending messages are folded; reconcile the chat
    /// against the fresh Model (a finished send op may carry the failure
    /// fact that resurfaces the draft).
    Drained,
    Input {
        event: InputEvent,
        viewport: (u16, u16),
        now: DateTime<Utc>,
    },
    Draw {
        viewport: (u16, u16),
        now: DateTime<Utc>,
    },
    /// The shell dispatched a command and the runtime minted this op id.
    /// The id is the shell's to mint, so it enters as its own event
    /// rather than being guessed by a replay.
    Dispatched { op: OpId, command: Command },
    /// The status line's transient notice, set by something the chrome
    /// did not do itself: an attach that ended, a clipboard write, a
    /// written report. The shell knows the outcome; the chrome owns the
    /// view, so the outcome comes back in as an event rather than the
    /// shell reaching into the view behind the trace's back.
    Notice(Option<Notice>),
    /// The clock moved far enough to disarm a quit guard. The shell's
    /// 1 Hz tick is not itself an event — most of them change nothing —
    /// but the expiry it triggers is view state, so it enters the trace
    /// like every other mutation and a replay disarms the guard at the
    /// same point in the fold.
    Tick { now: DateTime<Utc> },
    /// The chat opened for a reason outside any keypress — the agent
    /// `amux new` asked for, once its inventory row arrived.
    ChatOpened {
        agent: AgentId,
        codex_configuration: Option<String>,
    },
}

/// What the chrome asks the shell to do. A replay drops all of them: it
/// has no runtime, no terminal and no agent to attach to.
#[derive(Clone, Debug, PartialEq)]
pub enum ShellEffect {
    Quit,
    Attach(AgentId),
    Dispatch(Command),
    NoteAttached(AgentId),
    WriteClipboard(String),
    Create { host: Option<HostId> },
}

/// Chrome configuration the view does not carry: the palette every frame
/// is built with. Resolved once at the terminal boundary, so a replay
/// restores it rather than re-detecting it.
#[derive(Clone, Copy, Debug)]
pub struct ChromeConfig {
    pub theme: Theme,
}

/// The chrome's own state plus the config it was opened with.
pub struct Chrome {
    pub view: ViewState,
    config: ChromeConfig,
    /// Lines produced by the most recent [`TraceEvent::Draw`], waiting for
    /// the caller to paint them.
    frame: Option<Vec<Line<'static>>>,
    dirty: bool,
}

impl Chrome {
    /// A chrome that owes its first paint: nothing has been drawn yet, so
    /// the caller's first loop turn draws unconditionally.
    pub fn new(view: ViewState, config: ChromeConfig) -> Self {
        Self {
            view,
            config,
            frame: None,
            dirty: true,
        }
    }

    pub fn theme(&self) -> Theme {
        self.config.theme
    }

    /// The frame environment for a draw. Both the live loop and a replay
    /// build it here so neither can drift into a different palette.
    pub fn frame_context(&self, viewport: (u16, u16), now: DateTime<Utc>) -> FrameContext {
        FrameContext {
            viewport,
            theme: self.config.theme,
            now,
        }
    }

    /// Whether a repaint is owed, clearing the flag. The chrome tracks it
    /// because only the chrome knows which steps changed the screen — a
    /// wheel event clamped at the top of a feed changes nothing.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// The lines the last [`TraceEvent::Draw`] built, if that draw has not
    /// been painted yet.
    pub fn take_frame(&mut self) -> Option<Vec<Line<'static>>> {
        self.frame.take()
    }

    /// Whether a quit guard — the fleet's or the open chat's — is armed.
    /// The armed guard is the only view state the clock alone can move,
    /// so the shell asks this to decide whether a tick is worth a
    /// [`TraceEvent::Tick`] at all.
    pub fn quit_guard_armed(&self) -> bool {
        self.view.quit_guard.is_armed()
            || self
                .view
                .chat
                .as_ref()
                .is_some_and(|chat| chat.quit_guard().is_armed())
    }

    /// A quit guard armed longer than its window disarms, and the warning
    /// footer it was rendering owes a repaint.
    fn expire(&mut self, now: DateTime<Utc>) -> bool {
        let mut expired = self.view.quit_guard.expire(now);
        if let Some(chat) = self.view.chat.as_mut() {
            expired |= chat.expire_quit_guard(now);
        }
        self.dirty |= expired;
        expired
    }

    /// Apply one event exactly as the live loop would.
    pub fn step(&mut self, model: &Model, event: &TraceEvent) -> Vec<ShellEffect> {
        match event {
            // The caller folded it; the screen is stale, nothing else.
            TraceEvent::Msg(_) => {
                self.dirty = true;
                Vec::new()
            }
            TraceEvent::Notice(notice) => {
                self.view.notice = notice.clone();
                self.dirty = true;
                Vec::new()
            }
            TraceEvent::Tick { now } => {
                self.expire(*now);
                Vec::new()
            }
            TraceEvent::Drained => {
                if let Some(chat) = self.view.chat.as_mut() {
                    chat.reconcile(model);
                }
                self.dirty = true;
                Vec::new()
            }
            TraceEvent::ChatOpened {
                agent,
                codex_configuration,
            } => {
                self.view.open_chat(model, *agent);
                if let Some(chat) = self.view.chat.as_mut() {
                    chat.set_codex_configuration_label(codex_configuration.clone());
                }
                self.dirty = true;
                vec![ShellEffect::NoteAttached(*agent)]
            }
            TraceEvent::Dispatched { op, command } => {
                // Dispatch can finish synchronously (a reducer refusal, the
                // disconnected fail-fast): reconcile now so a refused send
                // resurfaces the draft without waiting for another message.
                if let Some(chat) = self.view.chat.as_mut() {
                    chat.note_dispatched(*op, command);
                    chat.reconcile(model);
                }
                Vec::new()
            }
            TraceEvent::Draw { viewport, now } => {
                let context = self.frame_context(*viewport, *now);
                self.frame = Some(build_lines(model, &self.view, &context));
                self.dirty = false;
                Vec::new()
            }
            TraceEvent::Input {
                event,
                viewport,
                now,
            } => self.input(model, event, *viewport, *now),
        }
    }

    fn input(
        &mut self,
        model: &Model,
        event: &InputEvent,
        viewport: (u16, u16),
        now: DateTime<Utc>,
    ) -> Vec<ShellEffect> {
        match event {
            InputEvent::Key(key) => {
                self.dirty = true;
                // The chat screen, when open, owns the keys (its focus
                // derivation differs from the fleet's modes); both handlers
                // share the chrome-wide guarded Ctrl+C rule, each through
                // its own QuitGuard.
                let action = match self.view.chat.as_mut() {
                    Some(chat) => {
                        crate::chat::handle_chat_key(chat, model, key.to_event(), viewport, now)
                    }
                    None => crate::keys::handle_key(
                        &mut self.view,
                        model,
                        key.to_event(),
                        list_capacity(viewport.1),
                        now,
                    ),
                };
                self.action(model, action)
            }
            InputEvent::Paste(text) => {
                // Bracketed paste: literal insertion into the focused chat
                // text surface — newlines and tabs are text here, never
                // bindings; read-only chats and docked panels without a
                // field drop it.
                if let Some(chat) = self.view.chat.as_mut() {
                    crate::chat::handle_chat_paste(chat, model, text);
                    self.dirty = true;
                }
                Vec::new()
            }
            InputEvent::Mouse(mouse) => {
                // The fleet deliberately ignores the mouse. An open chat
                // accepts wheel motion only over its feed and reports
                // whether the shared viewport actually moved, so a clamped
                // wheel event costs no repaint.
                if let Some(chat) = self.view.chat.as_mut() {
                    self.dirty |=
                        crate::chat::handle_chat_mouse(chat, model, mouse.to_event(), viewport);
                }
                Vec::new()
            }
            InputEvent::Resize(..) => {
                self.dirty = true;
                Vec::new()
            }
        }
    }

    fn action(&mut self, model: &Model, action: Option<UiAction>) -> Vec<ShellEffect> {
        match action {
            None => Vec::new(),
            Some(UiAction::Quit) => vec![ShellEffect::Quit],
            Some(UiAction::Attach(agent)) => vec![ShellEffect::Attach(agent)],
            Some(UiAction::CopyToClipboard(text)) => vec![ShellEffect::WriteClipboard(text)],
            Some(UiAction::OpenChat(agent)) => {
                // Chat entry (A1/A3) stays inside the chrome — no terminal
                // handoff — but widens the subscription policy exactly like
                // raw attach: the reducer subscribes readonly agents'
                // streams on UserAttached, so the read-only chat's feed
                // lights up through the normal policy.
                self.view.open_chat(model, agent);
                if let Some(chat) = self.view.chat.as_mut() {
                    chat.reconcile(model);
                }
                vec![ShellEffect::NoteAttached(agent)]
            }
            Some(UiAction::Dispatch(command)) => vec![ShellEffect::Dispatch(command)],
            Some(UiAction::Create { host }) => vec![ShellEffect::Create { host }],
            Some(UiAction::CloseChat) => {
                self.view.close_chat();
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

    use super::*;

    /// Every crossterm key this build can be handed, so a mirror that
    /// forgets a variant fails here rather than in a recording nobody can
    /// replay.
    fn every_key_code() -> Vec<KeyCode> {
        let mut codes = vec![
            KeyCode::Backspace,
            KeyCode::Enter,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Delete,
            KeyCode::Insert,
            KeyCode::F(12),
            KeyCode::Char('é'),
            KeyCode::Null,
            KeyCode::Esc,
            KeyCode::CapsLock,
            KeyCode::ScrollLock,
            KeyCode::NumLock,
            KeyCode::PrintScreen,
            KeyCode::Pause,
            KeyCode::Menu,
            KeyCode::KeypadBegin,
        ];
        codes.extend(
            [
                MediaKeyCode::Play,
                MediaKeyCode::Pause,
                MediaKeyCode::PlayPause,
                MediaKeyCode::Reverse,
                MediaKeyCode::Stop,
                MediaKeyCode::FastForward,
                MediaKeyCode::Rewind,
                MediaKeyCode::TrackNext,
                MediaKeyCode::TrackPrevious,
                MediaKeyCode::Record,
                MediaKeyCode::LowerVolume,
                MediaKeyCode::RaiseVolume,
                MediaKeyCode::MuteVolume,
            ]
            .map(KeyCode::Media),
        );
        codes.extend(
            [
                ModifierKeyCode::LeftShift,
                ModifierKeyCode::LeftControl,
                ModifierKeyCode::LeftAlt,
                ModifierKeyCode::LeftSuper,
                ModifierKeyCode::LeftHyper,
                ModifierKeyCode::LeftMeta,
                ModifierKeyCode::RightShift,
                ModifierKeyCode::RightControl,
                ModifierKeyCode::RightAlt,
                ModifierKeyCode::RightSuper,
                ModifierKeyCode::RightHyper,
                ModifierKeyCode::RightMeta,
                ModifierKeyCode::IsoLevel3Shift,
                ModifierKeyCode::IsoLevel5Shift,
            ]
            .map(KeyCode::Modifier),
        );
        codes
    }

    #[test]
    fn key_events_survive_the_mirror_and_json() {
        for code in every_key_code() {
            for modifiers in [
                KeyModifiers::NONE,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER,
            ] {
                for kind in [
                    KeyEventKind::Press,
                    KeyEventKind::Repeat,
                    KeyEventKind::Release,
                ] {
                    let key = KeyEvent {
                        code,
                        modifiers,
                        kind,
                        state: KeyEventState::KEYPAD | KeyEventState::CAPS_LOCK,
                    };
                    let record = KeyRecord::from_event(key);
                    assert_eq!(
                        record.to_event(),
                        key,
                        "{key:?} lost something in the mirror"
                    );
                    let json = serde_json::to_string(&record).expect("key record serializes");
                    let back: KeyRecord =
                        serde_json::from_str(&json).expect("key record deserializes");
                    assert_eq!(back.to_event(), key, "{key:?} lost something in {json}");
                }
            }
        }
    }

    #[test]
    fn mouse_events_survive_the_mirror_and_json() {
        let kinds = [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Right),
            MouseEventKind::Drag(MouseButton::Middle),
            MouseEventKind::Moved,
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ];
        for kind in kinds {
            let mouse = MouseEvent {
                kind,
                column: 41,
                row: 7,
                modifiers: KeyModifiers::CONTROL,
            };
            let record = MouseRecord::from_event(mouse);
            assert_eq!(record.to_event(), mouse);
            let json = serde_json::to_string(&record).expect("mouse record serializes");
            let back: MouseRecord = serde_json::from_str(&json).expect("mouse deserializes");
            assert_eq!(back.to_event(), mouse, "{mouse:?} lost something in {json}");
        }
    }

    fn chrome_for(state: crate::fixtures::NamedState) -> (amux_ui::Model, Chrome) {
        let built = crate::fixtures::fixture(state);
        let chrome = Chrome::new(
            built.view,
            ChromeConfig {
                theme: Theme::default(),
            },
        );
        (built.model, chrome)
    }

    fn press(code: KeyCode) -> TraceEvent {
        TraceEvent::Input {
            event: InputEvent::Key(KeyRecord::from_event(KeyEvent::new(
                code,
                KeyModifiers::NONE,
            ))),
            viewport: (120, 40),
            now: Utc::now(),
        }
    }

    #[test]
    fn a_key_reaches_the_open_chat_through_step() {
        let (model, mut chrome) = chrome_for(crate::fixtures::NamedState::ClaudeIdle);
        assert!(chrome.step(&model, &press(KeyCode::Char('h'))).is_empty());
        assert!(chrome.step(&model, &press(KeyCode::Char('i'))).is_empty());
        assert_eq!(
            chrome
                .view
                .chat
                .as_mut()
                .expect("Claude chat open")
                .composer_mut()
                .text(),
            "hi"
        );
    }

    #[test]
    fn a_fleet_key_leaves_as_a_shell_effect() {
        let (model, mut chrome) = chrome_for(crate::fixtures::NamedState::Fleet);
        assert_eq!(
            chrome.step(&model, &press(KeyCode::Char('q'))),
            vec![ShellEffect::Quit],
            "the shell owns quitting; the chrome only asks"
        );
    }

    #[test]
    fn a_draw_settles_the_repaint_debt_and_a_key_reopens_it() {
        let (model, mut chrome) = chrome_for(crate::fixtures::NamedState::ClaudeIdle);
        assert!(chrome.take_dirty(), "a fresh chrome owes its first paint");
        chrome.step(
            &model,
            &TraceEvent::Draw {
                viewport: (120, 40),
                now: Utc::now(),
            },
        );
        assert!(chrome.take_frame().is_some(), "the draw built its lines");
        assert!(!chrome.take_dirty(), "and nothing is owed after it");
        chrome.step(&model, &press(KeyCode::Char('x')));
        assert!(chrome.take_dirty(), "a keypress owes a repaint");
    }

    #[test]
    fn focus_events_are_not_recorded() {
        assert!(InputEvent::from_terminal(&Event::FocusGained).is_none());
        assert!(InputEvent::from_terminal(&Event::FocusLost).is_none());
        assert_eq!(
            InputEvent::from_terminal(&Event::Resize(80, 24)),
            Some(InputEvent::Resize(80, 24))
        );
    }
}
