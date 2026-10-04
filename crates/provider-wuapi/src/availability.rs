//! What the API does not have yet, for this deployment or for one number.
//!
//! The routes for forwarding, favorite stickers and contacts' stories came
//! with SDK 0.12.0. A backend that was not deployed since answers
//! `404 not_found` ("Route not found.") to them, and an engine that has
//! favorite stickers turned off for a number answers `400 not_supported`
//! to a change of them. Neither is a failure of anything the user did: the
//! part is "not available yet". It is remembered here for [`RECHECK`], said
//! through [`Provider::unavailable`](client_provider::Provider::unavailable),
//! and asked about again after that. Calls made meanwhile answer
//! [`ProviderError::Unsupported`](client_provider::ProviderError::Unsupported)
//! without a request, so a missing part costs one request every ten
//! minutes and never touches what works.

use client_provider::Feature;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long "not available" is believed before it is asked again.
pub(crate) const RECHECK: Duration = Duration::from_secs(10 * 60);

/// The message of the API's answer to a route it does not have. A
/// resource that does not exist answers 404 `not_found` too, with other
/// words: only this one says the deployment lacks the route.
const NO_ROUTE: &str = "Route not found";

/// What is known to be missing, and since when.
#[derive(Default)]
pub(crate) struct Missing {
    /// Keyed by the feature and the account it is missing for; `None` is
    /// the whole deployment.
    marks: Mutex<HashMap<(Feature, Option<String>), Instant>>,
}

impl Missing {
    fn marks(&self) -> std::sync::MutexGuard<'_, HashMap<(Feature, Option<String>), Instant>> {
        self.marks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The deployment does not have the routes of `feature`.
    pub(crate) fn no_route(&self, feature: Feature) {
        if self
            .marks()
            .insert((feature, None), Instant::now())
            .is_none()
        {
            tracing::warn!(
                ?feature,
                "this wuapi deployment does not have these routes yet; asking again later"
            );
        }
    }

    /// `feature` is not turned on for `account`.
    pub(crate) fn off_for(&self, feature: Feature, account: &str) {
        if self
            .marks()
            .insert((feature, Some(account.to_owned())), Instant::now())
            .is_none()
        {
            tracing::info!(
                ?feature,
                "not turned on for this number yet; asking again later"
            );
        }
    }

    /// `feature` answered for `account`: it is there.
    pub(crate) fn works(&self, feature: Feature, account: &str) {
        let mut marks = self.marks();
        marks.remove(&(feature, None));
        marks.remove(&(feature, Some(account.to_owned())));
    }

    /// The user asked for `feature` again for `account`: what was
    /// believed of it is forgotten, so the next call asks the API instead
    /// of answering from memory.
    pub(crate) fn forget(&self, feature: Feature, account: &str) {
        let mut marks = self.marks();
        marks.remove(&(feature, None));
        marks.remove(&(feature, Some(account.to_owned())));
    }

    /// Whether the deployment is known to lack the routes of `feature`.
    pub(crate) fn lacks_route(&self, feature: Feature) -> bool {
        let mut marks = self.marks();
        marks.retain(|_, at| at.elapsed() < RECHECK);
        marks.contains_key(&(feature, None))
    }

    /// Whether `feature` is known to be missing for `account` right now.
    pub(crate) fn is(&self, feature: Feature, account: &str) -> bool {
        let mut marks = self.marks();
        marks.retain(|_, at| at.elapsed() < RECHECK);
        marks.contains_key(&(feature, None))
            || marks.contains_key(&(feature, Some(account.to_owned())))
    }

    /// Makes every mark as old as `age`, so a test can let time pass.
    #[cfg(test)]
    pub(crate) fn age(&self, age: Duration) {
        let then = Instant::now().checked_sub(age).expect("a clock that old");
        for at in self.marks().values_mut() {
            *at = then;
        }
    }
}

/// The API has no such route: an older deployment.
pub(crate) fn no_route(error: &wuapi::Error) -> bool {
    matches!(error, wuapi::Error::Api { status: 404, code, message, .. }
        if code == "not_found" && message.contains(NO_ROUTE))
}

/// WhatsApp, or the engine, does not do this for the account.
pub(crate) fn not_supported(error: &wuapi::Error) -> bool {
    matches!(error, wuapi::Error::Api { code, .. } if code == "not_supported")
}
