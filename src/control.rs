use crate::cli::Command;
use eframe::egui;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    fs,
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

const REQUEST_LIMIT: usize = 64 * 1024;
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl From<Result<Value, String>> for Response {
    fn from(result: Result<Value, String>) -> Self {
        match result {
            Ok(data) => Self {
                ok: true,
                data: Some(data),
                error: None,
            },
            Err(error) => Self {
                ok: false,
                data: None,
                error: Some(error),
            },
        }
    }
}

pub struct Pending {
    pub command: Command,
    pub reply: Sender<Response>,
    pub deadline: Instant,
}

pub struct Server {
    pub requests: Receiver<Pending>,
    path: PathBuf,
    stopping: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Server {
    // The caller holds the library lock, so any existing socket belongs to a previous run.
    pub fn start(directory: &Path, ctx: egui::Context) -> Result<Self, String> {
        private_directory(directory)?;
        let path = directory.join("control.sock");
        match fs::symlink_metadata(&path) {
            Ok(meta)
                if meta.file_type().is_socket() && meta.uid() == unsafe { libc::geteuid() } =>
            {
                fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
            Ok(_) => return Err("Refusing to replace a non-socket control.sock file.".into()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        let listener =
            UnixListener::bind(&path).map_err(|e| format!("Cannot create CLI socket: {e}"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        let (send, requests) = mpsc::sync_channel(1);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopping);
        let thread = thread::Builder::new().name("ssh-m8-cli".into()).stack_size(192 * 1024).spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::Acquire) { break; }
                let Ok(mut stream) = stream else { break; };
                let result = (|| -> Result<Response, String> {
                    same_user(&stream)?;
                    stream.set_read_timeout(Some(Duration::from_secs(3))).map_err(|e| e.to_string())?;
                    stream.set_write_timeout(Some(Duration::from_secs(3))).map_err(|e| e.to_string())?;
                    let command = read_json(&mut stream, REQUEST_LIMIT).map_err(|_| "Invalid or incomplete CLI request.")?;
                    let (reply, response) = mpsc::channel();
                    send.try_send(Pending { command, reply, deadline: Instant::now() + COMMAND_TIMEOUT })
                        .map_err(|_| "The app is busy. Try again after the current operation finishes.")?;
                    ctx.request_repaint();
                    response.recv_timeout(COMMAND_TIMEOUT).map_err(|_| "Command timed out; its outcome is unknown. Check status before retrying.".into())
                })();
                let response = result.unwrap_or_else(|error| Response::from(Err(error)));
                let _ = write_json(&mut stream, &response, RESPONSE_LIMIT);
            }
        }).map_err(|e| e.to_string())?;
        Ok(Self {
            requests,
            path,
            stopping,
            thread: Some(thread),
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let (_, empty) = mpsc::channel();
        drop(std::mem::replace(&mut self.requests, empty));
        // Wake a blocking accept without an idle polling timer.
        let _ = UnixStream::connect(&self.path);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

pub fn call(directory: &Path, command: &Command) -> Result<Response, String> {
    private_directory(directory)?;
    let path = directory.join("control.sock");
    let meta = fs::symlink_metadata(&path).map_err(|_| unavailable())?;
    if !meta.file_type().is_socket()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
    {
        return Err("The CLI socket must be owned by you and accessible only to you.".into());
    }
    let mut stream = UnixStream::connect(path).map_err(|_| unavailable())?;
    same_user(&stream)?;
    stream
        .set_read_timeout(Some(COMMAND_TIMEOUT + Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    write_json(&mut stream, command, REQUEST_LIMIT).map_err(|e| e.to_string())?;
    read_json(&mut stream, RESPONSE_LIMIT).map_err(|_| "No valid response from ssh-m8. The command's outcome is unknown; check status before retrying.".into())
}

fn unavailable() -> String {
    "Open the CLI-enabled version of ssh-m8 first, using the same SSH_M8_DATA_DIR if set.".into()
}

fn private_directory(directory: &Path) -> Result<(), String> {
    let meta = fs::metadata(directory).map_err(|_| unavailable())?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(
            "The library directory must be owned by you and accessible only to you.".into(),
        );
    }
    Ok(())
}

fn same_user(stream: &UnixStream) -> Result<(), String> {
    let mut uid = 0;
    let mut gid = 0;
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0
        || uid != unsafe { libc::geteuid() }
    {
        return Err("CLI connections must belong to the same macOS user.".into());
    }
    Ok(())
}

fn read_json<T: DeserializeOwned>(stream: &mut impl Read, limit: usize) -> io::Result<T> {
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    if length > limit {
        return Err(io::Error::other("CLI message is too large"));
    }
    let mut bytes = vec![0; length];
    let result = stream
        .read_exact(&mut bytes)
        .and_then(|()| serde_json::from_slice(&bytes).map_err(io::Error::other));
    wipe(&mut bytes);
    result
}

fn write_json(stream: &mut impl Write, value: &impl Serialize, limit: usize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let result = if bytes.len() > limit {
        Err(io::Error::other("CLI message is too large"))
    } else {
        stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .and_then(|()| stream.write_all(&bytes))
    };
    wipe(&mut bytes);
    result
}

pub fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_bounded_and_reject_malformed_json() {
        let mut large = ((REQUEST_LIMIT + 1) as u32)
            .to_be_bytes()
            .as_slice()
            .to_vec();
        assert!(read_json::<Command>(&mut large.as_slice(), REQUEST_LIMIT).is_err());
        large = vec![0, 0, 0, 1, b'{'];
        assert!(read_json::<Command>(&mut large.as_slice(), REQUEST_LIMIT).is_err());
        let mut bytes = Vec::new();
        write_json(&mut bytes, &Command::Status {}, REQUEST_LIMIT).unwrap();
        assert!(matches!(
            read_json::<Command>(&mut bytes.as_slice(), REQUEST_LIMIT).unwrap(),
            Command::Status {}
        ));
    }

    #[test]
    fn private_socket_round_trip_and_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let server = Server::start(dir.path(), egui::Context::default()).unwrap();
        assert_eq!(fs::metadata(&server.path).unwrap().mode() & 0o777, 0o600);
        let directory = dir.path().to_owned();
        let client = thread::spawn(move || call(&directory, &Command::Status {}).unwrap());
        let pending = server
            .requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(matches!(pending.command, Command::Status {}));
        pending
            .reply
            .send(Ok(serde_json::json!({"connected": 0})).into())
            .unwrap();
        assert!(client.join().unwrap().ok);
        drop(server);
        assert!(!dir.path().join("control.sock").exists());
        fs::write(dir.path().join("control.sock"), "keep").unwrap();
        assert!(Server::start(dir.path(), egui::Context::default()).is_err());
        assert_eq!(fs::read(dir.path().join("control.sock")).unwrap(), b"keep");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(dir.path()).is_err());
    }
}
