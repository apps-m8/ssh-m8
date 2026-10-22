use crate::{
    cli::Command,
    control, credentials,
    model::{Host, Library, Tunnel, TunnelKind},
    ssh::{self, Event, Runtime, Status},
    storage::{self, Store},
};
use eframe::egui::{self, Color32, RichText, Vec2};
use std::{
    collections::{HashMap, VecDeque},
    time::SystemTime,
};

const BG: Color32 = Color32::from_rgb(20, 23, 30);
const SIDEBAR: Color32 = Color32::from_rgb(26, 30, 39);
const CARD: Color32 = Color32::from_rgb(30, 35, 45);
const BORDER: Color32 = Color32::from_rgb(47, 54, 67);
const TEXT: Color32 = Color32::from_rgb(230, 234, 242);
const MUTED: Color32 = Color32::from_rgb(140, 153, 172);
const ACCENT: Color32 = Color32::from_rgb(143, 211, 185);
const RED: Color32 = Color32::from_rgb(242, 146, 149);

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Hosts,
    Tunnels,
    Logs,
}
enum Editor {
    Host(Host, String, String, bool),
    Tunnel(Tunnel, String, String),
}
#[derive(Clone, Copy)]
enum Delete {
    Host(u64),
    Tunnel(u64),
}
#[derive(serde::Serialize)]
struct Log {
    timestamp: u64,
    time: String,
    source: String,
    message: String,
    error: bool,
}

