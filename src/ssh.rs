use crate::model::{Host, Tunnel, TunnelKind};
use eframe::egui;
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Disconnected,
    Connecting,
    Connected,
    Retrying,
    Failed,
}
impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Disconnected => "Offline",
            Self::Connecting => "Connecting",
            Self::Connected => "Connected",
            Self::Retrying => "Retry pending",
            Self::Failed => "Failed",
        }
    }
    pub fn active(self) -> bool {
        matches!(self, Self::Connecting | Self::Connected | Self::Retrying)
    }
}

pub enum Event {
    Status(u64, Status),
    Log {
        source: String,
        message: String,
        error: bool,
    },
}
enum Request {
    Connect(Host, Tunnel),
    Disconnect(u64),
    SetKeepRetrying(u64, bool),
    Stop,
    Wake,
}

#[derive(Clone)]
struct Wake {
    sender: Sender<Request>,
    pending: Arc<AtomicBool>,
}

impl Wake {
    fn notify(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            let _ = self.sender.send(Request::Wake);
        }
    }
}
struct Session {
    child: Child,
    output: Receiver<String>,
    started: Instant,
    ready: bool,
    output_closed: Arc<AtomicBool>,
}

const RETRY_DELAY: Duration = Duration::from_secs(30);

struct Connection {
    host: Host,
    tunnel: Tunnel,
    session: Option<Session>,
    retry_at: Instant,
}

pub struct Runtime {
    requests: Sender<Request>,
    events: Arc<Mutex<EventBuffer>>,
    worker: Option<thread::JoinHandle<()>>,
    directory: Arc<tempfile::TempDir>,
    library_directory: PathBuf,
}

#[derive(Default)]
struct EventBuffer {
    queue: VecDeque<Event>,
    logs: usize,
}

impl EventBuffer {
    fn push(&mut self, event: Event) {
        match &event {
            Event::Status(id, _) => self
                .queue
                .retain(|e| !matches!(e, Event::Status(previous, _) if previous == id)),
            Event::Log { .. } => {
                if self.logs == 300
                    && let Some(index) = self
                        .queue
                        .iter()
                        .position(|e| matches!(e, Event::Log { .. }))
                {
                    self.queue.remove(index);
                    self.logs -= 1;
                }
                self.logs += 1;
            }
        }
        self.queue.push_back(event);
    }
}

