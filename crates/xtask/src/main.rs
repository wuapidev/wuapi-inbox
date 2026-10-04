//! Release tooling, so that cutting a release needs nothing but `cargo`:
//! the update signing key, the packages, the manifest and its signature.
//!
//! ```text
//! cargo xtask keygen                      a new signing key
//! cargo xtask public                      the public half of $UPDATE_SIGNING_KEY
//! cargo xtask package ...                 one platform's update archive
//! cargo xtask manifest ...                latest.json from a directory of archives
//! cargo xtask manifest-name              the name of a channel's manifest
//! cargo xtask sign --file latest.json     latest.json.sig, with $UPDATE_SIGNING_KEY
//! cargo xtask verify --file latest.json   what the application would say
//! cargo xtask version                     the workspace's version
//! cargo xtask check-tag v1.2.3            fails unless the tag is the version
//! ```
//!
//! `docs/RELEASING.md` says when each is used. The secret key is only
//! ever read from the environment and written to standard output by
//! `keygen`; this tool never puts it in a file.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use updater::archive;
use updater::manifest::{Artifact, Channel, Manifest, Rollback, FORMAT, PLATFORMS};
use updater::sign::{self, PublicKey, SecretKey};
use updater::Version;

/// The environment variable (and the Actions secret) the key is read from.
const KEY_VARIABLE: &str = "UPDATE_SIGNING_KEY";

/// The name archives and the binary go by.
const NAME: &str = "wuapi-inbox";

/// The version of the workspace: the one source of truth for a release.
const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "Release tooling for wuapi Inbox

Usage: cargo xtask <COMMAND>

Commands:
  keygen
      Makes a new update signing key. The secret key is printed on standard
      output (one line: pipe it into `gh secret set UPDATE_SIGNING_KEY`), the
      public key and what to do with it on standard error.
  public
      Prints the public key of the secret key in $UPDATE_SIGNING_KEY.
  package --platform <KEY> --input <PATH> [--input <PATH>...] --out-dir <DIR>
      Packs the binary (or the .app bundle), and anything else given, into
      <DIR>/wuapi-inbox-<version>-<KEY>.tar.gz.
  manifest --dir <DIR> [--out <FILE>] [--channel stable|beta]
           [--notes-file <FILE>] [--min-supported <VERSION>]
           [--rollback-from <VERSION>[,<VERSION>...]] [--require <KEY>[,<KEY>...]]
      Writes the manifest of the archives in <DIR> (default <DIR>/latest.json).
  manifest-name [--channel stable|beta]
      Prints the name of a channel's manifest (latest.json, beta.json).
  sign --file <FILE> [--allow-unknown-key]
      Signs <FILE> with $UPDATE_SIGNING_KEY into <FILE>.sig.
  verify --file <FILE> [--sig <FILE>] [--key <PUBLIC KEY>] [--dir <DIR>]
      Verifies a manifest as the application does; with --dir, the archives too.
  version
      Prints the workspace's version.
  check-tag <TAG>
      Fails unless <TAG> is `v` followed by the workspace's version.";

/// The arguments after a command, as `--flag value` pairs and bare flags.
struct Args {
    values: Vec<(String, String)>,
    flags: Vec<String>,
    bare: Vec<String>,
}

impl Args {
    fn parse(args: &[String], switches: &[&str]) -> Result<Self, String> {
        let mut parsed = Self {
            values: Vec::new(),
            flags: Vec::new(),
            bare: Vec::new(),
        };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if let Some(name) = arg.strip_prefix("--") {
                if switches.contains(&name) {
                    parsed.flags.push(name.to_owned());
                } else if let Some((name, value)) = name.split_once('=') {
                    parsed.values.push((name.to_owned(), value.to_owned()));
                } else {
                    let value = args.next().ok_or(format!("--{name} needs a value"))?;
                    parsed.values.push((name.to_owned(), value.clone()));
                }
            } else {
                parsed.bare.push(arg.clone());
            }
        }
        Ok(parsed)
    }

    fn all(&self, name: &str) -> Vec<&str> {
        self.values
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.all(name).last().copied()
    }

    fn need(&self, name: &str) -> Result<&str, String> {
        self.get(name).ok_or(format!("--{name} is needed"))
    }

    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|flag| flag == name)
    }

    fn only(&self, known: &[&str]) -> Result<(), String> {
        match self
            .values
            .iter()
            .find(|(key, _)| !known.contains(&key.as_str()))
        {
            Some((key, _)) => Err(format!("unknown option --{key}")),
            None => Ok(()),
        }
    }
}

