#![allow(dead_code)]

pub(crate) mod pin;
pub(crate) mod qr;
pub(crate) mod ssh;

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ring::rand::{SecureRandom as _, SystemRandom};

use crate::audit;

const TOKEN_LEN: usize = 32;
pub(crate) const PAIR_MODE_TTL: Duration = Duration::from_secs(5 * 60);
pub(crate) const PIN_FAILED_ATTEMPT_LIMIT: u8 = 5;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum PairModeError {
    #[error("pairing mode is already active")]
    AlreadyActive,
    #[error("pairing mode is not active")]
    NotActive,
    #[error("active pairing mode is not token-based")]
    NotTokenMode,
    #[error("active pairing mode is not PIN-based")]
    NotPinMode,
    #[error("one-shot token is invalid")]
    InvalidToken,
    #[error("PIN must be six decimal digits")]
    InvalidPinFormat,
    #[error("failed to generate pairing secret")]
    SecretGeneration,
}

#[derive(Debug)]
enum PairSecret {
    Token {
        value: [u8; TOKEN_LEN],
        reserved: bool,
    },
    Pin {
        value: String,
        failed_attempts: u8,
        in_flight_attempts: u8,
        reserved: bool,
    },
}

#[derive(Debug)]
struct PairModeSession {
    id: u64,
    secret: PairSecret,
    expires_at: Instant,
}

#[derive(Debug, Default)]
pub(crate) struct PairMode {
    state: Arc<Mutex<PairModeState>>,
}

#[derive(Debug, Default)]
struct PairModeState {
    next_session_id: u64,
    session: Option<PairModeSession>,
}

pub(crate) struct PairModePinAttempt {
    state: Arc<Mutex<PairModeState>>,
    session_id: u64,
    pin: String,
    active: bool,
}

pub(crate) struct PairModeTokenAttempt {
    state: Arc<Mutex<PairModeState>>,
    session_id: u64,
    active: bool,
}

pub(crate) struct PairModePinCommit {
    state: Arc<Mutex<PairModeState>>,
    session_id: u64,
    active: bool,
}

impl PairMode {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn is_active(&self) -> bool {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        purge_expired(&mut state);
        state.session.is_some()
    }

    pub(crate) fn start_token(&self) -> Result<[u8; TOKEN_LEN], PairModeError> {
        let mut token = [0_u8; TOKEN_LEN];
        SystemRandom::new()
            .fill(&mut token)
            .map_err(|_| PairModeError::SecretGeneration)?;
        self.start_token_for_duration(token, PAIR_MODE_TTL)?;
        Ok(token)
    }

    pub(crate) fn start_pin(&self) -> Result<String, PairModeError> {
        let pin = generate_pin()?;
        self.start_pin_for_duration(pin.clone(), PAIR_MODE_TTL)?;
        Ok(pin)
    }

    pub(crate) fn start_token_for_duration(
        &self,
        token: [u8; TOKEN_LEN],
        ttl: Duration,
    ) -> Result<(), PairModeError> {
        self.start_session(
            PairSecret::Token {
                value: token,
                reserved: false,
            },
            ttl,
        )
    }

    pub(crate) fn start_pin_for_duration(
        &self,
        pin: String,
        ttl: Duration,
    ) -> Result<(), PairModeError> {
        validate_pin(&pin)?;
        self.start_session(
            PairSecret::Pin {
                value: pin,
                failed_attempts: 0,
                in_flight_attempts: 0,
                reserved: false,
            },
            ttl,
        )
    }

    pub(crate) fn begin_token_attempt(
        &self,
        token: &[u8],
    ) -> Result<PairModeTokenAttempt, PairModeError> {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        purge_expired(&mut state);
        let Some(session) = state.session.as_mut() else {
            return Err(PairModeError::NotActive);
        };
        match &mut session.secret {
            PairSecret::Token { value, reserved } if token_matches(value, token) && !*reserved => {
                *reserved = true;
                Ok(PairModeTokenAttempt {
                    state: self.state.clone(),
                    session_id: session.id,
                    active: true,
                })
            }
            PairSecret::Token { .. } => Err(PairModeError::InvalidToken),
            PairSecret::Pin { .. } => Err(PairModeError::NotTokenMode),
        }
    }

