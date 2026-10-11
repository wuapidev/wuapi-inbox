//! Provider adapter over whatsapp-rust.
// The protocol library's futures nest deeply; it sets the same limit.
#![recursion_limit = "512"]
pub mod archive;
pub mod call_audio;
pub mod call_video;
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
