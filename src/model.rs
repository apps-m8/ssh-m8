use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::IpAddr;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Host {
    pub id: u64,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub username: String,
    pub identity: String,
    pub saved_password: bool,
}

impl Host {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            name: String::new(),
            address: String::new(),
            port: 22,
            username: String::new(),
            identity: String::new(),
            saved_password: false,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_name(&self.name)?;
        validate_address(&self.address)?;
        if self.port == 0 {
            return Err("SSH port must be between 1 and 65535.".into());
        }
        if !self
            .username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-@".contains(c))
        {
            return Err("Use a username without spaces or shell characters.".into());
        }
        if self.identity.chars().any(char::is_control) {
            return Err("The key path contains an invalid character.".into());
        }
        Ok(())
    }

    pub fn destination(&self) -> String {
        if self.username.is_empty() {
            self.address.clone()
        } else {
            format!("{}@{}", self.username, self.address)
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub enum TunnelKind {
    Local,
    Remote,
    Dynamic,
}

impl TunnelKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Remote => "Remote",
            Self::Dynamic => "SOCKS",
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Tunnel {
    pub id: u64,
    pub name: String,
    pub host_id: u64,
    pub kind: TunnelKind,
    pub listen_port: u16,
    pub destination: String,
    pub destination_port: u16,
    #[serde(default)]
    pub auto_connect: bool,
    #[serde(default)]
    pub keep_retrying: bool,
}

impl Tunnel {
    pub fn new(id: u64, host_id: u64) -> Self {
        Self {
            id,
            name: String::new(),
            host_id,
            kind: TunnelKind::Local,
            listen_port: 8080,
            destination: "127.0.0.1".into(),
            destination_port: 80,
            auto_connect: false,
            keep_retrying: false,
        }
    }

    pub fn validate(&self, hosts: &[Host]) -> Result<(), String> {
        validate_name(&self.name)?;
        if !hosts.iter().any(|h| h.id == self.host_id) {
            return Err("Choose a saved SSH host first.".into());
        }
        if self.listen_port == 0 {
            return Err("Listening port must be between 1 and 65535.".into());
        }
        if self.kind != TunnelKind::Dynamic {
            validate_address(&self.destination)?;
            if self.destination_port == 0 {
                return Err("Destination port must be between 1 and 65535.".into());
            }
        }
        Ok(())
    }

    pub fn forwarding(&self) -> (String, String) {
        if self.kind == TunnelKind::Dynamic {
            return ("-D".into(), format!("127.0.0.1:{}", self.listen_port));
        }
        let host = if self.destination.contains(':') {
            format!("[{}]", self.destination)
        } else {
            self.destination.clone()
        };
        (
            if self.kind == TunnelKind::Local {
                "-L"
            } else {
                "-R"
            }
            .into(),
            format!(
                "127.0.0.1:{}:{host}:{}",
                self.listen_port, self.destination_port
            ),
        )
    }

    pub fn route(&self) -> String {
        match self.kind {
            TunnelKind::Dynamic => format!("127.0.0.1:{}  ->  SOCKS5 proxy", self.listen_port),
            TunnelKind::Local => format!(
                "This Mac :{}  ->  {}:{}",
                self.listen_port, self.destination, self.destination_port
            ),
            TunnelKind::Remote => format!(
                "Remote :{}  ->  {}:{} on this side",
                self.listen_port, self.destination, self.destination_port
            ),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Library {
    pub version: u32,
    pub next_id: u64,
    pub hosts: Vec<Host>,
    pub tunnels: Vec<Tunnel>,
}

impl Default for Library {
    fn default() -> Self {
        Self {
            version: 1,
            next_id: 1,
            hosts: vec![],
            tunnels: vec![],
        }
    }
}

impl Library {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("This library uses an unsupported version.".into());
        }
        let mut ids = HashSet::new();
        for id in self
            .hosts
            .iter()
            .map(|h| h.id)
            .chain(self.tunnels.iter().map(|t| t.id))
        {
            if id == 0 || id >= self.next_id || !ids.insert(id) {
                return Err("The library has invalid or duplicate IDs.".into());
            }
        }
        for host in &self.hosts {
            host.validate()?;
        }
        for tunnel in &self.tunnels {
            tunnel.validate(&self.hosts)?;
        }
        Ok(())
    }
}

fn validate_name(value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > 120 || value.chars().any(char::is_control) {
        return Err("Enter a name between 1 and 120 characters.".into());
    }
    Ok(())
}

pub fn validate_address(value: &str) -> Result<(), String> {
    if value.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    if value.is_empty()
        || value.len() > 253
        || value.starts_with('-')
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    {
        return Err("Enter a hostname, SSH config alias, or IP address without a port.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn older_tunnels_default_to_manual_and_options_round_trip() {
        let old = r#"{"id":2,"name":"Web","host_id":1,"kind":"Local","listen_port":8080,"destination":"127.0.0.1","destination_port":80}"#;
        let mut tunnel: Tunnel = serde_json::from_str(old).unwrap();
        assert!(!tunnel.auto_connect);
        assert!(!tunnel.keep_retrying);
        tunnel.auto_connect = true;
        tunnel.keep_retrying = true;
        let saved = serde_json::to_vec(&tunnel).unwrap();
        let loaded: Tunnel = serde_json::from_slice(&saved).unwrap();
        assert!(loaded.auto_connect);
        assert!(loaded.keep_retrying);
    }

    #[test]
    fn rejects_option_and_shell_injection() {
        for value in [
            "-oProxyCommand=evil",
            "user@server",
            "$(touch /tmp/x)",
            "a\nb",
            "host:22",
            "",
        ] {
            assert!(validate_address(value).is_err(), "{value}");
        }
        for value in ["server.local", "my-alias", "::1", "203.0.113.2"] {
            assert!(validate_address(value).is_ok(), "{value}");
        }
    }
    #[test]
    fn forwarding_is_loopback_only_and_handles_ipv6() {
        let mut t = Tunnel::new(2, 1);
        t.destination = "::1".into();
        assert_eq!(
            t.forwarding(),
            ("-L".into(), "127.0.0.1:8080:[::1]:80".into())
        );
        t.kind = TunnelKind::Remote;
        assert_eq!(t.forwarding().0, "-R");
        t.kind = TunnelKind::Dynamic;
        assert_eq!(t.forwarding(), ("-D".into(), "127.0.0.1:8080".into()));
    }
    #[test]
    fn library_rejects_dangling_tunnels_and_duplicate_ids() {
        let mut h = Host::new(1);
        h.name = "Test".into();
        h.address = "localhost".into();
        let mut t = Tunnel::new(2, 1);
        t.name = "Web".into();
        let mut lib = Library {
            next_id: 3,
            hosts: vec![h],
            tunnels: vec![t],
            ..Default::default()
        };
        assert!(lib.validate().is_ok());
        lib.tunnels[0].host_id = 99;
        assert!(lib.validate().is_err());
        lib.tunnels[0].host_id = 1;
        lib.tunnels[0].id = 1;
        assert!(lib.validate().is_err());
    }
}
