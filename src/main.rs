#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod app;
mod catalog;
mod install;
mod launch;
mod ui;

fn main() -> anyhow::Result<()> {
    if app::run_cli()? {
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("R2MML")
            .with_app_id("io.github.tsifrovik.r2mml")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([940.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "R2MML",
        options,
        Box::new(|cc| Ok(Box::new(app::ManagerApp::new(cc)?))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