    pub(crate) fn complete_token_success(
        &self,
        attempt: &mut PairModeTokenAttempt,
    ) -> Result<(), PairModeError> {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        let Some(session) = state.session.as_ref() else {
            return Err(PairModeError::NotActive);
        };
        if session.id != attempt.session_id {
            return Err(PairModeError::NotActive);
        }
        match &session.secret {
            PairSecret::Token { reserved: true, .. } => {
                state.session = None;
                attempt.active = false;
                Ok(())
            }
            PairSecret::Token {
                reserved: false, ..
            } => Err(PairModeError::InvalidToken),
            PairSecret::Pin { .. } => Err(PairModeError::NotTokenMode),
        }
    }

    pub(crate) fn abort_token_attempt(&self, attempt: &mut PairModeTokenAttempt) {
        attempt.abort();
    }

    pub(crate) fn begin_pin_attempt(&self) -> Result<PairModePinAttempt, PairModeError> {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        purge_expired(&mut state);
        let Some(session) = state.session.as_mut() else {
            return Err(PairModeError::NotActive);
        };
        match &mut session.secret {
            PairSecret::Pin {
                value,
                failed_attempts,
                in_flight_attempts,
                reserved: false,
            } => {
                if failed_attempts.saturating_add(*in_flight_attempts) >= PIN_FAILED_ATTEMPT_LIMIT {
                    return Err(PairModeError::NotActive);
                }
                *in_flight_attempts = in_flight_attempts.saturating_add(1);
                Ok(PairModePinAttempt {
                    state: self.state.clone(),
                    session_id: session.id,
                    pin: value.clone(),
                    active: true,
                })
            }
            PairSecret::Pin { reserved: true, .. } => Err(PairModeError::NotActive),
            PairSecret::Token { .. } => Err(PairModeError::NotPinMode),
        }
    }

    pub(crate) fn record_pin_failure(
        &self,
        attempt: &mut PairModePinAttempt,
    ) -> Result<(), PairModeError> {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        purge_expired(&mut state);
        if !attempt.active {
            return Err(PairModeError::NotActive);
        }
        let Some(session) = state.session.as_mut() else {
            return Err(PairModeError::NotActive);
        };
        if session.id != attempt.session_id {
            return Err(PairModeError::NotActive);
        }
        match &mut session.secret {
            PairSecret::Pin {
                failed_attempts,
                in_flight_attempts,
                reserved: false,
                ..
            } => {
                *in_flight_attempts = in_flight_attempts.saturating_sub(1);
                *failed_attempts = failed_attempts.saturating_add(1);
                if *failed_attempts >= PIN_FAILED_ATTEMPT_LIMIT {
                    state.session = None;
                }
                attempt.active = false;
                Ok(())
            }
            PairSecret::Pin { reserved: true, .. } => Err(PairModeError::NotActive),
            PairSecret::Token { .. } => Err(PairModeError::NotPinMode),
        }
    }

    pub(crate) fn begin_pin_commit(
        &self,
        attempt: &mut PairModePinAttempt,
    ) -> Result<PairModePinCommit, PairModeError> {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        purge_expired(&mut state);
        if !attempt.active {
            return Err(PairModeError::NotActive);
        }
        let Some(session) = state.session.as_mut() else {
            return Err(PairModeError::NotActive);
        };
        if session.id != attempt.session_id {
            return Err(PairModeError::NotActive);
        }
        match &mut session.secret {
            PairSecret::Pin {
                in_flight_attempts,
                reserved: false,
                ..
            } => {
                *in_flight_attempts = in_flight_attempts.saturating_sub(1);
                if let PairSecret::Pin { reserved, .. } = &mut session.secret {
                    *reserved = true;
                }
                attempt.active = false;
                Ok(PairModePinCommit {
                    state: self.state.clone(),
                    session_id: attempt.session_id,
                    active: true,
                })
            }
            PairSecret::Pin { reserved: true, .. } => Err(PairModeError::NotActive),
            PairSecret::Token { .. } => Err(PairModeError::NotPinMode),
        }
    }

