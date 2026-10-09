# Releasing and updating

How a release is made, what it consists of, and how an installed copy finds, verifies and installs it. The code is `crates/updater` (pure logic, no window), `crates/app/src/update.rs` and `crates/app/src/ui/updates.rs` (the application's side), `crates/xtask` (the release tooling) and `.github/workflows/release.yml`.

```text
  tag v1.2.0 ──► release.yml ──► builds per platform ──► archives
                                                  │
                    latest.json  ◄── xtask manifest (sizes, SHA-256)
                    latest.json.sig ◄── xtask sign (UPDATE_SIGNING_KEY)
                                                  │
                                    GitHub Release (or any static host)
                                                  │
  installed copy ── asks now and then ── verifies the signature ── downloads
  in the background ── verifies size and SHA-256 ── "Restart to update"
        │
  next start: verifies again ── renames the new version into place, keeps
  the old one ── starts it ── watches it come up ── or puts the old one back
```

## What a release is

A release is a set of files under one address. Nothing in the application knows that the address is GitHub: the default base URL is `https://github.com/wuapidev/wuapi-inbox/releases/latest/download` (`updater::DEFAULT_BASE_URL`, the only place that says so), and the same files copied to any static host, or to a directory, are the same release.

| File | For |
|---|---|
| `latest.json` | The manifest of the `stable` channel |
| `latest.json.sig` | Its signature |
| `wuapi-inbox-<version>-linux-x86_64.tar.gz` | Linux: the updater, and people (binary, desktop entry, icon, licence) |
| `wuapi-inbox-<version>-linux-x86_64.AppImage` | Linux: people, one file that runs without installing (`packaging/linux/appimage.sh`; not replaced by the updater) |
| `install.sh` | Linux: people, a home install that keeps updating itself (`packaging/linux/install.sh`) |
| `wuapi-inbox_<version>_amd64.deb`, `wuapi-inbox-<version>-1.x86_64.rpm` | Linux: people, through the system's package manager (under `/usr`; updated by it, see "Linux format") |
| `wuapi-inbox-<version>-macos-aarch64.tar.gz`, `…-macos-x86_64.tar.gz` | macOS: the updater (the `.app` bundle) |
| `wuapi-inbox-<version>-macos-aarch64.dmg`, `…-macos-x86_64.dmg` | macOS: people |
| `wuapi-inbox-<version>-windows-x86_64.tar.gz` | Windows: the updater (the `.exe`) |
| `wuapi-inbox-<version>-windows-x86_64.zip` | Windows: people |
| `SHA256SUMS` | Every file's SHA-256, for people |

The disk image is built by `cargo xtask dmg --input "work/Wuapi.app" --out dist/<name>.dmg` (macOS only; `--volume <NAME>` for a trial image, `--dmgbuild <PROGRAM>` where `dmgbuild` is not on the path). It renders `crates/app/assets/brand/dmg-background.svg` at 1x and 2x into one TIFF, and has [`dmgbuild`](https://dmgbuild.readthedocs.io) (`pip install dmgbuild`; the workflow pins the version) write the window from `packaging/macos/dmg-settings.py`: icon view without toolbar or sidebar, the application on the left and the Applications folder on the right, the volume's icon. The numbers of the layout are constants of `crates/xtask/src/dmg.rs`, given to the settings and tested against the picture; Finder is never scripted. The command mounts what it built, out of sight, and fails if the window's settings, the picture, the volume's icon or the link are missing. The image is signed, notarized and stapled after that, as before. `cargo xtask dmg-background --out-dir <DIR>` renders the picture alone.

The names are fixed (`xtask::archive_name`), so the addresses in a manifest always have the same shape. The updater's archive is a gzipped tar on every platform, packed and unpacked by the same code (`updater::archive`), with one directory in it: packing the same input twice gives the same bytes.

`linux-aarch64` is a platform the manifest and the application know, and no build is made for it yet.

## The manifest

```json
{
  "format": 1,
  "channel": "stable",
  "version": "1.2.0",
  "released": "2026-10-02T12:00:00Z",
  "notes": "## What's Changed\n* Faster sends",
  "min_supported": "1.0.0",
  "platforms": {
    "linux-x86_64": {
      "url": "wuapi-inbox-1.2.0-linux-x86_64.tar.gz",
      "size": 31415926,
      "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
    },
    "macos-aarch64": { "url": "…", "size": 0, "sha256": "…" },
    "macos-x86_64": { "url": "…", "size": 0, "sha256": "…" },
    "windows-x86_64": { "url": "…", "size": 0, "sha256": "…" }
  }
}
```

| Field | Meaning |
|---|---|
| `format` | The format of the file itself: `1`. A build refuses a format it does not know. |
| `channel` | `stable` or `beta`. A manifest of another channel than the build's is refused, so a beta manifest served at the stable address installs nothing. |
| `version` | Semantic version. Compared as numbers (`1.10.0` is after `1.4.0`; a release is after its pre-releases). |
| `released` | RFC 3339, UTC. |
| `notes` | Short Markdown, shown as text in Settings > About. |
| `min_supported` | Optional. A build older than this does not install the update: it says a new version is there and links to the download page (the "install it again" path, for the day an update cannot be applied over an old layout). |
| `rollback` | Optional: `{"from": ["1.3.0"]}`. Without it a manifest with a lower version than the build's is ignored: there are no downgrades. With it, the builds it names (and only those) go back to `version`. A rollback manifest replayed later takes nobody back, since nobody is on a version it names. |
| `platforms.<key>.url` | A file name next to the manifest (what the release workflow writes: it makes the release independent of its host), or an absolute `https://` URL. |
| `platforms.<key>.size`, `.sha256` | The file's size in bytes and its SHA-256 in lower-case hexadecimal. |

Unknown fields are ignored, so fields can be added without a new format.

`cargo xtask manifest --dir dist` writes it from the archives in a directory; `--min-supported`, `--rollback-from 1.3.0,1.3.1`, `--channel` and `--notes-file` set the rest; `--require <platform>,…` fails when an archive is missing.

## The signature

The manifest is signed as a whole with Ed25519, in [minisign](https://jedisct1.github.io/minisign/)'s format, and the signature travels next to it as `latest.json.sig`:

```text
untrusted comment: signature from wuapi Inbox release key 1A2B3C4D5E6F7081
RUT…                    base64("ED" ‖ key id ‖ Ed25519(BLAKE2b-512(latest.json)))
trusted comment: timestamp:1790942400	file:latest.json	version:1.2.0	channel:stable
x9k…                    base64(Ed25519(signature ‖ trusted comment))
```

- **Signing and verifying are in this repository** (`updater::sign`), so no external tool is needed. `minisign -V -P <public key> -m latest.json` verifies a release too; a test holds the signatures made here to an independent implementation (`minisign-verify`).
- **The application embeds public keys only**, in `EMBEDDED_KEYS` in `crates/updater/src/sign.rs`: two places, so a key can be rotated. Until one of them is a real key, the placeholder `PLACEHOLDER_KEY` is there, the updater is disabled, the start says so in the log (`updates are disabled: this build has no update key embedded`) and Settings > About says "Updates are off".
- **The secret key** is one line (`base64("SK" ‖ key id ‖ seed)`, this tool's own format, not minisign's password-protected file). It is read from the environment variable `UPDATE_SIGNING_KEY` and never written to a file by any of this code.
- **What is trusted is the signature, not TLS.** A manifest is parsed only after its signature by an embedded key is good (`Manifest::verified`); a file is installed only if its size and SHA-256 are the ones the signed manifest names. HTTPS is still required (plain `http://` is taken for `localhost` only, and `file://` for a directory, both for trying a release).
- `cargo xtask sign` refuses to sign with a key the application does not embed: every install would refuse the result, so the release fails instead.

**Rotating the key:** run `cargo xtask keygen`, put the new public key in the free place of `EMBEDDED_KEYS` and release that version, signed with the old key. From the next release on, set `UPDATE_SIGNING_KEY` to the new secret. Once the versions that only know the old key are below a manifest's `min_supported`, take the old key out. If the old key leaked rather than aged, there is no way to reach the installs that only know it other than a manual reinstall: that is the cost of not trusting the transport.

## What the application does

- **When it asks.** 45 seconds after the start, then every six hours, each wait within a fifth either way (`updater::Schedule`). After a failure the wait is 15 minutes and doubles with every failure in a row, up to a day; it is never shorter than 15 minutes, so a server that is away is not hammered. Settings > About > "Check for updates automatically" (on by default) turns the schedule off; "Check now" and the palette's "Check for updates" ask at once.
- **What a request says.** `User-Agent: wuapi-inbox/<version> (<platform>; <channel>)`. No query, no cookie, no identifier of the install or the account.
- **Not reachable is not an error.** A 404, 401, 403 or 410 for the manifest means "no update" and nothing more (a private repository answers 404 to a request without a token). A network that is down, a 5xx, a signature that does not verify or a file that does not match are logged and tried again later; the window says "up to date", and About adds that the server could not be reached when that was the reason.
- **Downloading** happens on the engine's Tokio runtime, never on the interface's thread, into `updates/staged/<version>/artifact.part` in the data directory. A transfer that stops is continued with a `Range` request from the bytes already on disk (a server that cannot resume is asked for the whole file); connecting is bounded at 15 s and silence at 30 s; more bytes than the manifest's `size` end the transfer; more than 500 MB is never downloaded.
- **Ready** means: the manifest's signature is good, the file has the manifest's size and SHA-256, and the archive unpacks to something that can be installed. The manifest and its signature are kept next to the file. Only then does the window show the notice.
- **The notice** is one small button with the rail's tools (and the palette's "Restart to update"), there only while an update is ready. A click opens Settings > About with the version and the notes and "Restart now". Nothing opens by itself, at any point.
- **"Restart now"** with messages still on their way out, or a voice note being recorded, says what would be interrupted and asks once more ("Restart anyway"). Queued messages survive a restart (they are in the outbox on disk); an unsent recording does not.
- **Installing happens at a start, never while running**: at "Restart now", or at the next ordinary start if the user never presses it.
- **About** shows the version, the channel and when the source was last asked.
- **Another source**: `--update-url <base>` for one run, or Settings > About > Advanced > "Update source" (kept in `update.json` next to the settings). No token is embedded or sent, anywhere.
- **The database and the settings are not touched by an update.** They are in the data directory; an update replaces the executable (or the bundle) and nothing else.

### The start, and going back

The process that installs an update is the *old* version (`updater::launch`). That is what makes going back dependable: a new build that cannot start at all is noticed by a process that works.

1. The old version starts and finds a ready update. It verifies everything again from the signature down (the staged files may have been changed on disk), and checks again that the update is one for this version.
2. It unpacks the archive next to what it replaces (`.<name>.update`), and renames the new version into place. The version replaced is kept next to it (`.<name>.previous`). It leaves a mark, `updates/pending.json`.
3. It starts the new version with the same arguments and watches it for up to 20 seconds.
4. The new version opens its window and removes the mark ("it started"); the old process exits and the kept version is deleted.
5. If the new version instead ends with an error before its window (or cannot be started), the old process renames the kept version back, records the version as one never to install again (the next version is installed normally), leaves a notice for About, and goes on starting as the old version.

If nobody is watching (the watch timed out while the new version was still coming up) and the new version then fails to reach its window three starts in a row, it puts the previous version back itself.

**The schema guard.** Migrations only go forward. A build that finds a database with a newer schema than it knows never opens it and never changes it (`StoreError::NewerSchema`; `Store::schema_of` asks without migrating): the session runs in memory and says that the chats on this computer were written by a newer version and were left untouched. This is what makes going back safe, and it is also why going back is sometimes not done: just before the new version opens the store, it writes the schema it is about to bring the database to into the mark. If the new version then fails and that schema is newer than the old version's, the old version is **not** put back by itself, because it could not open the chats; the new version stays installed, the process exits with an error, and the log and About say why and what to do (install that version again, or a newer one). A new version that fails *before* it gets to the store is always undone.

## Per platform

| | Linux | macOS | Windows |
|---|---|---|---|
| What is replaced | The executable | The `.app` bundle | The `.exe` |
| How | A hard link keeps the old file as `.wuapi-inbox.previous`; `rename` puts the new file over the name in one step | `renamex_np(RENAME_SWAP)` exchanges the staged bundle and the installed one in one step; where that fails, two renames | The running `.exe` is renamed aside (Windows allows renaming a running executable, not writing or deleting it), the new one is renamed in; the old one is deleted at the next start |
| Never half-written | The new file is complete before the rename | The staged bundle is complete, and its code signature checked, before the exchange | As Linux |
| Not self-updating (told, with a link; on macOS, offered a move where one helps) | Installed under `/usr`, `/nix`, `/snap`, `/app`… (a system package), Flatpak or Snap, AppImage, or a directory that cannot be written to | Homebrew Cask, not inside a bundle: told, with a link. Running from a read-only volume (the disk image) or a folder that cannot be written to, or under App Translocation (opened from Downloads without being moved): offered "Move to Applications" instead of the link, see below | Under Program Files or `WindowsApps`, or a directory that cannot be written to: it would need elevation, which is never asked for |
| Code signing | None | If the running bundle is signed by a team, the staged bundle must pass `codesign --verify --deep --strict` and be signed by the same team; an unsigned (ad hoc) build goes by the signed manifest alone | Authenticode is applied by CI when the certificate exists; the updater does not check it (the signed manifest is the gate) |

A build made by `cargo` (anything under `target/debug` or `target/release`) never replaces itself.

**The bundle's name (macOS).** The application is "Wuapi" and its bundle `Wuapi.app` (`updater::install::BUNDLE_NAME`); in 0.1.0 and 0.1.1 it was `wuapi Inbox.app` (`FORMER_BUNDLE_NAMES`). The name of the bundle inside an update's archive does not matter to an install, those two versions included: the payload is the archive's first `.app` (`archive::find_payload`), and it is exchanged with the installed bundle at the installed bundle's path (`Target::replace_with`), so an install keeps the folder name it has. The executable inside is `wuapi-inbox` in both. An install whose folder still has the former name is renamed by the new version itself, at a start (`Startup::take_todays_name`, after `Startup::run`): only a self-updating bundle, only when no update is waiting to be confirmed or installed and nothing is kept next to the bundle, and only when nothing beside it is called `Wuapi.app`. The process then starts the executable under the new path and exits; if that does not start, the former name is put back and the start goes on. A bundle the user renamed is left alone. Everything that is data keeps the first name: the binary, the package, the data directory, the application id and the release files (`wuapi-inbox-…`).

**Move to Applications (macOS).** The two usual ways of opening a downloaded disk image leave a copy that cannot replace itself: straight from the image (read-only), or from Downloads (macOS runs a randomised read-only copy). Such a copy is asked once, on top of the chat list, whether to move to the Applications folder; "Not now" is remembered (`move_prompt_done` in the settings), and Settings > About keeps a "Move to Applications" button where the link to the download page would be. The decision is `updater::install::relocation_in` (pure, next to `detect_in`): `/Applications/Wuapi.app`, or `~/Applications` where that cannot be written to (made if it is not there), whatever the copy that runs is called (a name the user gave it is kept); a `wuapi Inbox.app` already in the folder is set aside by a rename to where a replaced version is kept, not left beside the new one; and nothing for every other reason an install does not update itself. `Relocation::run` copies the running bundle with `ditto` to a place next to the target, checks the copy's code signature against the running one, takes `com.apple.quarantine` off the copy, puts it in place with a rename (an older copy there is exchanged for it by `Target::replace_with` and kept as `.previous` until the next start, never deleted first), starts it with the same arguments and quits. The bundle that runs is never moved or removed. A step that fails undoes the ones before it and says why in the prompt. From the Applications folder the copy is `SelfUpdating`, and an update needs nothing but the restart.

**Linux format.** The release is a single self-contained binary in a tarball (it links only the C library, ALSA and the X11/Wayland keyboard libraries; SQLCipher, OpenSSL, fonts and icons are compiled in), built on the oldest Ubuntu runner so that it runs wherever the C library is at least that old: glibc 2.35, so Debian 12, Ubuntu 22.04, Fedora 35, RHEL 9 and later. The same files reach people four ways. The **tarball** is for extracting anywhere writable and running the binary. **`install.sh`** (`packaging/linux/install.sh`) unpacks it under `$HOME` (`~/.local` by default), points the desktop entry at the absolute path so the applications menu finds it whatever the session's `PATH` holds, verifies the download against the manifest's SHA-256, and checks the libraries the binary links before installing; a home directory is writable, so the self-updater keeps working there. The **`.deb` and `.rpm`** (`packaging/linux/package.sh`, nFPM, built by the publish job) are for the system's package managers: they install under `/usr`, where the application does not replace itself (the "not self-updating" row above) and About says a new version is there; the package manager owns those files. The **AppImage** (`packaging/linux/appimage.sh`, built by the publish job from the same archive) is one file that runs without installing. It is not the updatable format: an AppImage has its own update mechanism (zsync) that would sit beside the signed manifest, so the manifest keeps pointing at the tarball, and an AppImage is recognised (`$APPIMAGE`) and told about new versions instead of being replaced, as the packages are. Running one needs FUSE 2 on the user's machine, or `--appimage-extract-and-run`. The workflow pins appimagetool and checks its SHA-256 before it runs.

**macOS without a certificate.** The bundle is signed ad hoc. It runs, but Gatekeeper refuses a downloaded copy until the user allows it (right click, Open), and macOS runs it translocated until it is moved to Applications, where it then updates itself.

**Windows.** The release build is a window application (`windows_subsystem = "windows"`), so no console opens; a side effect is that `--help` prints nothing when started from a terminal. There is no installer: the `.zip` holds the `.exe`, to be put in a folder the user owns. The executable has no embedded icon yet.

## Why the swap is written here

| Crate | Licence | What it would do | Why not |
|---|---|---|---|
| [`self-replace`](https://crates.io/crates/self-replace) | Apache-2.0 | Replaces the running executable, including the Windows trick | One file only (no bundle), and it deletes the old executable: there would be nothing to go back to |
| [`self_update`](https://crates.io/crates/self_update) | MIT | Finds a release on GitHub or S3, downloads, replaces through `self-replace` | Ties the source to a backend, has its own optional signature scheme (zipsign) instead of a signed manifest, no rollback, no bundles |
| [`axoupdater`](https://crates.io/crates/axoupdater) | MIT or Apache-2.0 | Updates what cargo-dist's installers installed, from their install receipt | Takes over packaging and installing; no place for a signature check of our own; nothing for an install made by unpacking an archive |
| [Velopack](https://velopack.io) | MIT | A whole framework: packager, installer, delta updates, its own feed | Takes over packaging on all three platforms and its own release feed; Linux only as AppImage; the trust anchor would be its feed and the OS's code signing |

All four licences are compatible with Apache-2.0; the reasons are about fit. What they would save is the file swap, which here is three renames per platform (`updater::install`, about two hundred lines) and is exercised on the three systems in CI. What they do not give is the part that matters: one signed manifest as the only gate, a kept previous version, a watched first start, and a rollback that knows about the database schema. So the swap is ours, and whatever does it is behind the manifest's verification.

## CI

`ci.yml` runs on pull requests and on pushes to `main`: `cargo fmt --check` once, then `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` on Linux x86_64, macOS aarch64, macOS x86_64 and Windows x86_64. The window tests run everywhere: the toolkit's test platform needs no display. Linux needs the ALSA, fontconfig, Wayland and xkbcommon headers; everything else builds from source (SQLCipher with a vendored OpenSSL needs `perl` and `make`, or `nmake` with MSVC, which the runners have).

Where it stands on Windows: everything of the updater passes there, and two window tests of the sticker picker do not (`ui::tests::stickers::only_the_tile_under_the_pointer_or_the_keyboard_moves`, which the toolkit's test scheduler stops with "activity on thread tokio-rt-worker… your test is not deterministic", and `a_sticker_not_downloaded_yet_is_a_place_that_is_fetched_when_it_is_in_view`). They are left failing rather than turned off: the picker decodes on another thread and the tests do not hold that still on Windows. One test of pasted file addresses is ignored on Windows by name, because its paths are a Unix desktop's. The window tests write the secondary key as `ctrl`; the test helper presses Cmd for it on macOS.

`release.yml` runs on a tag `v*`, and by hand with "dry run" (the default by hand: build, package and sign, publish nothing).

- **One version.** `[workspace.package] version` in `Cargo.toml`. The workflow reads it and refuses a tag that is not `v<version>` (`cargo xtask check-tag v1.2.0` says the same locally).
- A version with a pre-release part (`1.3.0-beta.1`) is built as the `beta` channel: its manifest is `beta.json` and the GitHub release is marked a pre-release, so `releases/latest/download` (and with it every stable install) never sees it. Serving `beta.json` to beta builds needs an address of its own, which is not set up; no build follows the beta channel today (`update::CHANNEL`).
- **It fails closed.** A missing platform archive, a missing `UPDATE_SIGNING_KEY`, a key the application does not embed, or a manifest that does not verify against the archives stops the workflow before anything is published. A dry run without the secret stops short of signing with a warning and publishes nothing.
- **The Linux installer and packages are built with the manifest.** The publish job runs `packaging/linux/package.sh` (nFPM) and copies `packaging/linux/install.sh` into `dist/` before the checksums are written, so they are verified, uploaded and attached like every other file, and a failure to build them stops the release.

| Secret | Enables |
|---|---|
| `UPDATE_SIGNING_KEY` | Signing the manifest. Required for a release. |
| `MACOS_CERTIFICATE_P12` (base64 of the Developer ID Application certificate with its key), `MACOS_CERTIFICATE_PASSWORD`, `MACOS_SIGNING_IDENTITY` (`Developer ID Application: Name (TEAMID)`) | Signing the bundle and the disk image with the hardened runtime |
| `APPLE_ID`, `APPLE_APP_PASSWORD` (an app-specific password), `APPLE_TEAM_ID` | Notarizing and stapling them (needs the certificate too) |
| `WINDOWS_CERTIFICATE_PFX` (base64), `WINDOWS_CERTIFICATE_PASSWORD` | Authenticode on the `.exe` |

Each signing step that has no secret is skipped with a notice in the run.

## Cutting the first release

1. Make the key, straight into the secret, without it touching a disk:
   ```sh
   cargo xtask keygen | gh secret set UPDATE_SIGNING_KEY --repo wuapidev/wuapi-inbox
   ```
   The public key and its id are printed on the terminal (standard error). To see it again later: `UPDATE_SIGNING_KEY=… cargo xtask public`. Keep a copy of the secret somewhere safe and offline: a secret cannot be read back from GitHub, and without it no install made with that public key can ever be updated.
2. Put the public key in `EMBEDDED_KEYS` in `crates/updater/src/sign.rs`, in place of `PLACEHOLDER_KEY`:
   ```rust
   pub const EMBEDDED_KEYS: [&str; 2] = ["RWQ…the public key…", ""];
   ```
   Commit and push. (`update::tests::until_a_key_is_embedded_the_start_does_nothing` covers both states.)
3. Set the version in `Cargo.toml` (`[workspace.package] version`), run `cargo check` so that `Cargo.lock` follows, commit and push, and wait for CI.
4. Optionally run the release workflow by hand as a dry run and look at its `release-<version>` artifact.
5. Tag and push the tag:
   ```sh
   git tag v0.1.0 && git push origin v0.1.0
   ```
6. Check the release: `cargo xtask verify --file latest.json --dir .` in a directory with the release's files.

A build made *before* step 2 has no key and never updates itself: the first release is installed by hand, and updates start with the second.

## While the repository is private

Release files of a private repository answer 404 to a request without a token, and no token is embedded. So an installed copy asks, hears "not found", and treats it as "no update": nothing is shown and nothing fails. The pipeline still runs and the releases exist. To update real installs before the repository is public, put the release's files (the manifest, its signature and the archives, as they are) on any HTTPS host and point the copies at it with Settings > About > Advanced > "Update source" or `--update-url`. When the repository becomes public the default address starts answering and nothing else changes.

## Trying a release locally

Everything up to the swap can be rehearsed without GitHub, with a throwaway key embedded in a local build (do not commit it):

```sh
export UPDATE_SIGNING_KEY="$(cargo xtask keygen 2>key-notes.txt)"   # the public key is in key-notes.txt
# put that public key in the second place of EMBEDDED_KEYS, build, and copy
# target/release/wuapi-inbox somewhere outside target/, say ~/try/wuapi-inbox
# then bump the version in Cargo.toml, build again, and:
cargo xtask package --platform linux-x86_64 --input target/release/wuapi-inbox --out-dir /tmp/release
cargo xtask manifest --dir /tmp/release
cargo xtask sign --file /tmp/release/latest.json
~/try/wuapi-inbox --data-dir ~/try/data --update-url file:///tmp/release
```

Use a separate `--data-dir`, so that the trial has its own `updates/` directory and settings.

## What is covered by tests, and what is not

Covered, on Linux, macOS and Windows runners: the signature (good, changed file, changed comment, another key, two embedded keys, minisign's own verifier), the manifest (parsing, versions, no downgrade, rollback, `min_supported`), the archive (round trip, entries that climb out, links, other entry kinds), the source (https only, joins, `file://`), the whole road against a local server (`crates/updater/tests/flow.rs`: check, download, resume with `Range`, hashes and sizes, ready, install at start for a file and for a bundle, the first start, going back, the schema guard, a version that takes itself out, a staged update changed on disk), replacing the executable of a process that is really running, the one-step exchange of two bundles (macOS only), `codesign` on an unsigned bundle (macOS only), and the window (the notice, About, the palette's commands, the update source).

Not covered by anything that runs:

- A real release end to end: no key is embedded, no tag was pushed, nothing was published.
- A real macOS bundle starting itself after a swap; the Developer ID signing, notarization and stapling steps of the workflow; the same-team check against a really signed bundle; App Translocation as macOS really reports it (the path test is against the documented shape).
- A real Windows install: the GUI-subsystem build starting its successor, the Authenticode step, SmartScreen.
- `packaging/macos/bundle.sh` and the packaging steps of `release.yml`, which only run in the release workflow.
- "Restart now" between two real processes of the application (the hand-over waits on a pipe that closes when the first process ends; the pieces are tested, the pair of real windows is not).