impl Runtime {
    pub fn new(ctx: egui::Context, library_directory: PathBuf) -> Result<Self, String> {
        let directory = Arc::new(
            tempfile::Builder::new()
                .prefix("sshm-")
                .tempdir_in("/tmp")
                .map_err(|e| e.to_string())?,
        );
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        write_executable(
            &directory.path().join("askpass"),
            &format!(
                "#!/bin/sh\nexec {} --askpass \"$@\"\n",
                quote(&executable.to_string_lossy())
            ),
        )?;
        let (requests, receive) = mpsc::channel();
        let events = Arc::new(Mutex::new(EventBuffer::default()));
        let send = Arc::clone(&events);
        let wake = Wake {
            sender: requests.clone(),
            pending: Arc::new(AtomicBool::new(false)),
        };
        let dir = Arc::clone(&directory);
        let library = library_directory.clone();
        let worker = thread::Builder::new()
            .name("ssh-m8".into())
            .stack_size(256 * 1024)
            .spawn(move || worker(receive, wake, send, ctx, dir, library))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            requests,
            events,
            worker: Some(worker),
            directory,
            library_directory,
        })
    }

    pub fn connect(&self, host: Host, tunnel: Tunnel) {
        let _ = self.requests.send(Request::Connect(host, tunnel));
    }
    pub fn disconnect(&self, id: u64) {
        let _ = self.requests.send(Request::Disconnect(id));
    }
    pub fn set_keep_retrying(&self, id: u64, enabled: bool) {
        let _ = self.requests.send(Request::SetKeepRetrying(id, enabled));
    }

    pub fn drain_events(&self) -> VecDeque<Event> {
        let mut events = self.events.lock().unwrap();
        events.logs = 0;
        std::mem::take(&mut events.queue)
    }

    pub fn terminal(&self, host: &Host) -> Result<(), String> {
        host.validate()?;
        let mut command = String::from("#!/bin/sh\n");
        command.push_str("trap 'stty echo 2>/dev/null' EXIT\ntrap 'exit 130' INT TERM HUP\n");
        // The script removes itself as soon as Terminal opens it.
        command.push_str("rm -- \"$0\"\n");
        if host.saved_password {
            command.push_str(&format!(
                "export SSH_ASKPASS={}\nexport SSH_ASKPASS_REQUIRE=force\nexport DISPLAY=ssh-m8\nexport SSH_M8_CREDENTIAL_ACCOUNT={}\nexport SSH_M8_INTERACTIVE=1\n",
                quote(&self.directory.path().join("askpass").to_string_lossy()),
                quote(&account(&self.library_directory, host.id))));
        }
        command.push_str("'/usr/bin/ssh' ");
        command.push_str(
            &host_args(host)
                .iter()
                .map(|s| quote(s))
                .collect::<Vec<_>>()
                .join(" "),
        );
        command.push_str(" '-o' 'StrictHostKeyChecking=ask' '--' ");
        command.push_str(&quote(&host.address));
        command.push_str("\nresult=$?\nif [ \"$result\" -ne 0 ]; then\n  printf '\\nSSH exited with status %s. Press Return to close.\\n' \"$result\"\n  read -r reply\nfi\nexit \"$result\"\n");
        let file = tempfile::Builder::new()
            .prefix("session-")
            .suffix(".command")
            .tempfile_in(self.directory.path())
            .map_err(|e| e.to_string())?;
        let (_, path) = file.keep().map_err(|e| e.to_string())?;
        fs::remove_file(&path).map_err(|e| e.to_string())?;
        write_executable(&path, &command)?;
        let status = Command::new("/usr/bin/open")
            .args(["-a", "Terminal"])
            .arg(&path)
            .status()
            .map_err(|e| format!("Cannot open Terminal: {e}"))?;
        if !status.success() {
            return Err("macOS could not open Terminal.".into());
        }
        Ok(())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn account(directory: &Path, id: u64) -> String {
    format!("{}:{id}", directory.display())
}

pub fn host_args(host: &Host) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        &host.port.to_string(),
        "-o",
        "ConnectTimeout=12",
        "-o",
        "ConnectionAttempts=1",
        "-o",
        "ServerAliveInterval=30",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPath=none",
        "-o",
        "ControlPersist=no",
        "-o",
        "ForkAfterAuthentication=no",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ForwardX11=no",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    if !host.username.is_empty() {
        args.extend(["-l".into(), host.username.clone()]);
    }
    if !host.identity.is_empty() {
        let identity = if let Some(suffix) = host.identity.strip_prefix("~/") {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(suffix)
                .to_string_lossy()
                .into_owned()
        } else {
            host.identity.clone()
        };
        args.extend([
            "-i".into(),
            identity,
            "-o".into(),
            "IdentitiesOnly=yes".into(),
        ]);
    }
    args
}

fn tunnel_command(host: &Host, tunnel: &Tunnel, helper: &Path, library: &Path) -> Command {
    let mut command = Command::new("/usr/bin/ssh");
    command.args(host_args(host));
    let (flag, forwarding) = tunnel.forwarding();
    command.args([
        "-N",
        "-T",
        "-v",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "NumberOfPasswordPrompts=1",
        "-o",
        "RequestTTY=no",
        "-o",
        "SessionType=none",
        "-o",
        if host.saved_password {
            "BatchMode=no"
        } else {
            "BatchMode=yes"
        },
        &flag,
        &forwarding,
        "--",
        &host.address,
    ]);
    if host.saved_password {
        command
            .env("SSH_ASKPASS", helper)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("DISPLAY", "ssh-m8")
            .env("SSH_M8_CREDENTIAL_ACCOUNT", account(library, host.id))
            .env_remove("SSH_M8_INTERACTIVE");
    }
    command
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

fn ready_line(kind: TunnelKind, line: &str) -> bool {
    // shortcut: readiness uses OpenSSH's debug messages; update if OpenSSH changes their wording.
    match kind {
        TunnelKind::Remote => line.starts_with("debug1: remote forward success for:"),
        _ => line.starts_with("debug1: Entering interactive session."),
    }
}

fn read_ssh_line(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    let mut seen = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(seen.then(|| String::from_utf8_lossy(&line).into_owned()));
        }
        seen = true;
        let end = buffer
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buffer.len(), |n| n + 1);
        let complete = buffer[end - 1] == b'\n';
        let keep = end.min(2000 - line.len());
        line.extend_from_slice(&buffer[..keep]);
        reader.consume(end);
        if complete {
            return Ok(Some(
                String::from_utf8_lossy(&line)
                    .trim_end_matches(['\r', '\n'])
                    .into(),
            ));
        }
    }
}

