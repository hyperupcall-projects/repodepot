mod app;
mod backend;
mod check;
mod config;
mod db;
mod git;
mod github;
mod jobs;
mod layout;
mod token;
mod ui;

use anyhow::anyhow;
use eframe::egui;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("depository=info")),
        )
        .init();

    let paths = config::Paths::from_system()?;
    if std::env::args().skip(1).any(|arg| arg == "--check") {
        return check::run(&paths);
    }

    let (config, config_error) = config::load(&paths.config_file);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("depository")
            .with_app_id("depository")
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "depository",
        options,
        Box::new(move |cc| {
            let backend =
                backend::Backend::start(cc.egui_ctx.clone(), paths.clone(), config.clone());
            Ok(Box::new(app::App::new(paths, config, config_error, backend)))
        }),
    )
    .map_err(|e| anyhow!("couldn't open the window: {e}"))
}