pub struct App {
    logo: egui::TextureHandle,
    menubar: crate::menubar::MenuBar,
    control: Option<control::Server>,
    store: Option<Store>,
    library: Library,
    runtime: Option<Runtime>,
    page: Page,
    search: String,
    statuses: HashMap<u64, Status>,
    logs: VecDeque<Log>,
    editor: Option<Editor>,
    editor_error: Option<String>,
    delete: Option<Delete>,
    notice: Option<(String, bool)>,
    startup_error: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme(&cc.egui_ctx);
        let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/troll-logo.png"))
            .expect("Bundled logo must be a valid PNG");
        let mut app = Self {
            menubar: crate::menubar::MenuBar::new(&cc.egui_ctx, &icon),
            logo: cc.egui_ctx.load_texture(
                "Troll logo",
                egui::ColorImage::from_rgba_unmultiplied(
                    [icon.width as usize, icon.height as usize],
                    &icon.rgba,
                ),
                egui::TextureOptions::LINEAR,
            ),
            store: None,
            control: None,
            library: Library::default(),
            runtime: None,
            page: Page::Hosts,
            search: String::new(),
            statuses: HashMap::new(),
            logs: VecDeque::new(),
            editor: None,
            editor_error: None,
            delete: None,
            notice: None,
            startup_error: None,
        };
        match storage::default_directory().and_then(Store::open) {
            Ok((store, library)) => {
                match Runtime::new(cc.egui_ctx.clone(), store.directory.clone()) {
                    Ok(runtime) => {
                        match control::Server::start(&store.directory, cc.egui_ctx.clone()) {
                            Ok(server) => app.control = Some(server),
                            Err(error) => app.notify(format!("CLI unavailable: {error}"), true),
                        }
                        app.store = Some(store);
                        app.library = library;
                        app.runtime = Some(runtime);
                    }
                    Err(error) => app.startup_error = Some(error),
                }
            }
            Err(error) => app.startup_error = Some(error),
        }
        for tunnel in app
            .library
            .tunnels
            .clone()
            .into_iter()
            .filter(|t| t.auto_connect)
        {
            app.connect(tunnel);
        }
        app
    }

    fn status(&self, id: u64) -> Status {
        *self.statuses.get(&id).unwrap_or(&Status::Disconnected)
    }
    fn host_busy(&self, id: u64) -> bool {
        self.library
            .tunnels
            .iter()
            .any(|t| t.host_id == id && self.status(t.id).active())
    }
    fn log(&mut self, source: String, message: String, error: bool) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as libc::time_t;
        let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
        let time = if unsafe { libc::localtime_r(&now, local.as_mut_ptr()) }.is_null() {
            String::new()
        } else {
            let local = unsafe { local.assume_init() };
            format!(
                "{:02}:{:02}:{:02}",
                local.tm_hour, local.tm_min, local.tm_sec
            )
        };
        self.logs.push_back(Log {
            timestamp: now as u64,
            time,
            source,
            message,
            error,
        });
        if self.logs.len() > 300 {
            self.logs.pop_front();
        }
    }
    fn notify(&mut self, message: String, error: bool) {
        self.log("App".into(), message.clone(), error);
        self.notice = Some((message, error));
    }
    fn drain(&mut self) {
        let events = self
            .runtime
            .as_ref()
            .map(|r| r.drain_events())
            .unwrap_or_default();
        for event in events {
            match event {
                Event::Status(id, status) => {
                    self.statuses.insert(id, status);
                }
                Event::Log {
                    source,
                    message,
                    error,
                } => self.log(source, message, error),
            }
        }
    }
    fn connect(&mut self, tunnel: Tunnel) {
        if self.status(tunnel.id).active() {
            return;
        }
        if let Some(host) = self
            .library
            .hosts
            .iter()
            .find(|h| h.id == tunnel.host_id)
            .cloned()
        {
            self.statuses.insert(tunnel.id, Status::Connecting);
            if let Some(runtime) = &self.runtime {
                runtime.connect(host, tunnel);
            }
        }
    }
    fn terminal(&mut self, host: &Host) {
        if let Some(runtime) = &self.runtime {
            match runtime.terminal(host) {
                Ok(()) => self.notify(format!("Opened {} in Terminal.", host.name), false),
                Err(error) => self.notify(error, true),
            }
        }
    }

    fn connect_all(&mut self) {
        for tunnel in self.library.tunnels.clone() {
            self.connect(tunnel);
        }
    }

    fn disconnect_all(&self) {
        if let Some(runtime) = &self.runtime {
            for tunnel in &self.library.tunnels {
                if self.status(tunnel.id).active() {
                    runtime.disconnect(tunnel.id);
                }
            }
        }
    }

    fn set_tunnel_options(
        &mut self,
        id: u64,
        auto_connect: bool,
        keep_retrying: bool,
    ) -> Result<(), String> {
        let mut library = self.library.clone();
        let tunnel = library
            .tunnels
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or("Tunnel not found.")?;
        let retry_changed = tunnel.keep_retrying != keep_retrying;
        tunnel.auto_connect = auto_connect;
        tunnel.keep_retrying = keep_retrying;
        self.save_library(library)?;
        if retry_changed && let Some(runtime) = &self.runtime {
            runtime.set_keep_retrying(id, keep_retrying);
        }
        Ok(())
    }
    fn edit_host(&mut self, host: Host) {
        let port = host.port.to_string();
        self.editor = Some(Editor::Host(host, port, String::new(), false));
        self.editor_error = None;
    }
    fn edit_tunnel(&mut self, tunnel: Tunnel) {
        let listen = tunnel.listen_port.to_string();
        let dest = tunnel.destination_port.to_string();
        self.editor = Some(Editor::Tunnel(tunnel, listen, dest));
        self.editor_error = None;
    }
    fn save_library(&mut self, library: Library) -> Result<(), String> {
        self.store
            .as_ref()
            .ok_or("Library is unavailable")?
            .save(&library)?;
        self.library = library;
        Ok(())
    }

    fn save_with_password(
        &mut self,
        library: Library,
        host_id: u64,
        password: Option<&str>,
    ) -> Result<(), String> {
        let store = self.store.as_ref().ok_or("Library is unavailable")?;
        library.validate()?;
        let account = ssh::account(&store.directory, host_id);
        let mut previous = credentials::get(&account)?;
        match password {
            Some(value) => credentials::set(&account, value)?,
            None => credentials::delete(&account)?,
        };
        let result = store.save(&library);
        let rollback = if result.is_err() {
            match &previous {
                Some(value) => credentials::set_bytes(&account, value),
                None => credentials::delete(&account),
            }
        } else {
            Ok(())
        };
        if let Some(bytes) = &mut previous {
            for byte in bytes {
                unsafe { std::ptr::write_volatile(byte, 0) };
            }
        }
        if let Err(error) = result {
            return Err(match rollback {
                Ok(()) => error,
                Err(rollback) => format!("{error} Password rollback also failed: {rollback}"),
            });
        }
        self.library = library;
        Ok(())
    }

    fn save_editor(&mut self, editor: &mut Editor) -> Result<(), String> {
        let mut library = self.library.clone();
        match editor {
            Editor::Host(host, port, password, remove) => {
                if self.host_busy(host.id) {
                    return Err("Disconnect this host's tunnels before editing it.".into());
                }
                host.name = host.name.trim().into();
                host.address = host.address.trim().into();
                host.username = host.username.trim().into();
                host.identity = host.identity.trim().into();
                host.port = parse_port(port)?;
                host.validate()?;
                if *remove {
                    host.saved_password = false;
                } else if !password.is_empty() {
                    host.saved_password = true;
                }
                if let Some(existing) = library.hosts.iter_mut().find(|h| h.id == host.id) {
                    *existing = host.clone();
                } else {
                    library.hosts.push(host.clone());
                    library.next_id = library.next_id.checked_add(1).ok_or("Too many entries")?;
                }
                if *remove || !password.is_empty() {
                    self.save_with_password(
                        library,
                        host.id,
                        if *remove { None } else { Some(password) },
                    )?;
                } else {
                    self.save_library(library)?;
                }
            }
            Editor::Tunnel(tunnel, listen, destination) => {
                if self.status(tunnel.id).active() {
                    return Err("Disconnect this tunnel before editing it.".into());
                }
                tunnel.name = tunnel.name.trim().into();
                tunnel.destination = tunnel.destination.trim().into();
                tunnel.listen_port = parse_port(listen)?;
                if tunnel.kind != TunnelKind::Dynamic {
                    tunnel.destination_port = parse_port(destination)?;
                }
                tunnel.validate(&library.hosts)?;
                if let Some(existing) = library.tunnels.iter_mut().find(|t| t.id == tunnel.id) {
                    *existing = tunnel.clone();
                } else {
                    library.tunnels.push(tunnel.clone());
                    library.next_id = library.next_id.checked_add(1).ok_or("Too many entries")?;
                }
                self.save_library(library)?;
            }
        }
        self.notify("Saved changes.".into(), false);
        Ok(())
    }

    fn delete_entry(&mut self, delete: Delete) -> Result<(), String> {
        let mut library = self.library.clone();
        match delete {
            Delete::Host(id) => {
                let host = library
                    .hosts
                    .iter()
                    .find(|h| h.id == id)
                    .ok_or("Host not found.")?;
                if library.tunnels.iter().any(|t| t.host_id == id) {
                    return Err("Remove this host's tunnels before deleting it.".into());
                }
                let saved = host.saved_password;
                library.hosts.retain(|h| h.id != id);
                if saved {
                    self.save_with_password(library, id, None)
                } else {
                    self.save_library(library)
                }
            }
            Delete::Tunnel(id) => {
                if !library.tunnels.iter().any(|t| t.id == id) {
                    return Err("Tunnel not found.".into());
                }
                if self.status(id).active() {
                    return Err("Disconnect this tunnel before deleting it.".into());
                }
                library.tunnels.retain(|t| t.id != id);
                self.save_library(library)?;
                self.statuses.remove(&id);
                Ok(())
            }
        }
    }

    fn tunnel_json(&self) -> Vec<serde_json::Value> {
        self.library
            .tunnels
            .iter()
            .map(|t| {
                let mut value = serde_json::to_value(t).expect("Tunnel JSON");
                value["status"] = serde_json::json!(self.status(t.id));
                value
            })
            .collect()
    }

    fn cli_command(&mut self, command: Command) -> Result<serde_json::Value, String> {
        use serde_json::json;
        if command.changes_library() && (self.editor.is_some() || self.delete.is_some()) {
            return Err("Finish or cancel the open editor or delete dialog before changing the library through the CLI.".into());
        }
        match command {
            Command::Status {} => {
                let (connected, disconnected) =
                    tunnel_counts(&self.library.tunnels, &self.statuses);
                Ok(
                    json!({"version": env!("CARGO_PKG_VERSION"), "connected": connected, "disconnected": disconnected,
                    "total": self.library.tunnels.len(), "tunnels": self.tunnel_json()}),
                )
            }
            Command::Hosts {} => Ok(json!(self.library.hosts)),
            Command::Tunnels {} => Ok(json!(self.tunnel_json())),
            Command::Logs { limit } => {
                if !(1..=300).contains(&limit) {
                    return Err("Log limit must be between 1 and 300.".into());
                }
                Ok(json!(
                    self.logs
                        .iter()
                        .skip(self.logs.len().saturating_sub(limit))
                        .collect::<Vec<_>>()
                ))
            }
            Command::AddHost { fields } => {
                let mut host = Host::new(self.library.next_id);
                fields.apply(&mut host);
                let id = host.id;
                let port = host.port.to_string();
                self.save_editor(&mut Editor::Host(host, port, String::new(), false))?;
                Ok(json!(
                    self.library
                        .hosts
                        .iter()
                        .find(|h| h.id == id)
                        .expect("saved host")
                ))
            }
            Command::EditHost { id, fields } => {
                let mut host = self
                    .library
                    .hosts
                    .iter()
                    .find(|h| h.id == id)
                    .cloned()
                    .ok_or("Host not found.")?;
                fields.apply(&mut host);
                let port = host.port.to_string();
                self.save_editor(&mut Editor::Host(host, port, String::new(), false))?;
                Ok(json!(
                    self.library
                        .hosts
                        .iter()
                        .find(|h| h.id == id)
                        .expect("saved host")
                ))
            }
            Command::RemoveHost { id } => {
                self.delete_entry(Delete::Host(id))?;
                self.notify(format!("Deleted host {id} through CLI."), false);
                Ok(json!({"id": id}))
            }
            Command::OpenHost { id } => {
                let host = self
                    .library
                    .hosts
                    .iter()
                    .find(|h| h.id == id)
                    .ok_or("Host not found.")?;
                self.runtime
                    .as_ref()
                    .ok_or("SSH runtime unavailable.")?
                    .terminal(host)?;
                self.notify(format!("Opened host {id} in Terminal through CLI."), false);
                Ok(json!({"id": id, "opened": true}))
            }
            Command::SetPassword { id, password } => {
                if password.0.is_empty() || password.0.len() > 16_384 {
                    return Err("Password must contain 1 to 16384 bytes.".into());
                }
                self.cli_password(id, Some(&password.0))
            }
            Command::ClearPassword { id } => self.cli_password(id, None),
            Command::AddTunnel { fields } => {
                if fields.host.is_none()
                    || fields.kind.is_none()
                    || fields.listen.is_none()
                    || (fields.kind != Some(TunnelKind::Dynamic)
                        && fields.destination_port.is_none())
                {
                    return Err("Supply the host, kind, listening port, and destination port for local/remote tunnels.".into());
                }
                let mut tunnel = Tunnel::new(self.library.next_id, 0);
                fields.apply(&mut tunnel);
                let id = tunnel.id;
                let listen = tunnel.listen_port.to_string();
                let destination = tunnel.destination_port.to_string();
                self.save_editor(&mut Editor::Tunnel(tunnel, listen, destination))?;
                Ok(json!(
                    self.library
                        .tunnels
                        .iter()
                        .find(|t| t.id == id)
                        .expect("saved tunnel")
                ))
            }
            Command::EditTunnel { id, fields } => {
                let options_only = fields.options_only();
                let mut tunnel = self
                    .library
                    .tunnels
                    .iter()
                    .find(|t| t.id == id)
                    .cloned()
                    .ok_or("Tunnel not found.")?;
                fields.apply(&mut tunnel);
                if options_only {
                    self.set_tunnel_options(id, tunnel.auto_connect, tunnel.keep_retrying)?;
                } else {
                    let listen = tunnel.listen_port.to_string();
                    let destination = tunnel.destination_port.to_string();
                    self.save_editor(&mut Editor::Tunnel(tunnel, listen, destination))?;
                }
                Ok(json!(
                    self.library
                        .tunnels
                        .iter()
                        .find(|t| t.id == id)
                        .expect("saved tunnel")
                ))
            }
            Command::RemoveTunnel { id } => {
                self.delete_entry(Delete::Tunnel(id))?;
                self.notify(format!("Deleted tunnel {id} through CLI."), false);
                Ok(json!({"id": id}))
            }
            Command::Connect { id } => {
                let tunnels = self.cli_tunnels(id)?;
                let ids: Vec<_> = tunnels.iter().map(|t| t.id).collect();
                for tunnel in tunnels {
                    self.connect(tunnel);
                }
                Ok(json!({"accepted": true, "tunnel_ids": ids}))
            }
            Command::Disconnect { id } => {
                let tunnels = self.cli_tunnels(id)?;
                let runtime = self.runtime.as_ref().ok_or("SSH runtime unavailable.")?;
                let ids: Vec<_> = tunnels.iter().map(|t| t.id).collect();
                for tunnel in tunnels {
                    runtime.disconnect(tunnel.id);
                }
                Ok(json!({"accepted": true, "tunnel_ids": ids}))
            }
        }
    }

    fn cli_tunnels(&self, id: Option<u64>) -> Result<Vec<Tunnel>, String> {
        if let Some(id) = id {
            Ok(vec![
                self.library
                    .tunnels
                    .iter()
                    .find(|t| t.id == id)
                    .cloned()
                    .ok_or("Tunnel not found.")?,
            ])
        } else {
            Ok(self.library.tunnels.clone())
        }
    }

    fn cli_password(
        &mut self,
        id: u64,
        password: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        if self.host_busy(id) {
            return Err("Disconnect this host's tunnels before changing its password.".into());
        }
        let mut library = self.library.clone();
        let host = library
            .hosts
            .iter_mut()
            .find(|h| h.id == id)
            .ok_or("Host not found.")?;
        host.saved_password = password.is_some();
        self.save_with_password(library, id, password)?;
        self.notify(
            format!("Updated password for host {id} through CLI."),
            false,
        );
        Ok(serde_json::json!({"id": id, "saved_password": password.is_some()}))
    }

    fn drain_cli(&mut self) {
        let pending = self
            .control
            .as_ref()
            .and_then(|server| server.requests.try_recv().ok());
        if let Some(pending) = pending {
            let result = if std::time::Instant::now() > pending.deadline {
                Err("Command expired before it could run. No change was made.".into())
            } else {
                self.cli_command(pending.command)
            };
            let _ = pending.reply.send(result.into());
        }
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        let (connected, disconnected) = tunnel_counts(&self.library.tunnels, &self.statuses);
        egui::SidePanel::left("sidebar")
            .exact_width(210.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(SIDEBAR).inner_margin(20))
            .show(ctx, |ui| {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Image::new(&self.logo)
                            .fit_to_exact_size(Vec2::splat(38.0))
                            .alt_text("ssh-m8 troll logo"),
                    );
                    ui.label(RichText::new("ssh-m8").size(17.0).strong());
                });
                ui.add_space(5.0);
                ui.label(
                    RichText::new("Your connections, close by.")
                        .size(11.5)
                        .color(MUTED),
                );
                ui.add_space(38.0);
                ui.label(RichText::new("WORKSPACE").size(10.0).color(MUTED));
                ui.add_space(10.0);
                for (page, name, count) in [
                    (Page::Hosts, "Hosts", self.library.hosts.len()),
                    (Page::Tunnels, "Tunnels", self.library.tunnels.len()),
                    (Page::Logs, "Logs", self.logs.len()),
                ] {
                    let selected = self.page == page;
                    let text = if page == Page::Tunnels {
                        format!("{name}   {connected}/{count}")
                    } else {
                        format!("{name}   {count}")
                    };
                    let button = egui::Button::new(
                        RichText::new(text)
                            .color(if selected { ACCENT } else { MUTED })
                            .size(14.0),
                    )
                    .fill(if selected {
                        Color32::from_rgb(39, 52, 51)
                    } else {
                        Color32::TRANSPARENT
                    })
                    .stroke(egui::Stroke::NONE)
                    .corner_radius(7);
                    if ui.add_sized([170.0, 40.0], button).clicked() {
                        self.page = page;
                        self.search.clear();
                    }
                    ui.add_space(5.0);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(
                        RichText::new("NATIVE RUST  /  v0.1")
                            .size(10.0)
                            .color(MUTED),
                    );
                    ui.add_space(10.0);
                    ui.label(RichText::new("Saved on this Mac").size(12.0).color(TEXT));
                    ui.label(
                        RichText::new(format!(
                            "{disconnected}/{} disconnected",
                            self.library.tunnels.len()
                        ))
                        .size(12.0)
                        .color(MUTED),
                    )
                    .on_hover_text(
                        "Includes tunnels that are offline, connecting, retrying, or failed.",
                    );
                    ui.horizontal(|ui| {
                        dot(ui, if connected > 0 { ACCENT } else { MUTED });
                        ui.label(
                            RichText::new(format!(
                                "{connected}/{} connected",
                                self.library.tunnels.len()
                            ))
                            .size(12.0)
                            .color(MUTED),
                        );
                    });
                });
            });
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let (title, subtitle) = match self.page {
            Page::Hosts => ("Hosts", "A home for your SSH connections."),
            Page::Tunnels => ("Tunnels", "Forward ports through your saved hosts."),
            Page::Logs => ("Logs", "Connection activity from this session."),
        };
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading(RichText::new(title).size(29.0).strong());
                ui.add_space(5.0);
                ui.label(RichText::new(subtitle).color(MUTED));
            });
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| match self.page {
                    Page::Hosts => {
                        if primary(ui, "+  New host").clicked() {
                            self.edit_host(Host::new(self.library.next_id));
                        }
                    }
                    Page::Tunnels => {
                        let enabled = !self.library.hosts.is_empty();
                        ui.add_enabled_ui(enabled, |ui| {
                            if primary(ui, "+  New tunnel").clicked() {
                                self.edit_tunnel(Tunnel::new(
                                    self.library.next_id,
                                    self.library.hosts[0].id,
                                ));
                            }
                        })
                        .response
                        .on_hover_text(if enabled {
                            "Save a port forward"
                        } else {
                            "Add an SSH host first"
                        });
                    }
                    Page::Logs => {
                        if secondary(ui, "Clear logs").clicked() {
                            self.logs.clear();
                        }
                    }
                },
            );
        });
        ui.add_space(27.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text(match self.page {
                        Page::Hosts => "Search hosts…",
                        Page::Tunnels => "Search tunnels…",
                        Page::Logs => "Search logs…",
                    })
                    .desired_width(255.0)
                    .margin(egui::vec2(12.0, 9.0)),
            );
            if self.page == Page::Tunnels {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let has_disconnected = self
                        .library
                        .tunnels
                        .iter()
                        .any(|t| !self.status(t.id).active());
                    ui.add_enabled_ui(has_disconnected, |ui| {
                        if primary(ui, "Connect all")
                            .on_hover_text(
                                "Connect every saved tunnel, including those hidden by search",
                            )
                            .clicked()
                        {
                            self.connect_all();
                        }
                    });
                    ui.add_enabled_ui(self.statuses.values().any(|s| s.active()), |ui| {
                        if secondary(ui, "Disconnect all").clicked() {
                            self.disconnect_all();
                        }
                    });
                });
            }
        });
        ui.add_space(22.0);
    }

    fn hosts(&mut self, ui: &mut egui::Ui) {
        if self.library.hosts.is_empty() {
            empty(
                ui,
                ">_",
                "Your first connection starts here",
                "Save a host, then open it in Terminal or use it for a tunnel.",
            );
            ui.vertical_centered(|ui| {
                if primary(ui, "+  Add a host").clicked() {
                    self.edit_host(Host::new(self.library.next_id));
                }
            });
            return;
        }
        let search = self.search.to_lowercase();
        let hosts: Vec<_> = self
            .library
            .hosts
            .iter()
            .filter(|h| {
                format!("{} {} {}", h.name, h.address, h.username)
                    .to_lowercase()
                    .contains(&search)
            })
            .cloned()
            .collect();
        if hosts.is_empty() {
            empty(
                ui,
                "?",
                "No matching hosts",
                "Try a different name, address, or username.",
            );
        }
        for host in hosts {
            card().show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    glyph(ui, ">_", ACCENT, 46.0);
                    ui.add_space(7.0);
                    let details_width = (ui.available_width() - 220.0).max(160.0);
                    ui.vertical(|ui| {
                        ui.set_width(details_width);
                        ui.add(
                            egui::Label::new(RichText::new(&host.name).size(16.0).strong())
                                .truncate(),
                        )
                        .on_hover_text(&host.name);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{}  ·  port {}",
                                    host.destination(),
                                    host.port
                                ))
                                .color(MUTED)
                                .size(12.0),
                            )
                            .truncate(),
                        )
                        .on_hover_text(host.destination());
                        ui.add_space(3.0);
                        ui.label(
                            RichText::new(if host.saved_password {
                                "Saved password"
                            } else if !host.identity.is_empty() {
                                "SSH key"
                            } else {
                                "SSH agent / Terminal prompt"
                            })
                            .color(ACCENT)
                            .size(10.0),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("•••", |ui| {
                            let busy = self.host_busy(host.id);
                            if ui
                                .add_enabled(!busy, egui::Button::new("Edit host"))
                                .clicked()
                            {
                                self.edit_host(host.clone());
                                ui.close();
                            }
                            if ui
                                .add_enabled(!busy, egui::Button::new("Delete host"))
                                .clicked()
                            {
                                self.delete = Some(Delete::Host(host.id));
                                ui.close();
                            }
                            if busy {
                                ui.label("Disconnect its tunnels to edit.");
                            }
                        });
                        if secondary(ui, "Open terminal").clicked() {
                            self.terminal(&host);
                        }
                    });
                });
            });
            ui.add_space(10.0);
        }
        ui.add_space(10.0);
        ui.label(
            RichText::new(
                "Passwords are saved in macOS Keychain. Private keys stay in their original files.",
            )
            .size(11.0)
            .color(MUTED),
        );
    }

    fn tunnels(&mut self, ui: &mut egui::Ui) {
        if self.library.tunnels.is_empty() {
            empty(
                ui,
                "<>",
                "Keep your services within reach",
                if self.library.hosts.is_empty() {
                    "Add an SSH host first, then save a local, remote, or SOCKS tunnel."
                } else {
                    "Save a tunnel once. Connect it whenever you need it."
                },
            );
            ui.vertical_centered(|ui| {
                if self.library.hosts.is_empty() {
                    if primary(ui, "+  Add a host").clicked() {
                        self.edit_host(Host::new(self.library.next_id));
                    }
                } else if primary(ui, "+  Add a tunnel").clicked() {
                    self.edit_tunnel(Tunnel::new(self.library.next_id, self.library.hosts[0].id));
                }
            });
            return;
        }
        let search = self.search.to_lowercase();
        let tunnels: Vec<_> = self
            .library
            .tunnels
            .iter()
            .filter(|t| {
                let host = self
                    .library
                    .hosts
                    .iter()
                    .find(|h| h.id == t.host_id)
                    .map(|h| h.name.as_str())
                    .unwrap_or("");
                format!("{} {host} {}", t.name, t.route())
                    .to_lowercase()
                    .contains(&search)
            })
            .cloned()
            .collect();
        if tunnels.is_empty() {
            empty(
                ui,
                "?",
                "No matching tunnels",
                "Try a different name, host, or port.",
            );
        }
        for mut tunnel in tunnels {
            let status = self.status(tunnel.id);
            let host = self
                .library
                .hosts
                .iter()
                .find(|h| h.id == tunnel.host_id)
                .map(|h| h.name.clone())
                .unwrap_or_default();
            card().show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    glyph(
                        ui,
                        match tunnel.kind {
                            TunnelKind::Local => "L",
                            TunnelKind::Remote => "R",
                            TunnelKind::Dynamic => "S",
                        },
                        Color32::from_rgb(168, 181, 237),
                        44.0,
                    );
                    ui.add_space(6.0);
                    let details_width = (ui.available_width() - 255.0).max(160.0);
                    ui.vertical(|ui| {
                        ui.set_width(details_width);
                        ui.add(
                            egui::Label::new(RichText::new(&tunnel.name).size(15.0).strong())
                                .truncate(),
                        )
                        .on_hover_text(&tunnel.name);
                        ui.add(
                            egui::Label::new(RichText::new(tunnel.route()).color(MUTED).size(12.0))
                                .truncate(),
                        )
                        .on_hover_text(tunnel.route());
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{} · via {host}", tunnel.kind.label()))
                                    .color(MUTED)
                                    .size(11.0),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&host);
                        ui.push_id(tunnel.id, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                let retry_changed = ui.checkbox(&mut tunnel.keep_retrying, RichText::new("Reconnect").size(12.0))
                                    .on_hover_text("Retry every 30 seconds after a failed connection or disconnect. Turning this off cancels a pending retry and leaves connected tunnels running.")
                                    .changed();
                                let startup_changed = ui.checkbox(&mut tunnel.auto_connect, RichText::new("Connect on startup").size(12.0))
                                    .on_hover_text("Connect when ssh-m8 starts. This saves immediately and does not change the current connection.")
                                    .changed();
                                if (retry_changed || startup_changed)
                                    && let Err(error) = self.set_tunnel_options(tunnel.id, tunnel.auto_connect, tunnel.keep_retrying)
                                {
                                    self.notify(error, true);
                                }
                            });
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("•••", |ui| {
                            if ui
                                .add_enabled(!status.active(), egui::Button::new("Edit tunnel"))
                                .clicked()
                            {
                                self.edit_tunnel(tunnel.clone());
                                ui.close();
                            }
                            if ui
                                .add_enabled(!status.active(), egui::Button::new("Delete tunnel"))
                                .clicked()
                            {
                                self.delete = Some(Delete::Tunnel(tunnel.id));
                                ui.close();
                            }
                            if ui.button("View logs").clicked() {
                                self.page = Page::Logs;
                                self.search = tunnel.name.clone();
                                ui.close();
                            }
                        });
                        if status.active() {
                            if secondary(
                                ui,
                                if status == Status::Retrying {
                                    "Stop retrying"
                                } else {
                                    "Disconnect"
                                },
                            )
                            .clicked()
                                && let Some(runtime) = &self.runtime
                            {
                                runtime.disconnect(tunnel.id);
                            }
                        } else if secondary(ui, "Connect").clicked() {
                            self.connect(tunnel.clone());
                        }
                        let color = match status {
                            Status::Connected => ACCENT,
                            Status::Failed => RED,
                            _ => MUTED,
                        };
                        ui.label(RichText::new(status.label()).size(11.0).color(color));
                        dot(ui, color);
                    });
                });
            });
            ui.add_space(10.0);
        }
        ui.add_space(10.0);
        ui.label(
            RichText::new("Closing the window keeps tunnels running. Use Quit to stop the app.")
                .size(11.0)
                .color(MUTED),
        );
    }

    fn logs(&mut self, ui: &mut egui::Ui) {
        if self.logs.is_empty() {
            empty(
                ui,
                "≡",
                "Nothing to report yet",
                "Connect a host or tunnel to see activity here.",
            );
            return;
        }
        ui.label(
            RichText::new("LAST 300 EVENTS  ·  THIS SESSION ONLY")
                .size(10.0)
                .color(MUTED),
        );
        ui.add_space(14.0);
        let search = self.search.to_lowercase();
        for log in self.logs.iter().rev().filter(|l| {
            format!("{} {}", l.source, l.message)
                .to_lowercase()
                .contains(&search)
        }) {
            egui::Frame::new()
                .fill(CARD)
                .inner_margin(12)
                .corner_radius(6)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&log.time).monospace().size(11.0).color(MUTED));
                        dot(ui, if log.error { RED } else { ACCENT });
                        ui.label(RichText::new(&log.source).size(12.0).strong());
                    });
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&log.message).size(12.0).color(if log.error {
                                RED
                            } else {
                                TEXT
                            }),
                        )
                        .wrap()
                        .selectable(true),
                    );
                });
            ui.add_space(6.0);
        }
    }

    fn editor(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.editor.take() else {
            return;
        };
        let mut close = false;
        egui::Modal::new(egui::Id::new("editor")).show(ctx, |ui| {
            ui.set_width(440.0);
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - 60.0).max(200.0)).show(ui, |ui| {
            ui.add_space(5.0);
            match &mut editor {
                Editor::Host(host, port, password, remove) => {
                    ui.heading(if self.library.hosts.iter().any(|h| h.id == host.id) { "Edit host" } else { "New host" });
                    ui.add_space(16.0);
                    field(ui, "Name", &mut host.name, "e.g. Home server");
                    field(ui, "Address", &mut host.address, "Hostname, IP address, or SSH config alias");
                    ui.columns(2, |cols| {
                        field(&mut cols[0], "Username", &mut host.username, "Use SSH config");
                        field(&mut cols[1], "SSH port", port, "22");
                    });
                    field(ui, "Private key path (optional)", &mut host.identity, "~/.ssh/id_ed25519");
                    let password_label = ui.label(RichText::new("Password").size(12.0).color(MUTED));
                    ui.add(egui::TextEdit::singleline(password).password(true).desired_width(f32::INFINITY)
                        .hint_text(if host.saved_password { "Leave blank to keep saved password" } else { "Optional; saved in macOS Keychain" }).margin(egui::vec2(10.0, 8.0))).labelled_by(password_label.id);
                    if host.saved_password { ui.checkbox(remove, "Remove saved password"); }
                    ui.add_space(8.0);
                    ui.label(RichText::new("SSH config and your agent are available too. Verify a new host's fingerprint in Terminal before connecting tunnels.").size(11.0).color(MUTED));
                }
                Editor::Tunnel(tunnel, listen, destination) => {
                    ui.heading(if self.library.tunnels.iter().any(|t| t.id == tunnel.id) { "Edit tunnel" } else { "New tunnel" });
                    ui.add_space(16.0);
                    field(ui, "Name", &mut tunnel.name, "e.g. Database");
                    let host_label = ui.label(RichText::new("SSH host").size(12.0).color(MUTED));
                    egui::ComboBox::from_id_salt("host-choice").width(420.0)
                        .selected_text(self.library.hosts.iter().find(|h| h.id == tunnel.host_id).map(|h| h.name.as_str()).unwrap_or("Select host"))
                        .show_ui(ui, |ui| { for host in &self.library.hosts { ui.selectable_value(&mut tunnel.host_id, host.id, &host.name); } }).response.labelled_by(host_label.id);
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        for kind in [TunnelKind::Local, TunnelKind::Remote, TunnelKind::Dynamic] {
                            ui.selectable_value(&mut tunnel.kind, kind, kind.label());
                        }
                    });
                    ui.add_space(12.0);
                    field(ui, if tunnel.kind == TunnelKind::Remote { "Listening port on remote host" } else { "Listening port on this Mac" }, listen, "8080");
                    if tunnel.kind != TunnelKind::Dynamic {
                        field(ui, if tunnel.kind == TunnelKind::Local { "Destination reachable from SSH host" } else { "Destination reachable from this Mac" }, &mut tunnel.destination, "127.0.0.1");
                        field(ui, "Destination port", destination, "80");
                    }
                    ui.add_space(4.0);
                    ui.label(RichText::new(match tunnel.kind {
                        TunnelKind::Local => "Listens on 127.0.0.1 on this Mac.",
                        TunnelKind::Remote => "Requests 127.0.0.1 on the remote host. Its SSH server's GatewayPorts setting can override this.",
                        TunnelKind::Dynamic => "Creates a SOCKS5 proxy on 127.0.0.1 on this Mac.",
                    }).size(11.0).color(MUTED));
                    ui.add_space(12.0);
                    ui.checkbox(&mut tunnel.auto_connect, "Connect on startup")
                        .on_hover_text("Connect this tunnel when ssh-m8 opens.");
                    ui.checkbox(&mut tunnel.keep_retrying, "Keep retrying")
                        .on_hover_text("Retry 30 seconds after a failed connection or a dropped tunnel.");
                    ui.label(RichText::new("Retries continue while the window is closed. Disconnect cancels them.").size(11.0).color(MUTED));
                }
            }
            if let Some(error) = &self.editor_error { ui.add_space(10.0); ui.colored_label(RED, error); }
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                if secondary(ui, "Cancel").clicked() { close = true; }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if primary(ui, "Save changes").clicked() {
                        match self.save_editor(&mut editor) {
                            Ok(()) => close = true,
                            Err(error) => self.editor_error = Some(error),
                        }
                    }
                });
            });
            });
        });
        if !close {
            self.editor = Some(editor);
        }
    }

    fn confirm_delete(&mut self, ctx: &egui::Context) {
        let Some(delete) = self.delete.take() else {
            return;
        };
        let mut keep = true;
        egui::Modal::new(egui::Id::new("delete")).show(ctx, |ui| {
            ui.set_width(380.0);
            let (name, blocked) = match delete {
                Delete::Host(id) => (
                    self.library
                        .hosts
                        .iter()
                        .find(|h| h.id == id)
                        .map(|h| h.name.clone())
                        .unwrap_or_default(),
                    self.library.tunnels.iter().any(|t| t.host_id == id),
                ),
                Delete::Tunnel(id) => (
                    self.library
                        .tunnels
                        .iter()
                        .find(|t| t.id == id)
                        .map(|t| t.name.clone())
                        .unwrap_or_default(),
                    self.status(id).active(),
                ),
            };
            ui.heading(format!("Delete {name}?"));
            ui.add_space(12.0);
            ui.label(if blocked {
                "Remove this host's tunnels, or disconnect the selected tunnel, before deleting."
            } else {
                "This removes the saved entry and its saved password, if any."
            });
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                if secondary(ui, "Cancel").clicked() {
                    keep = false;
                }
                if ui
                    .add_enabled(
                        !blocked,
                        egui::Button::new(RichText::new("Delete").color(RED)),
                    )
                    .clicked()
                {
                    let result = self.delete_entry(delete);
                    match result {
                        Ok(()) => self.notify(format!("Deleted {name}."), false),
                        Err(error) => self.notify(error, true),
                    }
                    keep = false;
                }
            });
        });
        if keep {
            self.delete = Some(delete);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        keep_running_when_closed(ctx);
        self.drain();
        self.drain_cli();
        match self.menubar.take_action() {
            Some(crate::menubar::Action::ConnectAll) => self.connect_all(),
            Some(crate::menubar::Action::DisconnectAll) => self.disconnect_all(),
            None => {}
        }
        self.sidebar(ctx);
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(30)).show(ctx, |ui| {
            if let Some(error) = &self.startup_error {
                ui.heading("Could not open ssh-m8");
                ui.add_space(15.0); ui.colored_label(RED, error);
                ui.label("Resolve the issue and reopen the app. Existing files have been left untouched.");
                return;
            }
            self.header(ui);
            if let Some((message, error)) = self.notice.clone() {
                egui::Frame::new().fill(CARD).inner_margin(10).corner_radius(6).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(if error { RED } else { ACCENT }, &message);
                        if ui.small_button("Dismiss").clicked() { self.notice = None; }
                    });
                });
                ui.add_space(14.0);
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                match self.page { Page::Hosts => self.hosts(ui), Page::Tunnels => self.tunnels(ui), Page::Logs => self.logs(ui) }
            });
        });
        self.editor(ctx);
        self.confirm_delete(ctx);
        let (connected, _) = tunnel_counts(&self.library.tunnels, &self.statuses);
        let active = self
            .library
            .tunnels
            .iter()
            .filter(|t| self.status(t.id).active())
            .count();
        self.menubar
            .update(connected, self.library.tunnels.len(), active);
    }
}