    pub(crate) fn complete_pin_success(
        &self,
        commit: &mut PairModePinCommit,
    ) -> Result<(), PairModeError> {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        let Some(session) = state.session.as_ref() else {
            return Err(PairModeError::NotActive);
        };
        if session.id != commit.session_id {
            return Err(PairModeError::NotActive);
        }
        match &session.secret {
            PairSecret::Pin { reserved: true, .. } => {
                state.session = None;
                commit.active = false;
                Ok(())
            }
            PairSecret::Pin {
                reserved: false, ..
            } => Err(PairModeError::NotActive),
            PairSecret::Token { .. } => Err(PairModeError::NotPinMode),
        }
    }

    pub(crate) fn abort_pin_commit(&self, commit: &mut PairModePinCommit) {
        commit.abort();
    }

    pub(crate) fn cancel(&self) -> bool {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        let was_active = state.session.is_some();
        state.session = None;
        was_active
    }

    fn start_session(&self, secret: PairSecret, ttl: Duration) -> Result<(), PairModeError> {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        purge_expired(&mut state);
        if state.session.is_some() {
            return Err(PairModeError::AlreadyActive);
        }
        let id = state.next_session_id;
        state.next_session_id = state.next_session_id.wrapping_add(1);
        state.session = Some(PairModeSession {
            id,
            secret,
            expires_at: Instant::now() + ttl,
        });
        Ok(())
    }
}

impl PairModePinAttempt {
    pub(crate) fn pin(&self) -> &str {
        &self.pin
    }

    fn abort(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        if let Some(session) = state.session.as_mut()
            && session.id == self.session_id
            && let PairSecret::Pin {
                in_flight_attempts, ..
            } = &mut session.secret
        {
            *in_flight_attempts = in_flight_attempts.saturating_sub(1);
        }
        purge_expired(&mut state);
        self.active = false;
    }
}

impl fmt::Debug for PairModeTokenAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairModeTokenAttempt")
            .field("session_id", &self.session_id)
            .field("active", &self.active)
            .finish()
    }
}

impl PartialEq for PairModeTokenAttempt {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id && self.active == other.active
    }
}

impl Eq for PairModeTokenAttempt {}

impl fmt::Debug for PairModePinAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairModePinAttempt")
            .field("session_id", &self.session_id)
            .field("active", &self.active)
            .finish()
    }
}

impl PartialEq for PairModePinAttempt {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id && self.active == other.active
    }
}

impl Eq for PairModePinAttempt {}

impl fmt::Debug for PairModePinCommit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairModePinCommit")
            .field("session_id", &self.session_id)
            .field("active", &self.active)
            .finish()
    }
}

impl PartialEq for PairModePinCommit {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id && self.active == other.active
    }
}

impl Eq for PairModePinCommit {}

impl PairModeTokenAttempt {
    fn abort(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        if let Some(session) = state.session.as_mut()
            && session.id == self.session_id
            && let PairSecret::Token { reserved, .. } = &mut session.secret
        {
            *reserved = false;
        }
        purge_expired(&mut state);
        self.active = false;
    }
}

impl PairModePinCommit {
    fn abort(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        if let Some(session) = state.session.as_mut()
            && session.id == self.session_id
            && let PairSecret::Pin { reserved, .. } = &mut session.secret
        {
            *reserved = false;
        }
        purge_expired(&mut state);
        self.active = false;
    }
}

