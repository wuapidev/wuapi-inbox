//! One instance per data directory.
//!
//! The first start listens on a port of this computer only (`127.0.0.1`)
//! and writes the port, with a secret drawn at random, to a file of the
//! data directory that only the user can read. A start that finds it
//! running hands over what it was started with (a WhatsApp link, or
//! nothing: the window is then brought to the front) and ends, so two
//! never work the same data. A file left by an instance that ended
//! without removing it is found dead and replaced. Nothing here leaves
//! the computer.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The file, in the data directory.
const FILE_NAME: &str = "instance";
/// How long the running instance has to answer.
const WAIT: Duration = Duration::from_millis(1500);
/// The longest link handed over.
const MAX_LINK: usize = 8 * 1024;

/// What a start is.
pub enum Claim {
    /// The only one: it goes on, and hears the next starts here.
    First(Listener),
    /// Another was running and took what this one was started with.
    Handed,
}

/// The running instance's ear.
pub struct Listener {
    socket: TcpListener,
    secret: String,
    file: PathBuf,
}

/// Hands `link` to the instance running on `dir`, if one is; otherwise
/// becomes it. When the port cannot be opened at all this start goes on
/// alone, as it did before there was a port: `First` without anyone to
/// hear, which is better than no window.
pub fn claim(dir: &Path, link: Option<&str>) -> Result<Claim, String> {
    let file = dir.join(FILE_NAME);
    if let Some((port, secret)) = read(&file) {
        if hand_over(port, &secret, link).is_ok() {
            return Ok(Claim::Handed);
        }
    }
    let socket = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|error| error.to_string())?;
    let port = socket
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let secret = secret()?;
    fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    write_private(&file, &format!("{port} {secret}\n")).map_err(|error| error.to_string())?;
    Ok(Claim::First(Listener {
        socket,
        secret,
        file,
    }))
}

impl Listener {
    /// Hears the next starts on a thread of its own: each one's link, or
    /// `None` for a start with none, is given to `heard`.
    pub fn serve(self, heard: impl Fn(Option<String>) + Send + 'static) {
        let spawned = std::thread::Builder::new()
            .name("instance".into())
            .spawn(move || {
                for stream in self.socket.incoming() {
                    let Ok(stream) = stream else { continue };
                    match answer(stream, &self.secret) {
                        Ok(link) => heard(link),
                        Err(error) => tracing::debug!(%error, "a start was not heard"),
                    }
                }
                // Not reached while the socket is open; the file goes
                // with the process otherwise.
                drop(self);
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "the next starts will not be heard");
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        // Only the file this instance wrote: a newer one is left alone.
        if read(&self.file).is_some_and(|(_, secret)| secret == self.secret) {
            let _ = fs::remove_file(&self.file);
        }
    }
}

/// The port and secret in the file, when it reads as one.
fn read(file: &Path) -> Option<(u16, String)> {
    let text = fs::read_to_string(file).ok()?;
    let (port, secret) = text.trim().split_once(' ')?;
    Some((port.parse().ok()?, secret.to_owned()))
}

/// The second start's side: the secret, the link, and the word back.
fn hand_over(port: u16, secret: &str, link: Option<&str>) -> std::io::Result<()> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, WAIT)?;
    stream.set_read_timeout(Some(WAIT))?;
    stream.set_write_timeout(Some(WAIT))?;
    let link = link.unwrap_or_default().replace(['\r', '\n'], "");
    stream.write_all(format!("{secret}\n{link}\n").as_bytes())?;
    stream.flush()?;
    let mut word = String::new();
    BufReader::new(stream).read_line(&mut word)?;
    if word.trim() == "ok" {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "the running instance did not take it",
        ))
    }
}

/// The running instance's side: a start that knows the secret is heard.
fn answer(stream: TcpStream, secret: &str) -> Result<Option<String>, String> {
    stream
        .set_read_timeout(Some(WAIT))
        .map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|error| error.to_string())?
            .take(MAX_LINK as u64 + 256),
    );
    let mut told = String::new();
    reader
        .read_line(&mut told)
        .map_err(|error| error.to_string())?;
    if !same(told.trim_end().as_bytes(), secret.as_bytes()) {
        return Err("a start without the secret".into());
    }
    let mut link = String::new();
    reader
        .read_line(&mut link)
        .map_err(|error| error.to_string())?;
    let mut stream = stream;
    stream
        .write_all(b"ok\n")
        .map_err(|error| error.to_string())?;
    let link = link.trim();
    Ok((!link.is_empty() && link.len() <= MAX_LINK).then(|| link.to_owned()))
}

/// Compared without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn secret() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Writes `text` to `file` so that only the user reads it: into a new
/// file beside it first, then in its place.
fn write_private(file: &Path, text: &str) -> std::io::Result<()> {
    let part = file.with_extension("part");
    let _ = fs::remove_file(&part);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut out = options.open(&part)?;
    out.write_all(text.as_bytes())?;
    out.sync_all()?;
    drop(out);
    fs::rename(&part, file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn first(dir: &Path) -> Listener {
        match claim(dir, None).unwrap() {
            Claim::First(listener) => listener,
            Claim::Handed => panic!("nothing was running"),
        }
    }

    #[test]
    fn a_second_start_hands_its_link_to_the_first_and_ends() {
        let dir = tempfile::tempdir().unwrap();
        let (tell, heard) = mpsc::channel();
        first(dir.path()).serve(move |link| tell.send(link).unwrap());

        let link = "https://wa.me/584125550100?text=hi";
        assert!(matches!(claim(dir.path(), Some(link)), Ok(Claim::Handed)));
        assert_eq!(heard.recv_timeout(WAIT).unwrap().as_deref(), Some(link));
        // A start with nothing to open brings the window forward.
        assert!(matches!(claim(dir.path(), None), Ok(Claim::Handed)));
        assert_eq!(heard.recv_timeout(WAIT).unwrap(), None);
    }

    #[test]
    fn the_file_is_the_users_alone() {
        let dir = tempfile::tempdir().unwrap();
        let _listener = first(dir.path());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(FILE_NAME))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_file_left_by_an_instance_that_ended_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        // A port nothing listens on any more.
        let dead = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = dead.local_addr().unwrap().port();
        drop(dead);
        fs::write(dir.path().join(FILE_NAME), format!("{port} feedface\n")).unwrap();
        let listener = first(dir.path());
        assert_ne!(read(&listener.file).unwrap().1, "feedface");
        // And it goes when that instance does.
        drop(listener);
        assert!(!dir.path().join(FILE_NAME).exists());
    }

    #[test]
    fn a_start_without_the_secret_is_not_heard() {
        let dir = tempfile::tempdir().unwrap();
        let listener = first(dir.path());
        let port = listener.socket.local_addr().unwrap().port();
        let (tell, heard) = mpsc::channel();
        listener.serve(move |link| tell.send(link).unwrap());
        assert!(hand_over(port, "guessed", Some("https://wa.me/1")).is_err());
        assert!(heard.recv_timeout(Duration::from_millis(200)).is_err());
    }
}