fn version_of(text: &str) -> Result<Version, String> {
    Version::parse(text).map_err(|error| format!("`{text}` is not a version: {error}"))
}

fn versions_of(list: &str) -> Result<Vec<Version>, String> {
    list.split(',')
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(version_of)
        .collect()
}

/// The name of a platform's archive for a version. The manifest and the
/// release workflow both go by it, so the addresses in a manifest never
/// change shape.
fn archive_name(version: &Version, platform: &str) -> String {
    format!("{NAME}-{version}-{platform}.tar.gz")
}

/// A date and time as RFC 3339, in UTC.
fn rfc3339(seconds: u64) -> String {
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

fn keygen(out: &mut dyn Write, notes: &mut dyn Write) -> Result<(), String> {
    let secret = SecretKey::generate().map_err(|error| error.to_string())?;
    let public = secret.public();
    writeln!(out, "{}", secret.to_text()).map_err(|error| error.to_string())?;
    writeln!(
        notes,
        "A new update signing key, id {id}.

The line on standard output is the SECRET key. Store it as the Actions secret
{KEY_VARIABLE} and nowhere else; whoever has it can sign updates. It is not
written to any file by this tool.

The public key is:

    {public}

Put it in `EMBEDDED_KEYS` in crates/updater/src/sign.rs (in place of the
placeholder, or in the free place when rotating), commit, and release.
(`minisign -V -P {public} -m latest.json` verifies a release with it too.)",
        id = public.id(),
        public = public.to_text(),
    )
    .map_err(|error| error.to_string())
}

fn secret_key(key: Option<&str>) -> Result<SecretKey, String> {
    let text = key
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or(format!("${KEY_VARIABLE} is not set"))?;
    SecretKey::parse(text).map_err(|error| format!("${KEY_VARIABLE}: {error}"))
}

fn package(args: &Args) -> Result<String, String> {
    args.only(&["platform", "input", "out-dir", "version"])?;
    let platform = args.need("platform")?;
    let version = version_of(args.get("version").unwrap_or(VERSION))?;
    let inputs: Vec<PathBuf> = args.all("input").into_iter().map(PathBuf::from).collect();
    if inputs.is_empty() {
        return Err("--input is needed".into());
    }
    if let Some(missing) = inputs.iter().find(|input| !input.exists()) {
        return Err(format!("{} is not there", missing.display()));
    }
    let out_dir = PathBuf::from(args.need("out-dir")?);
    std::fs::create_dir_all(&out_dir).map_err(|error| error.to_string())?;
    let name = archive_name(&version, platform);
    let root = name.trim_end_matches(".tar.gz").to_owned();
    let inputs: Vec<&Path> = inputs.iter().map(PathBuf::as_path).collect();
    archive::pack_all(&inputs, &root, &out_dir.join(&name)).map_err(|error| error.to_string())?;
    Ok(name)
}

fn manifest(args: &Args) -> Result<PathBuf, String> {
    args.only(&[
        "dir",
        "out",
        "channel",
        "notes-file",
        "min-supported",
        "rollback-from",
        "require",
        "version",
    ])?;
    let dir = PathBuf::from(args.need("dir")?);
    let version = version_of(args.get("version").unwrap_or(VERSION))?;
    let channel = match args.get("channel") {
        None => Channel::Stable,
        Some(name) => Channel::parse(name).ok_or(format!("unknown channel `{name}`"))?,
    };
    let notes = match args.get("notes-file") {
        None => String::new(),
        Some(file) => std::fs::read_to_string(file)
            .map_err(|error| format!("{file}: {error}"))?
            .trim()
            .to_owned(),
    };
    let mut platforms = BTreeMap::new();
    for platform in PLATFORMS {
        let name = archive_name(&version, platform);
        let file = dir.join(&name);
        if !file.exists() {
            continue;
        }
        let (sha256, size) = archive::sha256_file(&file).map_err(|error| error.to_string())?;
        platforms.insert(
            platform.to_owned(),
            Artifact {
                // A name, not an address: wherever the release's files are
                // put together, the manifest is right.
                url: name,
                size,
                sha256,
            },
        );
    }
    let required = args.get("require").unwrap_or_default();
    for platform in required.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if !platforms.contains_key(platform) {
            return Err(format!(
                "{} is not in {}",
                archive_name(&version, platform),
                dir.display()
            ));
        }
    }
    if platforms.is_empty() {
        return Err(format!(
            "no archive of version {version} in {}",
            dir.display()
        ));
    }
    let manifest = Manifest {
        format: FORMAT,
        channel,
        version,
        released: rfc3339(now()),
        notes,
        min_supported: args.get("min-supported").map(version_of).transpose()?,
        rollback: args
            .get("rollback-from")
            .map(versions_of)
            .transpose()?
            .map(|from| Rollback { from }),
        platforms,
    };
    let out = args
        .get("out")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join(channel.manifest_name()));
    std::fs::write(&out, manifest.to_bytes()).map_err(|error| error.to_string())?;
    Ok(out)
}

