//! Provider adapter over whatsapp-rust.
// The protocol library's futures nest deeply; it sets the same limit.
#![recursion_limit = "512"]

/// Builds a message of the protocol's generated types, written as a struct
/// literal: `proto!(wa::Message { conversation: Some(text) })`. Those types
/// are non-exhaustive, so a literal cannot make them: this starts from their
/// default, or from the value after `..`, and sets the fields it names.
macro_rules! proto {
    (@value $field:ident) => { $field };
    (@value $field:ident $value:expr) => { $value };
    ($ty:path { $($field:ident $(: $value:expr)?),+ , ..$base:expr $(,)? }) => {{
        let mut message: $ty = $base;
        $(message.$field = proto!(@value $field $($value)?);)+
        message
    }};
    ($ty:path { $($field:ident $(: $value:expr)?),+ $(,)? }) => {{
        let mut message: $ty = ::core::default::Default::default();
        $(message.$field = proto!(@value $field $($value)?);)+
        message
    }};
}

pub mod archive;
pub mod call_audio;
pub mod call_video;
pub mod camera;
pub mod convert;
pub mod ids;
mod ingest;
pub mod live;
pub mod mapping;
mod provider;
pub mod session;

pub use provider::NativeProvider;

#[cfg(test)]
mod tests;
