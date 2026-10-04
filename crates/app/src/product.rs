//! The product's identity, in one place.
//!
//! Everything that shows or derives from the name (window title, data
//! directory, keychain service, user agent, application id) goes through
//! this module. Renaming the product means changing the constants here,
//! the package name in this crate's `Cargo.toml` (which names the binary)
//! and the Linux desktop entry in `assets/linux`, which cannot read a
//! constant; `product::tests` checks that they agree.
//!
//! The name is the product's; the look is the maker's. The brand assets
//! are in `brand.rs` and the design tokens in `theme.rs`.

/// The name shown to the user. "wuapi" is always lowercase, as the brand
/// writes it.
pub const PRODUCT_NAME: &str = "wuapi Inbox";

/// The name in a form safe for paths, the keychain service and the user
/// agent. It is also the package and binary name.
pub const SLUG: &str = "wuapi-inbox";

/// The application id: the window class on Wayland and X11, and the name
/// of the desktop entry and of the icon.
pub const APP_ID: &str = "dev.wuapi.inbox";

/// What [`SLUG`] was before the product had its name. Data directories and
/// keychain entries made under it are moved over on the first start (see
/// `rename.rs`).
pub const LEGACY_SLUG: &str = "fastwhatsapp";

/// Who makes it: the brand the application wears. Always lowercase.
pub const MAKER: &str = "wuapi";

/// Where the source code will live. A placeholder until the repository is
/// public: change it here when it has its address.
pub const REPOSITORY: &str = "https://github.com/wuapidev/wuapi-inbox";

/// The licence of the source code.
pub const LICENCE: &str = env!("CARGO_PKG_LICENSE");

/// The version of this build.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// [`SLUG`], owned.
pub fn slug() -> String {
    SLUG.to_owned()
}

/// The `User-Agent` sent by network providers.
pub fn user_agent() -> String {
    format!("{SLUG}/{VERSION}")
}

/// This computer's name, for the line the sign-in shows in the browser
/// ("wuapi Inbox on <host>"), as `wuapi login` sends its host name.
pub fn host_name() -> Option<String> {
    let from_file = || {
        ["/proc/sys/kernel/hostname", "/etc/hostname"]
            .iter()
            .find_map(|path| std::fs::read_to_string(path).ok())
    };
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(from_file)
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// The name this client gives itself when it signs in.
pub fn client_name() -> String {
    match host_name() {
        Some(host) => format!("{PRODUCT_NAME} on {host}"),
        None => PRODUCT_NAME.to_owned(),
    }
}

/// The directory that holds the data of the product named `slug`.
pub fn data_dir_of(slug: &str) -> std::path::PathBuf {
    data_base().join(slug)
}

/// The directory that holds the local database, created on demand.
///
/// Follows each platform's convention: `$XDG_DATA_HOME` (or
/// `~/.local/share`) on Linux, `~/Library/Application Support` on macOS,
/// `%APPDATA%` on Windows.
pub fn data_dir() -> std::path::PathBuf {
    data_dir_of(SLUG)
}

/// Where applications keep their data on this platform.
fn data_base() -> std::path::PathBuf {
    use std::path::PathBuf;
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home().map(|h| h.join(".local").join("share")))
    };
    base.unwrap_or_else(std::env::temp_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_is_the_same_everywhere() {
        // The binary is named after the package.
        assert_eq!(env!("CARGO_PKG_NAME"), SLUG);
        assert!(PRODUCT_NAME.starts_with("wuapi "), "the brand is lowercase");
        assert!(!PRODUCT_NAME.to_lowercase().contains("whatsapp"));
        assert_eq!(user_agent(), format!("wuapi-inbox/{VERSION}"));
        assert!(REPOSITORY.ends_with(SLUG));
        assert!(data_dir().ends_with(SLUG));
        assert!(client_name().starts_with(PRODUCT_NAME));

        // The desktop entry cannot read a constant; keep it in step.
        let entry = include_str!("../assets/linux/dev.wuapi.inbox.desktop");
        for line in [
            format!("Name={PRODUCT_NAME}"),
            format!("Exec={SLUG}"),
            format!("Icon={APP_ID}"),
            format!("StartupWMClass={APP_ID}"),
        ] {
            assert!(entry.lines().any(|l| l == line), "missing `{line}`");
        }
    }
}