fn signature_path(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".sig");
    PathBuf::from(name)
}

fn sign_file(args: &Args, key: Option<&str>, embedded: &[PublicKey]) -> Result<PathBuf, String> {
    args.only(&["file"])?;
    let file = PathBuf::from(args.need("file")?);
    let secret = secret_key(key)?;
    let public = secret.public();
    // A release signed with a key no build knows would be refused by
    // every install: better that the release fails here.
    if !embedded.contains(&public) && !args.has("allow-unknown-key") {
        return Err(format!(
            "the key in ${KEY_VARIABLE} (id {}) is not one of the keys embedded in the \
             application (EMBEDDED_KEYS in crates/updater/src/sign.rs): no install would \
             accept what it signs. Embed its public key first:\n\n    {}\n",
            public.id(),
            public.to_text()
        ));
    }
    let bytes = std::fs::read(&file).map_err(|error| format!("{}: {error}", file.display()))?;
    let manifest = Manifest::parse(&bytes).map_err(|error| error.to_string())?;
    let comment = format!(
        "timestamp:{}\tfile:{}\tversion:{}\tchannel:{}",
        now(),
        file.file_name().unwrap_or_default().to_string_lossy(),
        manifest.version,
        manifest.channel.name(),
    );
    let signature = sign::sign(&secret, &bytes, &comment);
    // What was just written must read back as good, with the public half.
    sign::verify(&[public], &bytes, &signature).map_err(|error| error.to_string())?;
    let out = signature_path(&file);
    std::fs::write(&out, signature).map_err(|error| error.to_string())?;
    Ok(out)
}

fn verify(args: &Args, embedded: &[PublicKey]) -> Result<String, String> {
    args.only(&["file", "sig", "key", "dir"])?;
    let file = PathBuf::from(args.need("file")?);
    let signature = args
        .get("sig")
        .map(PathBuf::from)
        .unwrap_or_else(|| signature_path(&file));
    let keys = match args.get("key") {
        Some(key) => vec![PublicKey::parse(key).map_err(|error| error.to_string())?],
        None => embedded.to_vec(),
    };
    if keys.is_empty() {
        return Err(
            "no public key is embedded yet (EMBEDDED_KEYS in crates/updater/src/sign.rs); \
             pass one with --key"
                .into(),
        );
    }
    let bytes = std::fs::read(&file).map_err(|error| format!("{}: {error}", file.display()))?;
    let signature = std::fs::read_to_string(&signature)
        .map_err(|error| format!("{}: {error}", signature.display()))?;
    let verified = sign::verify(&keys, &bytes, &signature).map_err(|error| error.to_string())?;
    let manifest = Manifest::parse(&bytes).map_err(|error| error.to_string())?;
    let mut said = format!(
        "{}: version {} ({}), signed by key {}\n",
        file.display(),
        manifest.version,
        manifest.channel.name(),
        verified.key_id
    );
    if let Some(dir) = args.get("dir") {
        for (platform, artifact) in &manifest.platforms {
            let name = artifact.url.rsplit('/').next().unwrap_or(&artifact.url);
            let (sha256, size) = archive::sha256_file(&Path::new(dir).join(name))
                .map_err(|error| format!("{name}: {error}"))?;
            if size != artifact.size || sha256 != artifact.sha256 {
                return Err(format!("{name} is not the file the manifest names"));
            }
            said.push_str(&format!("  {platform}: {name} ({size} bytes) matches\n"));
        }
    }
    Ok(said)
}

