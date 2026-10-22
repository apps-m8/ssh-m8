use crate::{
    control::{self, Response},
    model::{Host, Tunnel, TunnelKind},
    storage,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{self, IsTerminal, Read},
};

pub const HELP: &str = "ssh-m8 [--json] COMMAND

No arguments opens the Mac app. Commands control the running app and return JSON.

  status
  hosts list
  hosts add --name NAME --address HOST [--port PORT] [--user USER] [--identity PATH]
  hosts edit ID [--name NAME] [--address HOST] [--port PORT] [--user USER] [--identity PATH]
  hosts remove ID
  hosts open ID                         Open an interactive SSH session in Terminal
  hosts set-password ID --stdin         Read a password from piped stdin; never echo it
  hosts clear-password ID
  tunnels list
  tunnels add --name NAME --host ID --kind local|remote|dynamic --listen PORT
              [--destination HOST] [--destination-port PORT]
              [--auto-connect true|false] [--keep-retrying true|false]
  tunnels edit ID [the same options as add]
  tunnels remove ID
  tunnels connect ID|all
  tunnels disconnect ID|all
  logs [--limit 1..300]
  agent-help                           Print the agent guide without opening the app
  --help
  --version

Local/remote tunnels require --destination-port; destination defaults to 127.0.0.1.
New tunnels default to manual connection and no retries. Edit preserves omitted fields.
Connect/disconnect are asynchronous: check status until the requested state is reached.
Use numeric IDs from list/status, not array positions. Passwords cannot be read back.
Exit codes: 0 success, 1 app rejected command, 2 invalid arguments, 3 app unavailable/transport error.
SSH_M8_DATA_DIR selects an isolated library; the app and CLI must use the same directory.
See docs/agents.md for the JSON contract and agent workflow.
";

#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(pub String);

