//! Signing in to wuapi from inside the application.
//!
//! Two pieces, both free of UI:
//!
//! * [`LoginMachine`], the state of one sign-in (RFC 8628 device flow): it
//!   decides what each answer of the API means, how long to wait before
//!   asking again and when the code has run out. Pure, so it is tested
//!   without a window, a clock or a network.
//! * [`LoginFlow`], what the sign-in screen needs from the outside world:
//!   ask for a code, ask whether it was approved, open the session, forget
//!   the key. The real one is in `providers.rs`; the tests use a scripted
//!   one.
//!
//! The screen itself is `ui/login.rs`.

use client_core::SyncEngine;
use provider_wuapi::{DeviceCode, LoginError, PollOutcome, TokenGrant};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A future that runs to completion wherever it is awaited.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// What RFC 8628 adds to the polling interval on `slow_down`.
const SLOW_DOWN: Duration = Duration::from_secs(5);

/// Who the session is signed in as, for the settings screen. Never the
/// API key itself: only its public prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The organization the key belongs to.
    pub organization: String,
    /// The project the key is scoped to, when it is.
    pub project: Option<String>,
    /// The key's public prefix (`wu_live_9c9c`).
    pub key_prefix: Option<String>,
}

/// Looks the identity up, off the UI thread. Asked when the settings
/// screen opens, not before.
pub type IdentityLoader = Arc<dyn Fn() -> BoxFuture<Result<Identity, String>> + Send + Sync>;

/// A signed-in session, ready for the chat list.
pub struct Session {
    /// The engine, already started.
    pub engine: SyncEngine,
    /// Who signed in.
    pub identity: Option<IdentityLoader>,
    /// Whether the API key is in the OS keychain for the next start.
    pub key_saved: KeySaved,
    /// What to say about where the chats are kept, when they are not
    /// being saved to disk.
    pub storage_note: Option<String>,
}

/// What became of the API key after signing in. The session works in every
/// case; this decides whether the next start asks to sign in again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeySaved {
    /// It is in the OS keychain.
    Yes,
    /// The keychain refused it, with its reason. Said on screen.
    No(String),
    /// `--no-keychain`: nobody tried, on request.
    NotAsked,
}

/// What the sign-in screen needs from outside. Every future does its work
/// off the UI thread and is bounded.
pub trait LoginFlow: Send + Sync + 'static {
    /// Asks the API for a code to show.
    fn request_code(&self) -> BoxFuture<Result<DeviceCode, LoginError>>;
    /// Asks once whether the code was approved.
    fn poll(&self, code: &DeviceCode) -> BoxFuture<Result<PollOutcome, LoginError>>;
    /// Saves the key, loads the accounts and starts the session.
    fn open_session(&self, grant: TokenGrant) -> BoxFuture<Result<Session, String>>;
    /// Signs out for good: forgets the API key and deletes the chats kept
    /// on this computer (the database, its journals and its key). Settings
    /// stay. `Err` says what could not be removed.
    fn sign_out(&self) -> BoxFuture<Result<(), String>>;
    /// Forgets an API key the provider no longer accepts, and nothing
    /// else: the local data stays until someone signs in again.
    fn forget_key(&self) -> BoxFuture<Result<(), String>>;
}

/// Where a sign-in stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Nothing asked yet: the "Connect" action.
    Start,
    /// Asking the API for a code.
    Requesting,
    /// Showing the code and asking whether it was approved.
    Waiting {
        /// The code on screen.
        code: DeviceCode,
        /// The last question did not get through. Not an error: the next
        /// one is asked all the same.
        unreachable: bool,
    },
    /// Approved in the browser; the session is being opened.
    Approved {
        /// The organization that approved.
        organization: String,
    },
    /// Signed in, but the key could not be saved for the next start.
    KeyNotSaved {
        /// Why, in the keychain's words.
        reason: String,
    },
    /// Nobody approved the code in time.
    Expired,
    /// Somebody pressed "deny" in the browser.
    Denied,
    /// The API could not be reached to get a code.
    Offline,
    /// Something unexpected. Shown with the reason.
    Failed {
        /// What went wrong.
        reason: String,
    },
}

/// The state machine of one sign-in.
#[derive(Clone, Debug)]
pub struct LoginMachine {
    phase: Phase,
    interval: Duration,
    deadline: Option<Instant>,
}

impl Default for LoginMachine {
    fn default() -> Self {
        Self {
            phase: Phase::Start,
            interval: Duration::from_secs(5),
            deadline: None,
        }
    }
}

impl LoginMachine {
    /// Where the sign-in stands.
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Back to the start.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// A code is being requested.
    pub fn begin(&mut self) {
        self.reset();
        self.phase = Phase::Requesting;
    }

