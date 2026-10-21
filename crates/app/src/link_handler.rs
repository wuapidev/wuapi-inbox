//! Whether the system opens `whatsapp:` links with this application, and
//! the way to make it so.
//!
//! Linux: the desktop entry says it opens `x-scheme-handler/whatsapp`
//! (the packages' entry does, and an entry of the user's that does not
//! is put right first); `xdg-mime` chooses it. Windows: the scheme is
//! registered for the user (`HKCU\Software\Classes\whatsapp`); a choice
//! the user made in Windows' settings wins over that, and those settings
//! are opened to change it. macOS: the bundle declares the scheme, and
//! Launch Services is asked to choose it (the system may ask the user to
//! confirm). Each asks the system, so it is never called from a frame.

// Tests never ask the system (`ui/links.rs` puts a stand-in in place):
// in a test build most of this is not called.
#![cfg_attr(test, allow(dead_code))]

/// The scheme.
pub const SCHEME: &str = "whatsapp";

/// Who opens the links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handler {
    /// This application.
    Us,
    /// Another one, by the name the system gives it.
    Other(String),
    /// None at all.
    Nobody,
    /// The system could not be asked.
    Unknown(String),
}

impl Handler {
    /// One line for the settings.
    pub fn line(&self) -> String {
        match self {
            Handler::Us => crate::i18n::t("chat.link_handler.us").into(),
            Handler::Other(name) => crate::i18n::tr("chat.link_handler.other", &[("name", name)]),
            Handler::Nobody => crate::i18n::t("chat.link_handler.nobody").into(),
            Handler::Unknown(why) => crate::i18n::tr("chat.link_handler.unknown", &[("why", why)]),
        }
    }
}

/// Who opens `whatsapp:` links now.
pub fn current() -> Handler {
    platform::current()
}