fn check_tag(tag: &str) -> Result<(), String> {
    if tag.strip_prefix('v') == Some(VERSION) {
        Ok(())
    } else {
        Err(format!(
            "the tag is `{tag}` and the workspace's version is {VERSION}: the tag of a \
             release is `v{VERSION}` (the version is `[workspace.package] version` in \
             Cargo.toml)"
        ))
    }
}

/// Runs a command line. What it prints goes to `out`; what it says about
/// it to `notes`.
fn run(
    args: &[String],
    key: Option<&str>,
    embedded: &[PublicKey],
    out: &mut dyn Write,
    notes: &mut dyn Write,
) -> Result<(), String> {
    let Some((command, rest)) = args.split_first() else {
        return Err(USAGE.to_owned());
    };
    let parsed = Args::parse(rest, &["allow-unknown-key"])?;
    let said = match command.as_str() {
        "keygen" => return keygen(out, notes),
        "public" => secret_key(key)?.public().to_text(),
        "package" => package(&parsed)?,
        "manifest" => manifest(&parsed)?.display().to_string(),
        "manifest-name" => {
            parsed.only(&["channel"])?;
            match parsed.get("channel") {
                None => Channel::Stable,
                Some(name) => Channel::parse(name).ok_or(format!("unknown channel `{name}`"))?,
            }
            .manifest_name()
            .to_owned()
        }
        "sign" => sign_file(&parsed, key, embedded)?.display().to_string(),
        "verify" => verify(&parsed, embedded)?,
        "version" => VERSION.to_owned(),
        "check-tag" => match parsed.bare.first() {
            Some(tag) => return check_tag(tag),
            None => return Err("check-tag needs the tag".into()),
        },
        "help" | "-h" | "--help" => USAGE.to_owned(),
        other => return Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    writeln!(out, "{}", said.trim_end()).map_err(|error| error.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let key = std::env::var(KEY_VARIABLE).ok();
    let outcome = run(
        &args,
        key.as_deref(),
        &updater::embedded_keys(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    );
    if let Err(message) = outcome {
        eprintln!("xtask: {message}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(args: &[&str], key: Option<&str>, embedded: &[PublicKey]) -> Result<String, String> {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let (mut out, mut notes) = (Vec::new(), Vec::new());
        run(&args, key, embedded, &mut out, &mut notes)?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn a_release_is_packaged_described_signed_and_verified() {
        let dir = tempfile::tempdir().unwrap();
        let text = |path: &Path| path.to_str().unwrap().to_owned();

        // The key: the secret on standard output, alone.
        let args = vec!["keygen".to_owned()];
        let (mut out, mut notes) = (Vec::new(), Vec::new());
        run(&args, None, &[], &mut out, &mut notes).unwrap();
        let secret = String::from_utf8(out).unwrap();
        assert_eq!(secret.lines().count(), 1);
        let notes = String::from_utf8(notes).unwrap();
        let public = call(&["public"], Some(&secret), &[]).unwrap();
        assert!(notes.contains(public.trim()), "{notes}");
        assert!(!notes.contains(secret.trim()), "the secret is said once");
        let embedded = [PublicKey::parse(&public).unwrap()];

        // Two platforms' archives.
        let binary = dir.path().join("wuapi-inbox");
        std::fs::write(&binary, "a binary").unwrap();
        let dist = dir.path().join("dist");
        for platform in ["linux-x86_64", "windows-x86_64"] {
            let name = call(
                &[
                    "package",
                    "--platform",
                    platform,
                    "--input",
                    &text(&binary),
                    "--out-dir",
                    &text(&dist),
                ],
                None,
                &[],
            )
            .unwrap();
            assert_eq!(
                name.trim(),
                format!("wuapi-inbox-{VERSION}-{platform}.tar.gz")
            );
            assert!(dist.join(name.trim()).exists());
        }

        // The manifest.
        let notes_file = dir.path().join("notes.md");
        std::fs::write(&notes_file, "- Faster.\n").unwrap();
        let out = call(
            &[
                "manifest",
                "--dir",
                &text(&dist),
                "--notes-file",
                &text(&notes_file),
                "--min-supported",
                "0.0.1",
                "--require",
                "linux-x86_64,windows-x86_64",
            ],
            None,
            &[],
        )
        .unwrap();
        let file = dist.join("latest.json");
        assert_eq!(Path::new(out.trim()), file);
        let manifest = Manifest::parse(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(manifest.version.to_string(), VERSION);
        assert_eq!(manifest.notes, "- Faster.");
        assert_eq!(manifest.platforms.len(), 2);
        assert_eq!(
            manifest.platforms["linux-x86_64"].url,
            format!("wuapi-inbox-{VERSION}-linux-x86_64.tar.gz"),
            "a name next to the manifest, not an address"
        );
        assert!(manifest.released.ends_with('Z'));

        // A platform that was to be there and is not fails the release.
        assert!(call(
            &[
                "manifest",
                "--dir",
                &text(&dist),
                "--require",
                "macos-aarch64"
            ],
            None,
            &[]
        )
        .is_err());

        // Signing fails closed: no key, a key that is not one, a key the
        // application does not embed.
        let sign = ["sign", "--file", &text(&file)];
        assert!(call(&sign, None, &embedded).is_err());
        assert!(call(&sign, Some(""), &embedded).is_err());
        assert!(call(&sign, Some("rubbish"), &embedded).is_err());
        let said = call(&sign, Some(&secret), &[]).unwrap_err();
        assert!(said.contains("EMBEDDED_KEYS"), "{said}");
        assert!(!dist.join("latest.json.sig").exists());

        call(&sign, Some(&secret), &embedded).unwrap();
        let said = call(
            &["verify", "--file", &text(&file), "--dir", &text(&dist)],
            None,
            &embedded,
        )
        .unwrap();
        assert!(
            said.contains("linux-x86_64") && said.contains("matches"),
            "{said}"
        );
        // And with the key given by hand, as somebody checking a release does.
        call(
            &["verify", "--file", &text(&file), "--key", public.trim()],
            None,
            &[],
        )
        .unwrap();

        // A manifest changed after signing, or an archive changed after
        // the manifest, is caught.
        let archive = dist.join(format!("wuapi-inbox-{VERSION}-linux-x86_64.tar.gz"));
        std::fs::write(&archive, "something else").unwrap();
        assert!(call(
            &["verify", "--file", &text(&file), "--dir", &text(&dist)],
            None,
            &embedded
        )
        .is_err());
        let mut bytes = std::fs::read(&file).unwrap();
        bytes.extend_from_slice(b" ");
        std::fs::write(&file, bytes).unwrap();
        assert!(call(&["verify", "--file", &text(&file)], None, &embedded).is_err());
        // With no key embedded and none given there is nothing to verify by.
        assert!(call(&["verify", "--file", &text(&file)], None, &[]).is_err());
    }

    #[test]
    fn the_tag_of_a_release_is_the_version() {
        assert!(check_tag(&format!("v{VERSION}")).is_ok());
        assert!(check_tag(VERSION).is_err(), "without the v");
        assert!(check_tag("v0.0.0-nope").is_err());
        assert_eq!(call(&["version"], None, &[]).unwrap().trim(), VERSION);
        assert_eq!(
            call(&["manifest-name"], None, &[]).unwrap().trim(),
            "latest.json"
        );
        assert_eq!(
            call(&["manifest-name", "--channel", "beta"], None, &[])
                .unwrap()
                .trim(),
            "beta.json"
        );
        assert!(call(&["manifest-name", "--channel", "nightly"], None, &[]).is_err());
        assert!(call(&["check-tag"], None, &[]).is_err());
    }

    #[test]
    fn dates_are_written_as_rfc_3339() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_790_942_399), "2026-10-02T11:59:59Z");
    }

    #[test]
    fn what_it_does_not_know_is_an_error() {
        assert!(call(&[], None, &[]).is_err());
        assert!(call(&["frobnicate"], None, &[]).is_err());
        assert!(call(&["manifest", "--nope", "x"], None, &[]).is_err());
        assert!(call(&["package", "--platform"], None, &[]).is_err());
        assert!(call(&["help"], None, &[]).unwrap().contains("keygen"));
    }
}