impl Drop for PairModeTokenAttempt {
    fn drop(&mut self) {
        self.abort();
    }
}

impl Drop for PairModePinAttempt {
    fn drop(&mut self) {
        self.abort();
    }
}

impl Drop for PairModePinCommit {
    fn drop(&mut self) {
        self.abort();
    }
}

fn purge_expired(state: &mut PairModeState) {
    let Some(session) = state.session.as_ref() else {
        return;
    };
    let expired = Instant::now() >= session.expires_at
        && !matches!(
            &session.secret,
            PairSecret::Token { reserved: true, .. } | PairSecret::Pin { reserved: true, .. }
        );
    if !expired {
        return;
    }
    let method = match &session.secret {
        PairSecret::Token { .. } => "qr",
        PairSecret::Pin { .. } => "pin",
    };
    state.session = None;
    audit::pairing_failure(method, "pair mode expired");
}

fn generate_pin() -> Result<String, PairModeError> {
    let rng = SystemRandom::new();
    let mut bytes = [0_u8; 4];
    rng.fill(&mut bytes)
        .map_err(|_| PairModeError::SecretGeneration)?;
    let value = u32::from_be_bytes(bytes) % 1_000_000;
    Ok(format!("{value:06}"))
}

fn validate_pin(pin: &str) -> Result<(), PairModeError> {
    if pin.len() == 6 && pin.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(())
    } else {
        Err(PairModeError::InvalidPinFormat)
    }
}