impl Drop for Secret {
    fn drop(&mut self) {
        // Zeroes are valid UTF-8; clear the owned password before releasing its buffer.
        control::wipe(unsafe { self.0.as_bytes_mut() });
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Command {
    Status {},
    Hosts {},
    Tunnels {},
    Logs { limit: usize },
    AddHost { fields: HostFields },
    EditHost { id: u64, fields: HostFields },
    RemoveHost { id: u64 },
    OpenHost { id: u64 },
    SetPassword { id: u64, password: Secret },
    ClearPassword { id: u64 },
    AddTunnel { fields: TunnelFields },
    EditTunnel { id: u64, fields: TunnelFields },
    RemoveTunnel { id: u64 },
    Connect { id: Option<u64> },
    Disconnect { id: Option<u64> },
}

impl Command {
    pub fn changes_library(&self) -> bool {
        !matches!(
            self,
            Self::Status {}
                | Self::Hosts {}
                | Self::Tunnels {}
                | Self::Logs { .. }
                | Self::OpenHost { .. }
                | Self::Connect { .. }
                | Self::Disconnect { .. }
        )
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostFields {
    pub name: Option<String>,
    pub address: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub identity: Option<String>,
}

impl HostFields {
    pub fn apply(self, host: &mut Host) {
        if let Some(v) = self.name {
            host.name = v;
        }
        if let Some(v) = self.address {
            host.address = v;
        }
        if let Some(v) = self.port {
            host.port = v;
        }
        if let Some(v) = self.user {
            host.username = v;
        }
        if let Some(v) = self.identity {
            host.identity = v;
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunnelFields {
    pub name: Option<String>,
    pub host: Option<u64>,
    pub kind: Option<TunnelKind>,
    pub listen: Option<u16>,
    pub destination: Option<String>,
    pub destination_port: Option<u16>,
    pub auto_connect: Option<bool>,
    pub keep_retrying: Option<bool>,
}

impl TunnelFields {
    pub fn options_only(&self) -> bool {
        (self.auto_connect.is_some() || self.keep_retrying.is_some())
            && self.name.is_none()
            && self.host.is_none()
            && self.kind.is_none()
            && self.listen.is_none()
            && self.destination.is_none()
            && self.destination_port.is_none()
    }

    pub fn apply(self, tunnel: &mut Tunnel) {
        if let Some(v) = self.name {
            tunnel.name = v;
        }
        if let Some(v) = self.host {
            tunnel.host_id = v;
        }
        if let Some(v) = self.kind {
            tunnel.kind = v;
        }
        if let Some(v) = self.listen {
            tunnel.listen_port = v;
        }
        if let Some(v) = self.destination {
            tunnel.destination = v;
        }
        if let Some(v) = self.destination_port {
            tunnel.destination_port = v;
        }
        if let Some(v) = self.auto_connect {
            tunnel.auto_connect = v;
        }
        if let Some(v) = self.keep_retrying {
            tunnel.keep_retrying = v;
        }
    }
}

fn id(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or("Expected a positive numeric ID.".into())
}

fn options(args: &[String], allowed: &[&str]) -> Result<HashMap<String, String>, String> {
    let mut fields = HashMap::new();
    for pair in args.chunks(2) {
        if pair.len() != 2 || !allowed.contains(&pair[0].as_str()) {
            return Err("Unknown option or missing value. Run ssh-m8 --help.".into());
        }
        if fields.insert(pair[0].clone(), pair[1].clone()).is_some() {
            return Err(format!("Duplicate option: {}", pair[0]));
        }
    }
    Ok(fields)
}

fn port(value: Option<String>) -> Result<Option<u16>, String> {
    value
        .map(|v| {
            v.parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or("Ports must be between 1 and 65535.".into())
        })
        .transpose()
}

fn boolean(value: Option<String>) -> Result<Option<bool>, String> {
    value
        .map(|v| match v.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err("Boolean options require true or false.".into()),
        })
        .transpose()
}

fn host_fields(args: &[String], adding: bool) -> Result<HostFields, String> {
    let mut opts = options(
        args,
        &["--name", "--address", "--port", "--user", "--identity"],
    )?;
    if adding && (!opts.contains_key("--name") || !opts.contains_key("--address")) {
        return Err("Adding a host requires --name and --address.".into());
    }
    if opts.is_empty() {
        return Err("Provide at least one field to edit.".into());
    }
    Ok(HostFields {
        name: opts.remove("--name"),
        address: opts.remove("--address"),
        port: port(opts.remove("--port"))?,
        user: opts.remove("--user"),
        identity: opts.remove("--identity"),
    })
}

fn tunnel_fields(args: &[String], adding: bool) -> Result<TunnelFields, String> {
    let mut opts = options(
        args,
        &[
            "--name",
            "--host",
            "--kind",
            "--listen",
            "--destination",
            "--destination-port",
            "--auto-connect",
            "--keep-retrying",
        ],
    )?;
    if adding
        && ["--name", "--host", "--kind", "--listen"]
            .iter()
            .any(|key| !opts.contains_key(*key))
    {
        return Err("Adding a tunnel requires --name, --host, --kind, and --listen.".into());
    }
    if opts.is_empty() {
        return Err("Provide at least one field to edit.".into());
    }
    let kind = opts
        .remove("--kind")
        .map(|v| match v.as_str() {
            "local" => Ok(TunnelKind::Local),
            "remote" => Ok(TunnelKind::Remote),
            "dynamic" => Ok(TunnelKind::Dynamic),
            _ => Err("Kind must be local, remote, or dynamic."),
        })
        .transpose()?;
    if adding && kind != Some(TunnelKind::Dynamic) && !opts.contains_key("--destination-port") {
        return Err("Local and remote tunnels require --destination-port.".into());
    }
    Ok(TunnelFields {
        name: opts.remove("--name"),
        host: opts.remove("--host").map(|v| id(&v)).transpose()?,
        kind,
        listen: port(opts.remove("--listen"))?,
        destination: opts.remove("--destination"),
        destination_port: port(opts.remove("--destination-port"))?,
        auto_connect: boolean(opts.remove("--auto-connect"))?,
        keep_retrying: boolean(opts.remove("--keep-retrying"))?,
    })
}

fn parse(args: &[String]) -> Result<Command, String> {
    let args = if args.first().is_some_and(|a| a == "--json") {
        &args[1..]
    } else {
        args
    };
    let parts: Vec<_> = args.iter().map(String::as_str).collect();
    match parts.as_slice() {
        ["status"] => Ok(Command::Status {}),
        ["hosts", "list"] => Ok(Command::Hosts {}),
        ["tunnels", "list"] => Ok(Command::Tunnels {}),
        ["logs"] => Ok(Command::Logs { limit: 50 }),
        ["logs", "--limit", value] => {
            let limit = value
                .parse::<usize>()
                .ok()
                .filter(|v| (1..=300).contains(v))
                .ok_or("Log limit must be between 1 and 300.")?;
            Ok(Command::Logs { limit })
        }
        ["hosts", "add", ..] => Ok(Command::AddHost {
            fields: host_fields(&args[2..], true)?,
        }),
        ["hosts", "edit", value, ..] => Ok(Command::EditHost {
            id: id(value)?,
            fields: host_fields(&args[3..], false)?,
        }),
        ["hosts", "remove", value] => Ok(Command::RemoveHost { id: id(value)? }),
        ["hosts", "open", value] => Ok(Command::OpenHost { id: id(value)? }),
        ["hosts", "set-password", value, "--stdin"] => Ok(Command::SetPassword {
            id: id(value)?,
            password: Secret(String::new()),
        }),
        ["hosts", "clear-password", value] => Ok(Command::ClearPassword { id: id(value)? }),
        ["tunnels", "add", ..] => Ok(Command::AddTunnel {
            fields: tunnel_fields(&args[2..], true)?,
        }),
        ["tunnels", "edit", value, ..] => Ok(Command::EditTunnel {
            id: id(value)?,
            fields: tunnel_fields(&args[3..], false)?,
        }),
        ["tunnels", "remove", value] => Ok(Command::RemoveTunnel { id: id(value)? }),
        ["tunnels", action @ ("connect" | "disconnect"), value] => {
            let id = if *value == "all" {
                None
            } else {
                Some(id(value)?)
            };
            Ok(if *action == "connect" {
                Command::Connect { id }
            } else {
                Command::Disconnect { id }
            })
        }
        _ => Err("Unknown command or arguments. Run ssh-m8 --help.".into()),
    }
}

fn read_password(input: impl Read) -> Result<Secret, String> {
    let mut bytes = Vec::new();
    if input.take(16_385).read_to_end(&mut bytes).is_err() || bytes.len() > 16_384 {
        control::wipe(&mut bytes);
        return Err("Cannot read password: expected at most 16384 bytes from stdin.".into());
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    match String::from_utf8(bytes) {
        Ok(password) if !password.is_empty() => Ok(Secret(password)),
        Ok(_) => Err("Password is empty; use hosts clear-password to remove it.".into()),
        Err(error) => {
            control::wipe(&mut error.into_bytes());
            Err("Password must be UTF-8.".into())
        }
    }
}

pub fn run() -> i32 {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["agent-help"] {
        print!("{}", include_str!("../docs/agents.md"));
        return 0;
    }
    if args == ["--help"] || args == ["help"] || args.last().is_some_and(|a| a == "--help") {
        print!("{HELP}");
        return 0;
    }
    if args == ["--version"] {
        println!("ssh-m8 {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    let command = parse(&args).and_then(|mut command| {
        if let Command::SetPassword { password, .. } = &mut command {
            if io::stdin().is_terminal() { return Err("Pipe the password to stdin; interactive input is disabled to avoid echoing it.".into()); }
            *password = read_password(io::stdin().lock())?;
        }
        Ok(command)
    });
    let (response, code) = match command {
        Err(error) => (Response::from(Err(error)), 2),
        Ok(command) => {
            match storage::default_directory().and_then(|dir| control::call(&dir, &command)) {
                Ok(response) => {
                    let code = if response.ok { 0 } else { 1 };
                    (response, code)
                }
                Err(error) => (Response::from(Err(error)), 3),
            }
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&response).expect("JSON response")
    );
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command(args: &[&str]) -> Result<Command, String> {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parser_preserves_partial_edits_and_rejects_ambiguous_input() {
        let Command::EditTunnel { id, fields } =
            command(&["tunnels", "edit", "7", "--keep-retrying", "false"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(id, 7);
        assert!(fields.options_only());
        assert!(
            !TunnelFields {
                keep_retrying: Some(false),
                name: Some("New name".into()),
                ..Default::default()
            }
            .options_only()
        );
        let mut tunnel = Tunnel::new(7, 2);
        tunnel.auto_connect = true;
        tunnel.keep_retrying = true;
        fields.apply(&mut tunnel);
        assert!(tunnel.auto_connect);
        assert!(!tunnel.keep_retrying);
        assert_eq!(tunnel.host_id, 2);
        for args in [
            vec!["tunnels", "connect", "0"],
            vec!["tunnels", "connect", "all", "extra"],
            vec!["hosts", "edit", "1", "--port", "0"],
            vec!["hosts", "edit", "1", "--name", "a", "--name", "b"],
            vec!["hosts", "edit", "1", "--password", "secret"],
            vec!["hosts", "edit", "1"],
            vec!["tunnels", "edit", "1", "--auto-connect", "yes"],
            vec!["logs", "--limit", "301"],
        ] {
            assert!(command(&args).is_err());
        }
        assert!(matches!(
            command(&["--json", "tunnels", "connect", "all"]).unwrap(),
            Command::Connect { id: None }
        ));
    }

    #[test]
    fn creation_requires_explicit_routes_and_password_input_is_bounded() {
        assert!(command(&["hosts", "add", "--name", "Example"]).is_err());
        assert!(
            command(&[
                "tunnels", "add", "--name", "Web", "--host", "1", "--kind", "local", "--listen",
                "8080"
            ])
            .is_err()
        );
        assert!(
            command(&[
                "tunnels", "add", "--name", "Proxy", "--host", "1", "--kind", "dynamic",
                "--listen", "1080"
            ])
            .is_ok()
        );
        assert_eq!(read_password(&b"a b\r\n"[..]).unwrap().0, "a b");
        assert!(read_password(&b"\n"[..]).is_err());
        assert!(read_password(&b"\xff"[..]).is_err());
        assert!(read_password(vec![b'x'; 16_385].as_slice()).is_err());
        assert!(
            serde_json::from_str::<Command>(r#"{"command":"status","password":"no"}"#).is_err()
        );
    }
}
