#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod app;
mod catalog;
mod icon_cache;
mod install;
mod launch;
mod markdown;
mod ui;

fn main() -> anyhow::Result<()> {
    if app::run_cli()? {
        return Ok(());
    }
    let (initial_size, minimum_size) = app::initial_window_dimensions()?;
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/r2mml-icon.png"))?;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("R2MML")
            .with_icon(icon)
            .with_app_id("io.github.tsifrovik.r2mml")
            .with_inner_size(initial_size)
            .with_min_inner_size(minimum_size),
        ..Default::default()
    };
    eframe::run_native(
        "R2MML",
        options,
        Box::new(|cc| Ok(Box::new(app::ManagerApp::new(cc)?))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
