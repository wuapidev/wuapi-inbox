//! A [`Provider`](client_provider::Provider) for [wuapi](https://wuapi.dev),
//! a hosted WhatsApp REST API.
//!
//! This is the reference adapter: copy it to write your own. It is laid
//! out so each concern can be read on its own.
//!
//! | Module | What it does |
//! |---|---|
//! | `client` | One method per endpoint, on top of the generated wuapi SDK (the `wuapi` crate): requests, wire types, paging, idempotency keys. |
//! | `mapping` | The SDK's types to the neutral model. Pure functions. |
//! | `error` | The SDK's errors to [`ProviderError`](client_provider::ProviderError): transient or terminal. |
//! | `http` | Plain HTTP for what the SDK does not cover: the login and media bytes. |
//! | `identity` | Who an API key belongs to. |
//! | `events` | Live updates by polling, behind the `EventSource` trait the stream implements too. |
//! | `sse` | The server-sent events parser: bytes to frames, no I/O. |
//! | `envelope` | One stream event to the neutral model: decoding, the `evt_` window, the chats to read again. |
//! | `stream` | The connection to wuapi Streams: reconnecting, resuming with `Last-Event-ID`, pacing, and the strict (`--live stream`) source. |
//! | `live` | `Auto`: the stream when the deployment has one, polling when it does not, and the safety poll beside the stream. |
//! | `social` | Profiles and groups: their calls, mapping and the wording of refusals. |
//! | `forward`, `stickers`, `stories`, `uploads` | Forwarding by naming a message, favorite stickers, stories, sending files. |
//! | `availability` | What a deployment or a number does not have yet, and when to ask again. |
//! | `compat` | Temporary: reads the answers of a backend older than the SDK. |
//! | `provider` | The `Provider` implementation tying it together. |
//! | [`login`](DeviceLogin) | Browser sign-in (device authorization flow). |
//! | `keychain` | The API key in the OS keychain. |
//!
//! # Usage
//!
//! ```no_run
//! use provider_wuapi::{load_api_key, WuapiConfig, WuapiProvider};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let key = load_api_key("my-app", "default")?.ok_or("not signed in")?;
//! let provider = WuapiProvider::new(WuapiConfig::new("my-app/0.1"), key)?;
//! # let _ = provider;
//! # Ok(())
//! # }
//! ```
//!
//! # Known gaps
//!
//! The public wuapi API was designed for servers, and a few things a chat
//! client wants are missing. The adapter says so in its
//! [`Capabilities`](client_provider::Capabilities) and marks each spot with
//! `TODO(wuapi-api)`: no `since` filter on the message listing. Events
//! arrive over wuapi Streams where the deployment has it (see
//! [`LiveTransport`]) and by polling where it does not.
//!
//! Forwarding, favorite stickers and contacts' stories came with SDK
//! 0.12.0. A deployment that does not have their routes yet, or a number
//! they are not turned on for, is "not available yet": see `availability`.

#![warn(missing_docs)]

mod availability;
mod client;
mod compat;
mod config;
mod diagnose;
mod envelope;
mod error;
mod events;
mod follow;
mod forward;
mod http;
mod identity;
mod keychain;
mod live;
mod login;
mod mapping;
mod provider;
mod social;
mod sse;
mod stickers;
mod stories;
mod stream;
mod uploads;

pub use config::{
    check_stream_url, ApiKey, LiveTransport, WuapiConfig, DEFAULT_BASE_URL, DEFAULT_STREAM_URL,
};
pub use identity::{AuthContext, KeyInfo, Named};
pub use keychain::{delete_api_key, load_api_key, store_api_key, KeychainError};
pub use login::{DeviceCode, DeviceLogin, LoginError, PollOutcome, TokenGrant};
pub use provider::WuapiProvider;

#[cfg(test)]
mod tests;