    /// The API issued a code: show it and start asking, at the pace and
    /// for as long as the server said.
    pub fn code_issued(&mut self, code: DeviceCode, now: Instant) {
        self.interval = Duration::from_secs(code.interval.max(1));
        self.deadline = Some(now + Duration::from_secs(code.expires_in));
        self.phase = Phase::Waiting {
            code,
            unreachable: false,
        };
    }

    /// No code: the API could not be reached (try again) or it refused.
    pub fn code_refused(&mut self, error: LoginError) {
        self.phase = match error {
            LoginError::Network(_) => Phase::Offline,
            other => Phase::Failed {
                reason: other.to_string(),
            },
        };
    }

    /// How long to wait before the next question, or `None` when there is
    /// nothing left to ask (the code ran out, or the sign-in ended).
    pub fn next_poll(&mut self, now: Instant) -> Option<Duration> {
        self.tick(now);
        match (&self.phase, self.deadline) {
            (Phase::Waiting { .. }, Some(deadline)) => {
                Some(self.interval.min(deadline.saturating_duration_since(now)))
            }
            _ => None,
        }
    }

    /// The answer to one question. Returns the grant when the sign-in was
    /// approved; the caller then opens the session.
    ///
    /// A question that did not get through never ends the sign-in: only
    /// the server's "expired", "denied" or a real refusal do.
    pub fn polled(
        &mut self,
        answer: Result<PollOutcome, LoginError>,
        now: Instant,
    ) -> Option<TokenGrant> {
        let Phase::Waiting { unreachable, .. } = &mut self.phase else {
            // Cancelled or expired while the question was on its way.
            return None;
        };
        match answer {
            Ok(PollOutcome::Granted(grant)) => {
                self.phase = Phase::Approved {
                    organization: grant.organization.name.clone(),
                };
                return Some(grant);
            }
            Ok(PollOutcome::Pending) => *unreachable = false,
            Ok(PollOutcome::SlowDown) => {
                *unreachable = false;
                self.interval += SLOW_DOWN;
            }
            Ok(PollOutcome::Unreachable(_)) | Err(LoginError::Network(_)) => *unreachable = true,
            Err(LoginError::Expired | LoginError::InvalidGrant) => self.phase = Phase::Expired,
            Err(LoginError::Denied) => self.phase = Phase::Denied,
            Err(error @ LoginError::Unexpected(_)) => {
                self.phase = Phase::Failed {
                    reason: error.to_string(),
                }
            }
        }
        self.tick(now);
        None
    }

    /// Lets time pass: a code nobody approved in time has expired.
    pub fn tick(&mut self, now: Instant) {
        if matches!(self.phase, Phase::Waiting { .. })
            && self.deadline.is_some_and(|deadline| now >= deadline)
        {
            self.phase = Phase::Expired;
        }
    }

    /// Time left to approve the code on screen.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        match (&self.phase, self.deadline) {
            (Phase::Waiting { .. }, Some(deadline)) => {
                Some(deadline.saturating_duration_since(now))
            }
            _ => None,
        }
    }

    /// The session opened but the key is not in the keychain.
    pub fn key_not_saved(&mut self, reason: String) {
        self.phase = Phase::KeyNotSaved { reason };
    }

    /// The session could not be opened after the approval.
    pub fn failed(&mut self, reason: String) {
        self.phase = Phase::Failed { reason };
    }
}

