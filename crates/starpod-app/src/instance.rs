//! One running Starpod at a time, and a way for a second launch to hand its
//! request to the first.
//!
//! The first launch (the primary) binds a TCP listener on `127.0.0.1` with a
//! random port, and writes `<data dir>/instance.json` holding the port and a
//! random token. A later launch reads that file and connects within 500 ms.
//! If it connects, it sends `token\nCOMMAND\n`, where the command is `SHOW`
//! or a deep link, and exits. If it does not connect, the file is stale and
//! the later launch becomes the primary itself.
//!
//! The listener checks the token in constant time and ignores messages that
//! are larger than 8 KiB. The file is removed when the primary exits cleanly.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use rand::RngCore as _;
use tracing::{info, warn};

/// The file, in the data directory, that describes the primary instance.
pub const INSTANCE_FILE: &str = "instance.json";
/// The largest message the listener reads.
const MAX_MESSAGE_BYTES: usize = 8 * 1024;
/// How long a later launch waits to connect to the primary.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// How long a connection may stall while it is read or written.
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// The command that asks the primary to show its window.
const SHOW_COMMAND: &str = "SHOW";

/// A request another launch handed to the primary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forwarded {
    /// Show the window.
    Show,
    /// A deep link, not yet checked. The UI parses it.
    Link(String),
}

/// The outcome of [`start`].
pub enum Startup {
    /// Another instance is running and took this launch's request. The
    /// process should exit with status 0.
    Forwarded,
    /// This process is the primary instance.
    Primary {
        /// Requests from later launches.
        forwards: Receiver<Forwarded>,
        /// Removes the instance file when dropped.
        guard: InstanceGuard,
    },
}

/// Removes `instance.json` when the primary exits.
pub struct InstanceGuard {
    path: PathBuf,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            warn!(%error, "could not remove the instance file");
        }
    }
}

/// What the instance file says about the primary.
struct Record {
    port: u16,
    token: String,
}

/// Become the primary instance, or hand this launch to the running one.
///
/// `link` is the deep link this launch was started with, or `None` for a
/// plain launch, which asks the primary to show its window. An error means
/// the channel could not be set up; the caller should then run without it.
pub fn start(data_dir: &Path, link: Option<&str>) -> io::Result<Startup> {
    fs::create_dir_all(data_dir)?;
    let path = data_dir.join(INSTANCE_FILE);
    if let Some(record) = read_record(&path) {
        match forward(&record, link) {
            Ok(()) => return Ok(Startup::Forwarded),
            Err(error) => {
                info!(%error, "no running instance answered; this launch becomes the primary");
            }
        }
    }
    become_primary(&path)
}

fn become_primary(path: &Path) -> io::Result<Startup> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let token = new_token();
    write_record(path, port, &token)?;

    let (sender, forwards) = mpsc::channel();
    thread::Builder::new()
        .name("starpod-instance".to_string())
        .spawn(move || serve(listener, token, sender))?;
    info!(port, "this is the primary instance");
    Ok(Startup::Primary {
        forwards,
        guard: InstanceGuard {
            path: path.to_path_buf(),
        },
    })
}

/// Send this launch's request to the primary. Fails when nothing is listening.
fn forward(record: &Record, link: Option<&str>) -> io::Result<()> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, record.port));
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    // A line break would split the message, so it is dropped from the link.
    let command = match link {
        Some(link) => link.replace(['\r', '\n'], ""),
        None => SHOW_COMMAND.to_string(),
    };
    stream.write_all(format!("{}\n{command}\n", record.token).as_bytes())?;
    stream.flush()
}