fn start_session(
    host: &Host,
    tunnel: &Tunnel,
    helper: &Path,
    library: &Path,
    wake: &Wake,
) -> Result<Session, String> {
    let mut child = tunnel_command(host, tunnel, helper, library)
        .spawn()
        .map_err(|error| format!("Cannot start SSH: {error}"))?;
    let stderr = child.stderr.take().expect("piped stderr");
    let (send_lines, output) = mpsc::sync_channel(64);
    let output_closed = Arc::new(AtomicBool::new(false));
    let closed = Arc::clone(&output_closed);
    let wake = wake.clone();
    let kind = tunnel.kind;
    let reader = thread::Builder::new()
        .name("ssh-output".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            let mut reader = BufReader::new(stderr);
            while let Ok(Some(line)) = read_ssh_line(&mut reader) {
                if ready_line(kind, &line)
                    || (!line.starts_with("debug") && !line.trim().is_empty())
                {
                    if send_lines.send(line).is_err() {
                        break;
                    }
                    wake.notify();
                }
            }
            closed.store(true, Ordering::Release);
            wake.notify();
        });
    if let Err(error) = reader {
        stop(&mut child);
        return Err(error.to_string());
    }
    Ok(Session {
        child,
        output,
        started: Instant::now(),
        ready: false,
        output_closed,
    })
}

impl Connection {
    fn set_keep_retrying(&mut self, enabled: bool) -> bool {
        self.tunnel.keep_retrying = enabled;
        // A live session stays up; disabling retry only removes a waiting connection.
        enabled || self.session.is_some()
    }

    fn wait(&self, now: Instant) -> Option<Duration> {
        match &self.session {
            Some(session) if session.output_closed.load(Ordering::Acquire) => {
                Some(Duration::from_millis(250))
            }
            Some(session) if !session.ready => {
                Some((session.started + Duration::from_secs(30)).saturating_duration_since(now))
            }
            Some(_) => None,
            None => Some(self.retry_at.saturating_duration_since(now)),
        }
    }

    fn failed(&mut self, now: Instant, message: String, emit: &impl Fn(Event)) -> bool {
        if let Some(mut session) = self.session.take() {
            stop(&mut session.child);
        }
        emit(Event::Log {
            source: self.tunnel.name.clone(),
            message,
            error: true,
        });
        let retry = self.tunnel.keep_retrying;
        emit(Event::Status(
            self.tunnel.id,
            if retry {
                Status::Retrying
            } else {
                Status::Failed
            },
        ));
        if retry {
            self.retry_at = now + RETRY_DELAY;
            emit(Event::Log {
                source: self.tunnel.name.clone(),
                message: "Retrying in 30 seconds. Disconnect to cancel.".into(),
                error: false,
            });
        }
        retry
    }

    fn poll(
        &mut self,
        now: Instant,
        helper: &Path,
        library: &Path,
        wake: &Wake,
        emit: &impl Fn(Event),
    ) -> bool {
        if self.session.is_none() {
            if now < self.retry_at {
                return true;
            }
            emit(Event::Status(self.tunnel.id, Status::Connecting));
            match start_session(&self.host, &self.tunnel, helper, library, wake) {
                Ok(session) => {
                    self.session = Some(session);
                    emit(Event::Log {
                        source: self.tunnel.name.clone(),
                        message: format!(
                            "Connecting through {}. {}",
                            self.host.name,
                            self.tunnel.route()
                        ),
                        error: false,
                    });
                }
                Err(error) => return self.failed(now, error, emit),
            }
        }
        let session = self.session.as_mut().expect("started session");
        for line in session.output.try_iter().take(64) {
            if !session.ready && ready_line(self.tunnel.kind, &line) {
                session.ready = true;
                emit(Event::Status(self.tunnel.id, Status::Connected));
                emit(Event::Log {
                    source: self.tunnel.name.clone(),
                    message: "Tunnel connected.".into(),
                    error: false,
                });
            } else if !line.starts_with("debug") && !line.trim().is_empty() {
                emit(Event::Log {
                    source: self.tunnel.name.clone(),
                    message: line,
                    error: true,
                });
            }
        }
        let error = match session.child.try_wait() {
            Ok(Some(code)) => format!("SSH exited ({code}). For an unknown host, connect from Hosts to verify its fingerprint. Check saved credentials and port availability."),
            Ok(None) if !session.ready && now.saturating_duration_since(session.started) >= Duration::from_secs(30) =>
                "Connection timed out. Check the host, credentials, and any pending macOS password access prompt.".into(),
            Ok(None) => return true,
            Err(error) => error.to_string(),
        };
        self.failed(now, error, emit)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(session) = &mut self.session {
            stop(&mut session.child);
        }
    }
}

