mod cli;
mod control;
mod credentials;
mod menubar;
mod model;
mod ssh;
mod storage;
mod ui;

fn main() -> eframe::Result {
    if std::env::args().nth(1).as_deref() == Some("--askpass") {
        match credentials::askpass() {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    if std::env::args_os().len() > 1 {
        std::process::exit(cli::run());
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([860.0, 600.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/troll-icon.png"))
                    .expect("Bundled app icon must be a valid PNG"),
            )
            .with_app_id("local.ssh-m8"),
        renderer: eframe::Renderer::Glow,
        multisampling: 0,
        vsync: true,
        ..Default::default()
    };
    eframe::run_native(
        "ssh-m8",
        options,
        Box::new(|cc| Ok(Box::new(ui::App::new(cc)))),
    )
}