/// `m:ss`, for the countdown.
pub fn countdown(remaining: Duration) -> String {
    let seconds = remaining.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use provider_wuapi::{ApiKey, Named};

    pub(crate) fn code(expires_in: u64, interval: u64) -> DeviceCode {
        DeviceCode {
            device_code: "secret-device-code".into(),
            user_code: "WXYZ-1234".into(),
            verification_uri: "https://wuapi.dev/cli".into(),
            verification_uri_complete: "https://wuapi.dev/cli?code=WXYZ-1234".into(),
            expires_in,
            interval,
        }
    }

    pub(crate) fn grant() -> TokenGrant {
        TokenGrant {
            api_key: ApiKey::new("wu_live_0123456789abcdef0123456789abcdef"),
            key_prefix: Some("wu_live_0123".into()),
            organization: Named {
                id: "org_1".into(),
                name: "Acme Labs".into(),
            },
            project: None,
        }
    }

    fn waiting(expires_in: u64, interval: u64) -> (LoginMachine, Instant) {
        let now = Instant::now();
        let mut machine = LoginMachine::default();
        machine.begin();
        assert_eq!(machine.phase(), &Phase::Requesting);
        machine.code_issued(code(expires_in, interval), now);
        (machine, now)
    }

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn follows_the_servers_interval_and_expiry() {
        let (mut machine, start) = waiting(600, 5);
        assert!(matches!(machine.phase(), Phase::Waiting { .. }));
        assert_eq!(machine.next_poll(start), Some(5 * SECOND));
        assert_eq!(machine.remaining(start + 60 * SECOND), Some(540 * SECOND));

        // `slow_down` adds five seconds, for good.
        assert_eq!(machine.polled(Ok(PollOutcome::SlowDown), start), None);
        assert_eq!(machine.next_poll(start), Some(10 * SECOND));
        assert_eq!(machine.polled(Ok(PollOutcome::Pending), start), None);
        assert_eq!(machine.next_poll(start), Some(10 * SECOND));

        // Near the end the wait is cut to what is left, and then it is over.
        assert_eq!(machine.next_poll(start + 596 * SECOND), Some(4 * SECOND));
        assert_eq!(machine.next_poll(start + 600 * SECOND), None);
        assert_eq!(machine.phase(), &Phase::Expired);
        assert_eq!(machine.remaining(start + 600 * SECOND), None);
    }

    #[test]
    fn a_server_without_an_interval_is_not_hammered() {
        let (mut machine, start) = waiting(600, 0);
        assert_eq!(machine.next_poll(start), Some(SECOND));
    }

    #[test]
    fn a_question_that_does_not_get_through_is_not_an_error() {
        let (mut machine, start) = waiting(600, 5);
        for answer in [
            Ok(PollOutcome::Unreachable("could not connect".into())),
            Err(LoginError::Network("the request timed out".into())),
        ] {
            assert_eq!(machine.polled(answer, start), None);
            assert!(matches!(
                machine.phase(),
                Phase::Waiting {
                    unreachable: true,
                    ..
                }
            ));
            assert_eq!(machine.next_poll(start), Some(5 * SECOND), "still asking");
        }
        // The network is back: the note goes away, the code stays.
        machine.polled(Ok(PollOutcome::Pending), start);
        assert!(matches!(
            machine.phase(),
            Phase::Waiting {
                unreachable: false,
                ..
            }
        ));
        // And an approval after an outage is an approval.
        let granted = machine.polled(Ok(PollOutcome::Granted(grant())), start);
        assert_eq!(granted, Some(grant()));
        assert_eq!(
            machine.phase(),
            &Phase::Approved {
                organization: "Acme Labs".into()
            }
        );
        assert_eq!(machine.next_poll(start), None, "nothing left to ask");
    }

    #[test]
    fn the_servers_verdicts_end_the_sign_in() {
        for (answer, phase) in [
            (LoginError::Expired, Phase::Expired),
            (LoginError::InvalidGrant, Phase::Expired),
            (LoginError::Denied, Phase::Denied),
        ] {
            let (mut machine, start) = waiting(600, 5);
            assert_eq!(machine.polled(Err(answer), start), None);
            assert_eq!(machine.phase(), &phase);
            assert_eq!(machine.next_poll(start), None);
        }
        let (mut machine, start) = waiting(600, 5);
        machine.polled(Err(LoginError::Unexpected("HTTP 418".into())), start);
        assert!(matches!(machine.phase(), Phase::Failed { reason } if reason.contains("418")));
    }

    #[test]
    fn an_answer_that_arrives_after_the_end_is_ignored() {
        let (mut machine, start) = waiting(60, 5);
        machine.tick(start + 61 * SECOND);
        assert_eq!(machine.phase(), &Phase::Expired);
        assert_eq!(machine.polled(Ok(PollOutcome::Pending), start), None);
        assert_eq!(machine.phase(), &Phase::Expired);

        machine.reset();
        assert_eq!(machine.phase(), &Phase::Start);
        assert_eq!(machine.polled(Err(LoginError::Denied), start), None);
        assert_eq!(machine.phase(), &Phase::Start);
    }

    #[test]
    fn not_getting_a_code_is_offline_or_a_refusal() {
        let mut machine = LoginMachine::default();
        machine.begin();
        machine.code_refused(LoginError::Network("could not connect".into()));
        assert_eq!(machine.phase(), &Phase::Offline);

        machine.begin();
        machine.code_refused(LoginError::Unexpected("HTTP 403".into()));
        assert!(matches!(machine.phase(), Phase::Failed { .. }));
    }

    #[test]
    fn the_countdown_reads_as_minutes_and_seconds() {
        assert_eq!(countdown(Duration::from_secs(600)), "10:00");
        assert_eq!(countdown(Duration::from_millis(61_900)), "1:01");
        assert_eq!(countdown(Duration::ZERO), "0:00");
    }
}