fn worker(
    requests: Receiver<Request>,
    wake: Wake,
    events: Arc<Mutex<EventBuffer>>,
    ctx: egui::Context,
    directory: Arc<tempfile::TempDir>,
    library: PathBuf,
) {
    let mut connections = HashMap::<u64, Connection>::new();
    let emit = |event| {
        events.lock().unwrap().push(event);
        ctx.request_repaint();
    };
    let helper = directory.path().join("askpass");
    loop {
        let now = Instant::now();
        let timeout = connections.values().filter_map(|c| c.wait(now)).min();
        // SSH output wakes the worker; only connection deadlines and retries need timers.
        let request = match timeout {
            Some(timeout) => match requests.recv_timeout(timeout) {
                Ok(request) => Some(request),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match requests.recv() {
                Ok(request) => Some(request),
                Err(_) => break,
            },
        };
        match request {
            Some(Request::Stop) => break,
            Some(Request::Disconnect(id)) => {
                if let Some(connection) = connections.remove(&id) {
                    emit(Event::Log {
                        source: connection.tunnel.name.clone(),
                        message: "Tunnel disconnected. Pending retries cancelled.".into(),
                        error: false,
                    });
                }
                emit(Event::Status(id, Status::Disconnected));
            }
            Some(Request::Connect(host, tunnel)) => {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    connections.entry(tunnel.id)
                {
                    if let Err(error) = host
                        .validate()
                        .and_then(|_| tunnel.validate(std::slice::from_ref(&host)))
                    {
                        emit(Event::Status(tunnel.id, Status::Failed));
                        emit(Event::Log {
                            source: tunnel.name,
                            message: error,
                            error: true,
                        });
                    } else {
                        entry.insert(Connection {
                            host,
                            tunnel,
                            session: None,
                            retry_at: Instant::now(),
                        });
                    }
                }
            }
            Some(Request::SetKeepRetrying(id, enabled)) => {
                if let Some(connection) = connections.get_mut(&id)
                    && !connection.set_keep_retrying(enabled)
                {
                    let connection = connections.remove(&id).expect("waiting connection");
                    emit(Event::Status(id, Status::Disconnected));
                    emit(Event::Log {
                        source: connection.tunnel.name.clone(),
                        message: "Reconnect disabled. Pending retry cancelled.".into(),
                        error: false,
                    });
                }
            }
            Some(Request::Wake) => wake.pending.store(false, Ordering::Release),
            None => {}
        }
        let now = Instant::now();
        connections.retain(|_, connection| connection.poll(now, &helper, &library, &wake, &emit));
    }
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn write_executable(path: &Path, body: &str) -> Result<(), String> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(path)
        .and_then(|mut file| file.write_all(body.as_bytes()))
        .map_err(|e| e.to_string())
}

pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};

    struct TestProcess(Child);
    impl Drop for TestProcess {
        fn drop(&mut self) {
            stop(&mut self.0);
        }
    }

    fn unused_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn retry_changes_preserve_the_live_process_and_remove_only_waiting_connections() {
        let child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let (_, output) = mpsc::channel();
        let mut connection = Connection {
            host: Host::new(1),
            tunnel: Tunnel::new(2, 1),
            retry_at: Instant::now(),
            session: Some(Session {
                child,
                output,
                started: Instant::now(),
                ready: true,
                output_closed: Arc::new(AtomicBool::new(false)),
            }),
        };
        assert!(connection.set_keep_retrying(true));
        assert!(connection.tunnel.keep_retrying);
        assert!(connection.set_keep_retrying(false));
        assert!(!connection.tunnel.keep_retrying);
        let session = connection.session.as_mut().unwrap();
        assert_eq!(session.child.id(), pid);
        assert!(session.child.try_wait().unwrap().is_none());
        stop(&mut session.child);
        connection.session = None;
        assert!(connection.set_keep_retrying(true));
        assert!(!connection.set_keep_retrying(false));
    }

    #[test]
    fn noisy_ssh_output_queues_only_one_wakeup() {
        let (sender, requests) = mpsc::channel();
        let wake = Wake {
            sender,
            pending: Arc::new(AtomicBool::new(false)),
        };
        for _ in 0..10_000 {
            wake.notify();
        }
        assert_eq!(requests.try_iter().count(), 1);
        wake.pending.store(false, Ordering::Release);
        wake.notify();
        assert!(matches!(requests.try_recv(), Ok(Request::Wake)));
    }

    #[test]
    fn idle_connections_have_no_poll_timer_and_connecting_has_a_deadline() {
        let now = Instant::now();
        let child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let (_, output) = mpsc::channel();
        let mut connection = Connection {
            host: Host::new(1),
            tunnel: Tunnel::new(2, 1),
            retry_at: now,
            session: Some(Session {
                child,
                output,
                started: now,
                ready: true,
                output_closed: Arc::new(AtomicBool::new(false)),
            }),
        };
        assert_eq!(connection.wait(now), None);
        connection.session.as_mut().unwrap().ready = false;
        assert_eq!(connection.wait(now), Some(Duration::from_secs(30)));
        assert_eq!(
            connection.wait(now + Duration::from_secs(30)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn pending_logs_are_bounded_and_latest_status_is_preserved() {
        let mut buffer = EventBuffer::default();
        buffer.push(Event::Status(1, Status::Connecting));
        for i in 0..10_000 {
            buffer.push(Event::Log {
                source: "test".into(),
                message: i.to_string(),
                error: false,
            });
        }
        buffer.push(Event::Status(1, Status::Connected));
        assert_eq!(buffer.logs, 300);
        assert_eq!(buffer.queue.len(), 301);
        assert!(matches!(
            buffer.queue.back(),
            Some(Event::Status(1, Status::Connected))
        ));
        assert!(
            matches!(buffer.queue.front(), Some(Event::Log {message, ..}) if message == "9700")
        );
    }

    #[test]
    fn ssh_lines_are_bounded_even_without_a_newline() {
        let mut data = vec![b'x'; 1_000_000];
        data.extend_from_slice(b"\r\nnext\nlast");
        let mut reader = BufReader::with_capacity(128, std::io::Cursor::new(data));
        assert_eq!(read_ssh_line(&mut reader).unwrap().unwrap().len(), 2000);
        assert_eq!(read_ssh_line(&mut reader).unwrap().as_deref(), Some("next"));
        assert_eq!(read_ssh_line(&mut reader).unwrap().as_deref(), Some("last"));
        assert!(read_ssh_line(&mut reader).unwrap().is_none());
    }

    #[test]
    fn dropped_sessions_retry_after_thirty_seconds_only_when_enabled() {
        let now = Instant::now();
        for ready in [false, true] {
            for keep_retrying in [false, true] {
                let mut child = Command::new("/usr/bin/true").spawn().unwrap();
                child.wait().unwrap();
                let (_, output) = mpsc::channel();
                let mut tunnel = Tunnel::new(2, 1);
                tunnel.keep_retrying = keep_retrying;
                let mut connection = Connection {
                    host: Host::new(1),
                    tunnel,
                    session: Some(Session {
                        child,
                        output,
                        started: now,
                        ready,
                        output_closed: Arc::new(AtomicBool::new(true)),
                    }),
                    retry_at: now,
                };
                let (send, events) = mpsc::channel();
                let (sender, _) = mpsc::channel();
                let wake = Wake {
                    sender,
                    pending: Arc::new(AtomicBool::new(false)),
                };
                let emit = |event| send.send(event).unwrap();
                assert_eq!(
                    connection.poll(
                        now,
                        Path::new("/unused"),
                        Path::new("/unused"),
                        &wake,
                        &emit
                    ),
                    keep_retrying
                );
                assert!(connection.session.is_none());
                let status = if keep_retrying {
                    Status::Retrying
                } else {
                    Status::Failed
                };
                let received: Vec<_> = events.try_iter().collect();
                assert!(
                    received
                        .iter()
                        .any(|event| matches!(event, Event::Status(2, s) if *s == status))
                );
                if keep_retrying {
                    assert_eq!(connection.wait(now), Some(RETRY_DELAY));
                    assert_eq!(connection.wait(now + RETRY_DELAY), Some(Duration::ZERO));
                    assert!(connection.poll(
                        now + Duration::from_secs(29),
                        Path::new("/unused"),
                        Path::new("/unused"),
                        &wake,
                        &emit
                    ));
                    assert!(events.try_recv().is_err(), "No attempt before the deadline");
                    assert!(connection.session.is_none());
                }
            }
        }
    }

    #[test]
    #[ignore = "Exercises real 30-second retry and cancellation timers on loopback"]
    fn worker_retries_and_disconnect_cancels_future_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = Runtime::new(egui::Context::default(), dir.path().to_owned()).unwrap();
        let mut host = Host::new(1);
        host.name = "Retry test".into();
        host.address = "127.0.0.1".into();
        host.port = unused_port();
        let mut tunnel = Tunnel::new(2, 1);
        tunnel.name = "Retry test".into();
        tunnel.keep_retrying = true;
        let wait_for = |matches: &dyn Fn(&Event) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(40);
            loop {
                let event = {
                    let mut buffer = runtime.events.lock().unwrap();
                    let event = buffer.queue.pop_front();
                    if matches!(event, Some(Event::Log { .. })) {
                        buffer.logs -= 1;
                    }
                    event
                };
                if let Some(event) = event
                    && matches(&event)
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "Expected worker event did not arrive"
                );
                thread::sleep(Duration::from_millis(10));
            }
        };
        let connecting = |event: &Event| matches!(event, Event::Log {message, ..} if message.starts_with("Connecting through"));
        let retrying = |event: &Event| matches!(event, Event::Status(2, Status::Retrying));
        runtime.connect(host.clone(), tunnel.clone());
        wait_for(&connecting);
        wait_for(&retrying);
        runtime.set_keep_retrying(2, false);
        wait_for(&|event| matches!(event, Event::Status(2, Status::Disconnected)));
        runtime.connect(host.clone(), tunnel.clone());
        wait_for(&connecting);
        wait_for(&retrying);
        let failed_at = Instant::now();
        runtime.connect(host, tunnel); // A duplicate Connect must not bypass the retry timer.
        wait_for(&connecting);
        assert!(failed_at.elapsed() >= RETRY_DELAY - Duration::from_millis(100));
        wait_for(&retrying);
        runtime.disconnect(2);
        wait_for(&|event| matches!(event, Event::Status(2, Status::Disconnected)));
        let deadline = Instant::now() + RETRY_DELAY + Duration::from_secs(1);
        while Instant::now() < deadline {
            assert!(
                !runtime.drain_events().iter().any(&connecting),
                "Disconnect did not cancel retry"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    #[ignore = "Starts a disposable loopback SSH server; requires local network access"]
    fn live_local_remote_and_socks_forwarding() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["host_key", "client_key"] {
            assert!(
                Command::new("/usr/bin/ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                    .arg(dir.path().join(name))
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let ssh_port = unused_port();
        let user = std::env::var("USER").unwrap();
        let config = format!(
            "ListenAddress 127.0.0.1\nPort {ssh_port}\nHostKey \"{0}/host_key\"\nAuthorizedKeysFile \"{0}/client_key.pub\"\nPidFile \"{0}/pid\"\nStrictModes no\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nAllowUsers {user}\nAllowTcpForwarding yes\nGatewayPorts no\nLogLevel ERROR\n",
            dir.path().display()
        );
        fs::write(dir.path().join("config"), config).unwrap();
        fs::write(
            dir.path().join("known_hosts"),
            format!(
                "[127.0.0.1]:{ssh_port} {}",
                fs::read_to_string(dir.path().join("host_key.pub")).unwrap()
            ),
        )
        .unwrap();
        let mut server = TestProcess(
            Command::new("/usr/sbin/sshd")
                .args(["-D", "-e", "-f"])
                .arg(dir.path().join("config"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut listening = false;
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", ssh_port)).is_ok() {
                listening = true;
                break;
            }
            if server.0.try_wait().unwrap().is_some() {
                let mut error = String::new();
                server
                    .0
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut error)
                    .unwrap();
                panic!("Test sshd failed: {error}");
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(listening, "Test SSH server did not start");
        let mut host = Host::new(1);
        host.name = "Loopback test".into();
        host.address = "127.0.0.1".into();
        host.port = ssh_port;
        host.username = user;
        host.identity = dir.path().join("client_key").to_string_lossy().into_owned();
        for kind in [TunnelKind::Local, TunnelKind::Remote, TunnelKind::Dynamic] {
            let echo = TcpListener::bind("127.0.0.1:0").unwrap();
            echo.set_nonblocking(true).unwrap();
            let mut tunnel = Tunnel::new(2, 1);
            tunnel.kind = kind;
            tunnel.listen_port = unused_port();
            tunnel.destination_port = echo.local_addr().unwrap().port();
            let base = tunnel_command(&host, &tunnel, Path::new("/unused"), dir.path());
            let mut command = Command::new(base.get_program());
            command
                .args(["-F", "/dev/null", "-o"])
                .arg(format!(
                    "UserKnownHostsFile={}",
                    dir.path().join("known_hosts").display()
                ))
                .args(base.get_args())
                .env("LC_ALL", "C")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            let mut process = TestProcess(command.spawn().unwrap());
            let stderr = process.0.stderr.take().unwrap();
            let (send, receive) = mpsc::channel();
            thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if send.send(line).is_err() {
                        break;
                    }
                }
            });
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut ready = false;
            let mut output = String::new();
            while Instant::now() < deadline {
                match receive.recv_timeout(Duration::from_millis(100)) {
                    Ok(line) => {
                        output.push_str(&line);
                        output.push('\n');
                        if ready_line(kind, &line) {
                            ready = true;
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(_) => {}
                }
            }
            assert!(ready, "{kind:?} failed:\n{output}");
            let mut client = TcpStream::connect(("127.0.0.1", tunnel.listen_port)).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            if kind == TunnelKind::Dynamic {
                client.write_all(&[5, 1, 0]).unwrap();
                let mut response = [0; 2];
                client.read_exact(&mut response).unwrap();
                assert_eq!(response, [5, 0]);
                let port = tunnel.destination_port.to_be_bytes();
                client
                    .write_all(&[5, 1, 0, 1, 127, 0, 0, 1, port[0], port[1]])
                    .unwrap();
                let mut response = [0; 10];
                client.read_exact(&mut response).unwrap();
                assert_eq!(response[1], 0);
            }
            client.write_all(b"ping").unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut received = false;
            while Instant::now() < deadline {
                match echo.accept() {
                    Ok((mut remote, _)) => {
                        remote.set_nonblocking(false).unwrap();
                        remote
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        let mut data = [0; 4];
                        remote.read_exact(&mut data).unwrap();
                        assert_eq!(&data, b"ping");
                        remote.write_all(b"pong").unwrap();
                        received = true;
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
            assert!(received);
            let mut data = [0; 4];
            client.read_exact(&mut data).unwrap();
            assert_eq!(&data, b"pong");
            stop(&mut process.0);
            assert!(TcpStream::connect(("127.0.0.1", tunnel.listen_port)).is_err());
        }
    }
    #[test]
    fn shell_quotes_preserve_metacharacters() {
        let value = "path with 'quotes' $(not-a-command); `anything`";
        let result = Command::new("/bin/sh")
            .args(["-c", &format!("printf %s {}", quote(value))])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(result.stdout).unwrap(), value);
    }
    #[test]
    fn commands_keep_passwords_out_of_arguments_and_require_host_verification() {
        let mut host = Host::new(1);
        host.address = "test.example".into();
        host.saved_password = true;
        let tunnel = Tunnel::new(2, 1);
        let command = tunnel_command(
            &host,
            &tunnel,
            Path::new("/tmp/helper"),
            Path::new("/tmp/library"),
        );
        let args: Vec<_> = command.get_args().map(|s| s.to_string_lossy()).collect();
        assert!(args.contains(&"StrictHostKeyChecking=yes".into()));
        assert!(args.contains(&"ExitOnForwardFailure=yes".into()));
        assert!(args.contains(&"ControlPath=none".into()));
        assert!(args.contains(&"127.0.0.1:8080:127.0.0.1:80".into()));
        assert_eq!(&args[args.len() - 2..], ["--", "test.example"]);
    }
    #[test]
    fn remote_tunnel_waits_for_forward_confirmation() {
        assert!(!ready_line(
            TunnelKind::Remote,
            "debug1: Entering interactive session."
        ));
        assert!(ready_line(
            TunnelKind::Remote,
            "debug1: remote forward success for: listen 127.0.0.1:8080"
        ));
        assert!(!ready_line(
            TunnelKind::Local,
            "Authenticated to example.org"
        ));
    }
}