fn token_matches(expected: &[u8; TOKEN_LEN], token: &[u8]) -> bool {
    if token.len() != TOKEN_LEN {
        return false;
    }
    let diff = expected
        .iter()
        .zip(token)
        .fold(0_u8, |diff, (left, right)| diff | (left ^ right));
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_mode_is_active_until_token_is_consumed() {
        let pair_mode = PairMode::new();
        let token = [7_u8; TOKEN_LEN];

        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();

        assert!(pair_mode.is_active());
        let mut attempt = pair_mode.begin_token_attempt(&token).unwrap();
        pair_mode.complete_token_success(&mut attempt).unwrap();
        assert!(!pair_mode.is_active());
        assert_eq!(
            pair_mode.begin_token_attempt(&token),
            Err(PairModeError::NotActive)
        );
    }

    #[test]
    fn token_mismatch_does_not_consume_pair_mode() {
        let pair_mode = PairMode::new();
        let token = [7_u8; TOKEN_LEN];

        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();

        assert_eq!(
            pair_mode.begin_token_attempt(&[8_u8; TOKEN_LEN]),
            Err(PairModeError::InvalidToken)
        );
        assert!(pair_mode.is_active());
        let mut attempt = pair_mode.begin_token_attempt(&token).unwrap();
        pair_mode.complete_token_success(&mut attempt).unwrap();
    }

    #[test]
    fn token_attempt_reservation_blocks_racing_consumers_until_aborted() {
        let pair_mode = PairMode::new();
        let token = [7_u8; TOKEN_LEN];

        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();

        let mut attempt = pair_mode.begin_token_attempt(&token).unwrap();
        assert_eq!(
            pair_mode.begin_token_attempt(&token),
            Err(PairModeError::InvalidToken)
        );
        assert!(pair_mode.is_active());

        pair_mode.abort_token_attempt(&mut attempt);
        let mut retry = pair_mode.begin_token_attempt(&token).unwrap();
        pair_mode.complete_token_success(&mut retry).unwrap();
        assert!(!pair_mode.is_active());
    }

    #[test]
    fn dropped_token_attempt_aborts_reservation() {
        let pair_mode = PairMode::new();
        let token = [7_u8; TOKEN_LEN];

        pair_mode
            .start_token_for_duration(token, Duration::from_secs(60))
            .unwrap();

        drop(pair_mode.begin_token_attempt(&token).unwrap());
        let mut retry = pair_mode.begin_token_attempt(&token).unwrap();
        pair_mode.complete_token_success(&mut retry).unwrap();
        assert!(!pair_mode.is_active());
    }

    #[test]
    fn active_pair_mode_blocks_new_secret_until_cancelled_or_expired() {
        let pair_mode = PairMode::new();

        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();

        assert_eq!(
            pair_mode.start_token_for_duration([1_u8; TOKEN_LEN], Duration::from_secs(60)),
            Err(PairModeError::AlreadyActive)
        );

        pair_mode.cancel();
        pair_mode
            .start_token_for_duration([1_u8; TOKEN_LEN], Duration::from_secs(60))
            .unwrap();
    }

    #[test]
    fn expired_pair_mode_is_not_active_and_allows_new_secret() {
        let pair_mode = PairMode::new();

        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::ZERO)
            .unwrap();

        assert!(!pair_mode.is_active());
        pair_mode
            .start_token_for_duration([1_u8; TOKEN_LEN], Duration::from_secs(60))
            .unwrap();
        assert!(pair_mode.is_active());
    }

    #[test]
    fn pin_attempt_exposes_pin_and_cancels_after_failed_attempt_cap() {
        let pair_mode = PairMode::new();

        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();

        let mut attempt = pair_mode.begin_pin_attempt().unwrap();
        assert_eq!(attempt.pin(), "123456");
        for _ in 0..PIN_FAILED_ATTEMPT_LIMIT {
            pair_mode.record_pin_failure(&mut attempt).unwrap();
            if pair_mode.is_active() {
                attempt = pair_mode.begin_pin_attempt().unwrap();
            }
        }
        assert!(!pair_mode.is_active());
    }

    #[test]
    fn pin_attempts_are_capped_including_in_flight_attempts() {
        let pair_mode = PairMode::new();

        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();

        let attempts = (0..PIN_FAILED_ATTEMPT_LIMIT)
            .map(|_| pair_mode.begin_pin_attempt().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(pair_mode.begin_pin_attempt(), Err(PairModeError::NotActive));
        drop(attempts);
        assert!(pair_mode.begin_pin_attempt().is_ok());
    }

    #[test]
    fn pin_success_consumption_is_bound_to_attempt_session() {
        let pair_mode = PairMode::new();

        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();

        let mut first = pair_mode.begin_pin_attempt().unwrap();
        let mut second = pair_mode.begin_pin_attempt().unwrap();
        assert_eq!(first.pin(), "123456");
        assert_eq!(second.pin(), "123456");

        let mut commit = pair_mode.begin_pin_commit(&mut first).unwrap();
        assert_eq!(
            pair_mode.begin_pin_commit(&mut second),
            Err(PairModeError::NotActive)
        );
        pair_mode.complete_pin_success(&mut commit).unwrap();
        assert_eq!(
            pair_mode.begin_pin_commit(&mut second),
            Err(PairModeError::NotActive)
        );
        assert_eq!(
            pair_mode.record_pin_failure(&mut second),
            Err(PairModeError::NotActive)
        );
    }

    #[test]
    fn dropped_pin_commit_aborts_reservation() {
        let pair_mode = PairMode::new();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();

        let mut first = pair_mode.begin_pin_attempt().unwrap();
        drop(pair_mode.begin_pin_commit(&mut first).unwrap());
        let mut second = pair_mode.begin_pin_attempt().unwrap();
        let mut commit = pair_mode.begin_pin_commit(&mut second).unwrap();
        pair_mode.complete_pin_success(&mut commit).unwrap();
        assert!(!pair_mode.is_active());
    }

    #[test]
    fn generated_pin_is_six_decimal_digits() {
        let pair_mode = PairMode::new();

        let pin = pair_mode.start_pin().unwrap();

        assert_eq!(pin.len(), 6);
        assert!(pin.bytes().all(|byte| byte.is_ascii_digit()));
    }
}
