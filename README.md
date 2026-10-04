# wuapi Inbox

An open-source, native desktop WhatsApp client for macOS, Windows and Linux.

It is written in Rust, with the UI in [GPUI](https://gpui.rs) (the framework behind the Zed editor), so there is no browser inside it. It is local-first: the window only ever reads from a database on your computer, and a background engine keeps that database in sync. The UI does not wait for the network to open a chat, scroll back or search.

How it reaches WhatsApp is pluggable. A *provider* is a small adapter around some backend: a hosted API, a self-hosted gateway, a protocol library. The reference adapter is for [wuapi](https://wuapi.dev); writing your own means implementing one trait. See [docs/PROVIDERS.md](docs/PROVIDERS.md).

## Status

Early. This is the first milestone: the skeleton of the whole thing, end to end.

Working today, on the built-in demo provider:

- several accounts, chat list with search and filters, 1:1 and group conversations;
- text messages, replies, reactions, delivery and read ticks, typing indicators;
- sending through a persistent outbox that survives restarts and never sends twice;
- full-text search over message history;
- light and dark themes.

- pin, mute, archive and mark as unread; starting a chat with a number; search inside a conversation;
- a settings panel (account, appearance, notification preferences, about);
- an encrypted local database (SQLCipher), with its key in the OS keychain.

- profile pictures, images and stickers inline, voice notes and audio played in place, and other attachments opened with the system's application.

- Status (stories): a Status button in the rail beside Chats, a strip of who has something new above the chats, the list of updates with a ring around each person who has one, a viewer with progress, pause, replies and reactions, posting a text, a picture or a video, who saw your status, who sees it, and muting. With wuapi (SDK 0.12.0), posting, your own status with who saw it, contacts' updates, view receipts, replies and reactions go through the API; contacts' updates reach a number once stories are turned on for it. Muting a contact's status stays on this computer and the audience is read-only: the API cannot write either yet. Where the backend does not have a part yet, that part says "not available yet" and the rest works.

Files can be attached (the "+" button, a drop on the conversation, or a paste of a picture or of copied files); with wuapi that waits for the upload routes to be deployed, and says "not available yet" until then. Not there yet: desktop notifications are not delivered (the preference is saved), and voice-note recording is shown as unavailable. The wuapi adapter is implemented and tested against a mock of the API but has not been run against the live service.

## Run it

You need a recent stable Rust (the repository pins the toolchain in `rust-toolchain.toml`). On Linux you also need the development packages for Wayland or X11, xkbcommon and Vulkan.

```sh
cargo run -p wuapi-inbox                    # wuapi: sign in, then your chats
cargo run -p wuapi-inbox -- --theme dark
cargo run -p wuapi-inbox -- --provider mock   # demo data, nothing leaves your machine
cargo run -p wuapi-inbox -- --welcome         # the welcome screen first
cargo run -p wuapi-inbox -- --live stream     # wuapi Streams only: no polling (auto and polling are the others)
```

With wuapi, the default, the first start shows a welcome and then a sign-in screen: you get a short code and approve it in the browser. The API key it receives is kept in your operating system's keychain, and so is the key of the local database. `--no-keychain` uses neither (you sign in at every start and nothing is saved to disk). Signing out deletes the key and the chats stored on the computer. Live updates come over wuapi Streams when the API has them and by polling when it does not (`--live auto`, the default); `--live stream` insists on the stream, `--live polling` never uses it, and `--stream-url` points the stream at another address. `--help` lists the other options.

Building needs `perl` and `make` as well (for the vendored OpenSSL that encrypts the database), and on Linux the ALSA development headers for audio (`alsa-lib` on Arch, `libasound2-dev` on Debian and Ubuntu).

## Layout

| Crate | What it is |
|---|---|
| [`crates/client-provider`](crates/client-provider) | The provider trait and the neutral model it speaks. The public extension point. |
| [`crates/client-core`](crates/client-core) | The local SQLite store, the outbox and the sync engine. |
| [`crates/provider-mock`](crates/provider-mock) | An in-memory provider with seeded data and fake live events, for development and tests. |
| [`crates/provider-wuapi`](crates/provider-wuapi) | The reference adapter, for wuapi. |
| [`crates/brand-mark`](crates/brand-mark) | The animated wuapi mark, as a GPUI element. |
| [`crates/app`](crates/app) | The GPUI application. |
| [`crates/updater`](crates/updater) | Self-update: a signed manifest, a resumable download, an install that is undone if the new version does not start. |
| [`crates/xtask`](crates/xtask) | Release tooling (`cargo xtask`): the signing key, the packages, the manifest. |

[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains how they fit together, and [docs/RELEASING.md](docs/RELEASING.md) how a release is made and how installed copies update themselves.

## Development

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

## Licence

[Apache-2.0](LICENSE).

This project is not affiliated with, endorsed by or connected to WhatsApp.