/// Makes this application the one. Answers what to tell the user.
pub fn make_default() -> Result<String, String> {
    platform::make_default()
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{Handler, SCHEME};
    use crate::product::APP_ID;
    use std::path::PathBuf;
    use std::process::Command;

    fn mime() -> String {
        format!("x-scheme-handler/{SCHEME}")
    }

    fn entry_name() -> String {
        format!("{APP_ID}.desktop")
    }

    fn applications() -> Option<PathBuf> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })?;
        Some(data.join("applications"))
    }

    pub fn current() -> Handler {
        match Command::new("xdg-mime")
            .args(["query", "default", &mime()])
            .output()
        {
            Ok(out) => {
                let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                if name == entry_name() {
                    Handler::Us
                } else if name.is_empty() {
                    Handler::Nobody
                } else {
                    Handler::Other(name.trim_end_matches(".desktop").to_owned())
                }
            }
            Err(error) => Handler::Unknown(format!("xdg-mime: {error}")),
        }
    }

    pub fn make_default() -> Result<String, String> {
        let dir = applications().ok_or(crate::i18n::t("chat.link_handler.no_home"))?;
        let file = dir.join(entry_name());
        let installed = std::fs::read_to_string(&file).ok();
        let system = ["/usr/share/applications", "/usr/local/share/applications"]
            .iter()
            .map(|dir| PathBuf::from(dir).join(entry_name()))
            .find_map(|path| std::fs::read_to_string(path).ok());
        // The user's own entry wins over the system's: it is the one put
        // right. Without either, one is written for the program running.
        let fixed = match (&installed, &system) {
            (Some(text), _) => fix_entry(text, None),
            (None, Some(text)) if opens_scheme(text) => None,
            (None, _) => Some(fix_entry(&new_entry(), Some(&program()?)).unwrap_or_else(new_entry)),
        };
        if let Some(text) = fixed {
            std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
            std::fs::write(&file, text).map_err(|error| error.to_string())?;
            // Where the cache of what opens what is kept up to date.
            let _ = Command::new("update-desktop-database").arg(&dir).status();
        }
        let status = Command::new("xdg-mime")
            .args(["default", &entry_name(), &mime()])
            .status()
            .map_err(|error| format!("xdg-mime: {error}"))?;
        if !status.success() {
            return Err(crate::i18n::t("chat.link_handler.xdg_failed").into());
        }
        match current() {
            Handler::Us => Ok(crate::i18n::t("chat.link_handler.made_default").into()),
            other => Err(other.line()),
        }
    }

    /// The program to name in a new entry: the AppImage when it is one.
    fn program() -> Result<String, String> {
        if let Some(image) = std::env::var_os("APPIMAGE").filter(|path| !path.is_empty()) {
            return Ok(image.to_string_lossy().into_owned());
        }
        std::env::current_exe()
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|error| error.to_string())
    }

    fn new_entry() -> String {
        include_str!("../assets/linux/dev.wuapi.inbox.desktop").to_owned()
    }

    fn opens_scheme(entry: &str) -> bool {
        let mime = mime();
        entry.lines().any(|line| {
            line.strip_prefix("MimeType=")
                .is_some_and(|types| types.split(';').any(|kind| kind == mime))
        })
    }

    /// The entry, put right: it opens the scheme and is given the link
    /// (`%u`). `program` names the program to run. `None` when nothing
    /// needed changing.
    pub(super) fn fix_entry(entry: &str, program: Option<&str>) -> Option<String> {
        let mime = mime();
        let mut changed = false;
        let mut has_mime = false;
        let mut lines: Vec<String> = Vec::new();
        for line in entry.lines() {
            if let Some(types) = line.strip_prefix("MimeType=") {
                has_mime = true;
                if !types.split(';').any(|kind| kind == mime) {
                    let types = types.trim_end_matches(';');
                    let joined = if types.is_empty() {
                        format!("MimeType={mime};")
                    } else {
                        format!("MimeType={types};{mime};")
                    };
                    lines.push(joined);
                    changed = true;
                    continue;
                }
            } else if let Some(command) = line.strip_prefix("Exec=") {
                let command = match program {
                    Some(program) => quote(program),
                    None => command
                        .split_whitespace()
                        .filter(|part| !part.starts_with('%'))
                        .collect::<Vec<_>>()
                        .join(" "),
                };
                let line_now = format!("Exec={command} %u");
                changed |= line_now != line;
                lines.push(line_now);
                continue;
            }
            lines.push(line.to_owned());
        }
        if !has_mime {
            // In the main group, the first one.
            let at = lines
                .iter()
                .skip(1)
                .position(|line| line.starts_with('['))
                .map_or(lines.len(), |at| at + 1);
            lines.insert(at, format!("MimeType={mime};"));
            changed = true;
        }
        changed.then(|| lines.join("\n") + "\n")
    }

    fn quote(program: &str) -> String {
        if program.contains([' ', '"', '\\', '\'', '$', '`']) {
            let escaped = program
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('$', "\\$")
                .replace('`', "\\`");
            format!("\"{escaped}\"")
        } else {
            program.to_owned()
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{Handler, SCHEME};
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    /// No console window flashes for `reg.exe`.
    const NO_WINDOW: u32 = 0x0800_0000;

    fn reg(args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new("reg")
            .args(args)
            .creation_flags(NO_WINDOW)
            .output()
    }

    fn classes() -> String {
        format!(r"HKCU\Software\Classes\{SCHEME}")
    }

    fn program() -> Result<String, String> {
        std::env::current_exe()
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|error| error.to_string())
    }

    /// The program a choice in Windows' settings names, when there is one.
    fn user_choice() -> Option<String> {
        let key = format!(
            r"HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\{SCHEME}\UserChoice"
        );
        let out = reg(&["query", &key, "/v", "ProgId"]).ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines()
            .find(|line| line.trim_start().starts_with("ProgId"))
            .and_then(|line| line.split_whitespace().last())
            .map(str::to_owned)
    }

    pub fn current() -> Handler {
        if let Some(chosen) = user_choice().filter(|id| !id.eq_ignore_ascii_case(SCHEME)) {
            return Handler::Other(chosen);
        }
        let key = format!(r"{}\shell\open\command", classes());
        match reg(&["query", &key, "/ve"]) {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
                match program() {
                    Ok(me) if text.contains(&me.to_lowercase()) => Handler::Us,
                    _ => Handler::Other(crate::i18n::t("chat.link_handler.another_app").into()),
                }
            }
            Ok(_) => Handler::Nobody,
            Err(error) => Handler::Unknown(format!("reg: {error}")),
        }
    }

    pub fn make_default() -> Result<String, String> {
        let program = program()?;
        let command = format!("\"{program}\" \"%1\"");
        let class = classes();
        let open = format!(r"{class}\shell\open\command");
        for args in [
            [
                "add",
                class.as_str(),
                "/ve",
                "/d",
                "URL:WhatsApp link",
                "/f",
            ]
            .as_slice(),
            ["add", class.as_str(), "/v", "URL Protocol", "/d", "", "/f"].as_slice(),
            ["add", open.as_str(), "/ve", "/d", command.as_str(), "/f"].as_slice(),
        ] {
            let out = reg(args).map_err(|error| format!("reg: {error}"))?;
            if !out.status.success() {
                return Err(crate::i18n::t("chat.link_handler.windows_kept").into());
            }
        }
        match current() {
            Handler::Us => Ok(crate::i18n::t("chat.link_handler.made_default").into()),
            Handler::Other(_) => {
                // A choice made in Windows' settings is changed there.
                let _ = Command::new("cmd")
                    .args(["/C", "start", "", "ms-settings:defaultapps"])
                    .creation_flags(NO_WINDOW)
                    .status();
                Ok(crate::i18n::t("chat.link_handler.windows_other").into())
            }
            other => Err(other.line()),
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Handler, SCHEME};
    use crate::product::APP_ID;
    use std::ffi::{c_char, c_void, CStr, CString};

    type CFStringRef = *const c_void;
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFStringGetCString(
            string: CFStringRef,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(object: *const c_void);
    }

    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        fn LSSetDefaultHandlerForURLScheme(scheme: CFStringRef, bundle: CFStringRef) -> i32;
        fn LSCopyDefaultHandlerForURLScheme(scheme: CFStringRef) -> CFStringRef;
    }

    /// A CFString, released when dropped.
    struct Cf(CFStringRef);

    impl Cf {
        fn new(text: &str) -> Option<Self> {
            let text = CString::new(text).ok()?;
            let made = unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), UTF8) };
            (!made.is_null()).then_some(Cf(made))
        }

        fn text(&self) -> Option<String> {
            let mut buffer = vec![0 as c_char; 512];
            let read = unsafe {
                CFStringGetCString(self.0, buffer.as_mut_ptr(), buffer.len() as isize, UTF8)
            };
            (read != 0).then(|| {
                unsafe { CStr::from_ptr(buffer.as_ptr()) }
                    .to_string_lossy()
                    .into_owned()
            })
        }
    }

    impl Drop for Cf {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) };
        }
    }

    pub fn current() -> Handler {
        let Some(scheme) = Cf::new(SCHEME) else {
            return Handler::Unknown(crate::i18n::t("chat.link_handler.the_scheme").into());
        };
        let found = unsafe { LSCopyDefaultHandlerForURLScheme(scheme.0) };
        if found.is_null() {
            return Handler::Nobody;
        }
        let found = Cf(found);
        match found.text() {
            Some(id) if id.eq_ignore_ascii_case(APP_ID) => Handler::Us,
            Some(id) => Handler::Other(id),
            None => Handler::Unknown(crate::i18n::t("chat.link_handler.its_name").into()),
        }
    }

    pub fn make_default() -> Result<String, String> {
        let (Some(scheme), Some(bundle)) = (Cf::new(SCHEME), Cf::new(APP_ID)) else {
            return Err(crate::i18n::t("chat.link_handler.the_scheme").into());
        };
        let code = unsafe { LSSetDefaultHandlerForURLScheme(scheme.0, bundle.0) };
        if code != 0 {
            return Err(crate::i18n::tr(
                "chat.link_handler.macos_kept",
                &[("code", &code)],
            ));
        }
        Ok(crate::i18n::t("chat.link_handler.made_default_macos").into())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod platform {
    use super::Handler;

    pub fn current() -> Handler {
        Handler::Unknown(crate::i18n::t("chat.link_handler.not_here").into())
    }

    pub fn make_default() -> Result<String, String> {
        Err(crate::i18n::t("chat.link_handler.not_here_sentence").into())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::platform::fix_entry;

    #[test]
    fn an_entry_is_put_right_to_open_the_links() {
        let entry = include_str!("../assets/linux/dev.wuapi.inbox.desktop");
        // The packages' entry needs nothing.
        assert_eq!(fix_entry(entry, None), None);
        // One written before there were links is given both lines.
        let old = "[Desktop Entry]\nType=Application\nName=Wuapi\nExec=\"/home/me/.local/bin/wuapi-inbox\"\nIcon=dev.wuapi.inbox\n";
        let fixed = fix_entry(old, None).unwrap();
        assert!(
            fixed.contains("Exec=\"/home/me/.local/bin/wuapi-inbox\" %u\n"),
            "{fixed}"
        );
        assert!(
            fixed.contains("MimeType=x-scheme-handler/whatsapp;\n"),
            "{fixed}"
        );
        assert_eq!(fix_entry(&fixed, None), None, "once is enough");
        // Other types are kept.
        let typed = "[Desktop Entry]\nExec=wuapi-inbox %u\nMimeType=text/plain\n";
        assert!(fix_entry(typed, None)
            .unwrap()
            .contains("MimeType=text/plain;x-scheme-handler/whatsapp;"));
        // A program with a space in its path is quoted.
        let named = fix_entry(old, Some("/opt/my apps/wuapi-inbox")).unwrap();
        assert!(
            named.contains("Exec=\"/opt/my apps/wuapi-inbox\" %u"),
            "{named}"
        );
    }
}