fn tunnel_counts(tunnels: &[Tunnel], statuses: &HashMap<u64, Status>) -> (usize, usize) {
    let connected = tunnels
        .iter()
        .filter(|t| statuses.get(&t.id) == Some(&Status::Connected))
        .count();
    (connected, tunnels.len() - connected)
}

fn keep_running_when_closed(ctx: &egui::Context) {
    if ctx.input(|input| input.viewport().close_requested()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }
}

fn parse_port(value: &str) -> Result<u16, String> {
    value
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|p| *p > 0)
        .ok_or("Enter a port between 1 and 65535.".into())
}
fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    let label = ui.label(RichText::new(label).size(12.0).color(MUTED));
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .hint_text(hint)
            .margin(egui::vec2(10.0, 8.0)),
    )
    .labelled_by(label.id);
    ui.add_space(10.0);
}
fn primary(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).color(BG).size(12.5).strong())
            .fill(ACCENT)
            .min_size(Vec2::new(104.0, 34.0))
            .corner_radius(6)
            .stroke(egui::Stroke::NONE),
    )
}
fn secondary(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).color(TEXT).size(12.0))
            .min_size(Vec2::new(90.0, 32.0))
            .corner_radius(6)
            .fill(Color32::from_rgb(39, 46, 58))
            .stroke(egui::Stroke::new(1.0_f32, BORDER)),
    )
}
fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .corner_radius(9)
        .inner_margin(16)
}
fn glyph(ui: &mut egui::Ui, text: &str, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 9, color.gamma_multiply(0.12));
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::monospace(size * 0.42),
        color,
    );
}
fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.0, color);
}
fn empty(ui: &mut egui::Ui, symbol: &str, title: &str, description: &str) {
    ui.add_space(80.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(symbol).monospace().size(40.0).color(ACCENT));
        ui.add_space(20.0);
        ui.label(RichText::new(title).size(20.0).strong());
        ui.add_space(8.0);
        ui.label(RichText::new(description).size(12.0).color(MUTED));
        ui.add_space(22.0);
    });
}
fn theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = SIDEBAR;
    style.visuals.extreme_bg_color = BG;
    style.visuals.faint_bg_color = CARD;
    style.visuals.selection.bg_fill = Color32::from_rgb(49, 89, 78);
    style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, ACCENT);
    style.visuals.widgets.inactive.bg_fill = CARD;
    style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(49, 60, 70);
    style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, MUTED);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(48, 78, 69);
    style.spacing.item_spacing = Vec2::new(9.0, 7.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    ctx.set_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sidebar_counts_only_established_saved_tunnels() {
        let tunnels: Vec<_> = (1..=5).map(|id| Tunnel::new(id, 9)).collect();
        let statuses = HashMap::from([
            (1, Status::Connected),
            (2, Status::Connecting),
            (3, Status::Retrying),
            (4, Status::Failed),
            (99, Status::Connected),
        ]);
        assert_eq!(tunnel_counts(&tunnels, &statuses), (1, 4));
        assert_eq!(tunnel_counts(&[], &statuses), (0, 0));
    }

    #[test]
    fn closing_the_window_cancels_exit_and_minimizes() {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let output = ctx.run(input, keep_running_when_closed);
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(commands.contains(&egui::ViewportCommand::CancelClose));
        assert!(commands.contains(&egui::ViewportCommand::Minimized(true)));
        let output = ctx.run(egui::RawInput::default(), keep_running_when_closed);
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .is_empty()
        );
    }

    #[test]
    fn ports_must_be_real_tcp_ports() {
        for value in ["0", "-1", "65536", "22x", ""] {
            assert!(parse_port(value).is_err());
        }
        assert_eq!(parse_port(" 5432 ").unwrap(), 5432);
    }
}