/// Accept connections until the channel's receiver is gone.
fn serve(listener: TcpListener, token: String, sender: Sender<Forwarded>) {
    for connection in listener.incoming() {
        match connection {
            Ok(mut stream) => {
                if let Some(forward) = receive(&mut stream, &token)
                    && sender.send(forward).is_err()
                {
                    break;
                }
            }
            Err(error) => {
                warn!(%error, "the instance listener could not accept a connection");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Read one message and check its token. Returns what it asks for, if
/// anything valid.
fn receive(stream: &mut TcpStream, token: &str) -> Option<Forwarded> {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let mut buffer = Vec::new();
    let mut limited = Read::by_ref(stream).take(MAX_MESSAGE_BYTES as u64 + 1);
    if let Err(error) = limited.read_to_end(&mut buffer) {
        warn!(%error, "an instance message could not be read");
        return None;
    }
    if buffer.len() > MAX_MESSAGE_BYTES {
        warn!("an instance message was larger than 8 KiB and was dropped");
        return None;
    }
    let Ok(text) = std::str::from_utf8(&buffer) else {
        warn!("an instance message was not UTF-8 and was dropped");
        return None;
    };
    let mut lines = text.split('\n');
    let given = lines.next().unwrap_or("").trim_end_matches('\r');
    let command = lines.next().unwrap_or("").trim_end_matches('\r').trim();
    if !constant_time_eq(given.as_bytes(), token.as_bytes()) {
        warn!("an instance message had the wrong token and was ignored");
        return None;
    }
    match command {
        "" => None,
        SHOW_COMMAND => Some(Forwarded::Show),
        link => Some(Forwarded::Link(link.to_string())),
    }
}

/// Compare two byte strings without stopping at the first difference.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b)
        .fold(0u8, |difference, (x, y)| difference | (x ^ y))
        == 0
}

/// 16 random bytes as 32 lowercase hex digits.
fn new_token() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(token, "{byte:02x}");
    }
    token
}

/// Read the instance file. A missing or damaged file means no primary.
fn read_record(path: &Path) -> Option<Record> {
    let text = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let port = u16::try_from(value.get("port")?.as_u64()?).ok()?;
    let token = value.get("token")?.as_str()?.to_string();
    Some(Record { port, token })
}

/// Write the instance file through a temporary file, so a reader never sees
/// half of it.
fn write_record(path: &Path, port: u16, token: &str) -> io::Result<()> {
    let temp = path.with_extension("json.tmp");
    let body = serde_json::json!({ "port": port, "token": token }).to_string();
    {
        let mut file = fs::File::create(&temp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primary(dir: &Path) -> (Receiver<Forwarded>, InstanceGuard) {
        match start(dir, None).unwrap() {
            Startup::Primary { forwards, guard } => (forwards, guard),
            Startup::Forwarded => panic!("the first launch must become the primary"),
        }
    }

    #[test]
    fn the_tokens_are_compared_in_full() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn a_second_launch_forwards_its_link_to_the_primary() {
        let dir = tempfile::tempdir().unwrap();
        let (forwards, guard) = primary(dir.path());

        let link = "starpod://install-config?url=https%3A%2F%2Fexample.com";
        assert!(matches!(
            start(dir.path(), Some(link)).unwrap(),
            Startup::Forwarded
        ));
        let received = forwards.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(received, Forwarded::Link(link.to_string()));

        assert!(matches!(
            start(dir.path(), None).unwrap(),
            Startup::Forwarded
        ));
        let received = forwards.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(received, Forwarded::Show);

        drop(guard);
        assert!(!dir.path().join(INSTANCE_FILE).exists());
    }

    #[test]
    fn a_message_with_the_wrong_token_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let (forwards, _guard) = primary(dir.path());
        let record = read_record(&dir.path().join(INSTANCE_FILE)).unwrap();

        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, record.port)).unwrap();
        stream.write_all(b"not-the-token\nSHOW\n").unwrap();
        drop(stream);

        assert!(forwards.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn an_oversized_message_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (forwards, _guard) = primary(dir.path());
        let record = read_record(&dir.path().join(INSTANCE_FILE)).unwrap();

        let mut body = format!("{}\nstarpod://install-config?url=", record.token);
        body.push_str(&"a".repeat(MAX_MESSAGE_BYTES));
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, record.port)).unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        drop(stream);

        assert!(forwards.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn a_stale_file_is_replaced_by_a_new_primary() {
        let dir = tempfile::tempdir().unwrap();
        let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        write_record(&dir.path().join(INSTANCE_FILE), port, "stale").unwrap();

        let (_forwards, _guard) = primary(dir.path());
        let record = read_record(&dir.path().join(INSTANCE_FILE)).unwrap();
        assert_ne!(record.token, "stale");
        assert_eq!(record.token.len(), 32);
    }
}
