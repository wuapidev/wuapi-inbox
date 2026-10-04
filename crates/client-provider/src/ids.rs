//! Identifier newtypes.
//!
//! Every id is an opaque string chosen by the provider. The client never
//! parses them; it only stores them and hands them back. Keeping one type per
//! kind of id makes it a compile error to pass a chat id where a message id is
//! expected.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wraps a raw id.
            pub fn new(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            /// The raw id, exactly as the provider issued it.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Unwraps the raw id.
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(raw: &str) -> Self {
                Self(raw.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(raw: String) -> Self {
                Self(raw)
            }
        }
    };
}

id_type!(
    /// One linked WhatsApp number (a "device" in WhatsApp's terms).
    ///
    /// Must be unique across every account the provider returns and stable
    /// across restarts: the local store keys everything by it.
    AccountId
);

id_type!(
    /// A conversation inside an account: a 1:1 chat or a group.
    ///
    /// Unique within its account, stable across restarts.
    ChatId
);

id_type!(
    /// A message, as named by the provider.
    ///
    /// Unique within its account, stable across restarts. Not to be confused
    /// with [`ClientMessageId`], which the client generates before the
    /// provider has seen the message.
    MessageId
);

id_type!(
    /// A person: the sender of a message or a group participant.
    ///
    /// Typically a phone number in E.164 or a provider-specific handle.
    ContactId
);

id_type!(
    /// The id the client generates for a message *before* sending it.
    ///
    /// It is the idempotency key of [`Provider::send`](crate::Provider::send):
    /// the same `ClientMessageId` may be submitted any number of times and
    /// must produce at most one WhatsApp message. The client persists it in
    /// its outbox, so it survives restarts.
    ClientMessageId
);

id_type!(
    /// An opaque position in a paged listing.
    ///
    /// Returned by the provider in [`Page::next_cursor`](crate::Page) and
    /// passed back unchanged to get the following page. The client never
    /// inspects it.
    Cursor
);

id_type!(
    /// An opaque handle to a media payload.
    ///
    /// Whatever the provider needs to fetch the bytes later through
    /// [`Provider::download_media`](crate::Provider::download_media): a URL, a
    /// storage key, a serialised media descriptor.
    MediaRef
);
