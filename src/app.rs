use crate::{
    catalog::{self, Package},
    install::{self, ProfileState},
    launch,
    ui::{Language, UiAction, UiData, UiState},
};
use anyhow::{Context, Result, bail};
use eframe::egui;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, mpsc},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
struct Paths {
    root: PathBuf,
    cache: PathBuf,
    profile: PathBuf,
    logs: PathBuf,
}
impl Paths {
    fn new() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .context("Home directory is unavailable")?;
        let root = std::env::var_os("R2MML_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(home).join("Library/Application Support/R2MML"));
        if root.components().any(|part| {
            Path::new(part.as_os_str())
                .extension()
                .is_some_and(|e| e == "app")
        }) {
            bail!("R2MML data must be stored outside application bundles");
        }
        let paths = Self {
            cache: root.join("Cache"),
            profile: root.join("Profiles/Default"),
            logs: root.join("Logs"),
            root,
        };
        for path in [&paths.root, &paths.cache, &paths.profile, &paths.logs] {
            fs::create_dir_all(path)?;
        }
        Ok(paths)
    }
    fn lock(&self) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("manager.lock"))?;
        file.try_lock_exclusive()
            .context("R2MML is already running for this profile")?;
        Ok(file)
    }
    fn log(&self, message: &str) {
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.logs.join("R2MML.log"))
        {
            let seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let _ = writeln!(file, "[{seconds}] {message}");
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Settings {
    #[serde(default)]
    game_path: String,
}

const DEFAULT_GAME: &str = "/Applications/Risk of Rain 2.app";
const MAIN_SIZE: [f32; 2] = [1180.0, 760.0];
const MAIN_MIN_SIZE: [f32; 2] = [940.0, 620.0];

fn resolve_game_path(saved: &str, default: &Path) -> Option<PathBuf> {
    let saved = saved.trim();
    if !saved.is_empty() {
        if let Ok(game) = launch::validate_game(Path::new(saved)) {
            return Some(game.app_path);
        }
    }
    launch::validate_game(default)
        .ok()
        .map(|game| game.app_path)
}

fn startup_settings(paths: &Paths) -> (Settings, bool) {
    let mut settings: Settings = fs::read(paths.root.join("settings.json"))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default();
    let game_path = resolve_game_path(&settings.game_path, Path::new(DEFAULT_GAME));
    let setup_required = game_path.is_none();
    settings.game_path = game_path
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    #[cfg(debug_assertions)]
    let setup_required =
        setup_required || std::env::var("R2MML_PREVIEW_SETUP").is_ok_and(|value| value == "1");
    (settings, setup_required)
}

pub fn initial_window_dimensions() -> Result<([f32; 2], [f32; 2])> {
    let (_, setup_required) = startup_settings(&Paths::new()?);
    Ok(if setup_required {
        ([540.0, 390.0], [480.0, 350.0])
    } else {
        (MAIN_SIZE, MAIN_MIN_SIZE)
    })
}

fn primary_language() -> Language {
    if let Ok(override_lang) = std::env::var("R2MML_LANGUAGE") {
        return language_code(&override_lang);
    }
    #[cfg(target_os = "macos")]
    if let Ok(output) = Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
    {
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout);
            if let Some(first) = value
                .lines()
                .map(|s| s.trim().trim_matches(|c| c == '"' || c == ','))
                .find(|s| !s.is_empty() && *s != "(" && *s != ")")
            {
                return language_code(first);
            }
        }
    }
    language_code(&std::env::var("LANG").unwrap_or_else(|_| "en".into()))
}
fn language_code(value: &str) -> Language {
    if value
        .split(['-', '_', '.'])
        .next()
        .unwrap_or("")
        .eq_ignore_ascii_case("ru")
    {
        Language::Ru
    } else {
        Language::En
    }
}
fn text(lang: Language, ru: &str, en: &str) -> String {
    if matches!(lang, Language::Ru) {
        ru.into()
    } else {
        en.into()
    }
}

enum Event {
    Catalog(Result<Vec<Package>, String>),
    Profile(Result<ProfileState, String>),
    Launch {
        modded: bool,
        result: Result<u32, String>,
    },
    Readme(String, Result<String, String>),
    Changelog(String, Result<String, String>),
    Progress(String),
}

pub struct ManagerApp {
    paths: Paths,
    _lock: File,
    settings: Settings,
    language: Language,
    setup_required: bool,
    ui: UiState,
    catalog: Arc<Vec<Package>>,
    profile: ProfileState,
    busy: bool,
    status: String,
    error: Option<String>,
    readme: Option<String>,
    changelog: Option<String>,
    selected_readme: String,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
    #[cfg(debug_assertions)]
    screenshot: Option<PathBuf>,
    #[cfg(debug_assertions)]
    screenshot_requested: bool,
    #[cfg(debug_assertions)]
    created: std::time::Instant,
}
impl ManagerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self> {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let paths = Paths::new()?;
        let lock = paths.lock()?;
        crate::icon_cache::install(&cc.egui_ctx, &paths.cache)?;
        let (settings, setup_required) = startup_settings(&paths);
        let profile = ProfileState::load(&paths.profile)?;
        let (sender, receiver) = mpsc::channel();
        let language = primary_language();
        let mut app = Self {
            paths,
            _lock: lock,
            settings,
            language,
            setup_required,
            ui: UiState::default(),
            catalog: Arc::new(Vec::new()),
            profile,
            busy: false,
            status: String::new(),
            error: None,
            readme: None,
            changelog: None,
            selected_readme: String::new(),
            sender,
            receiver,
            #[cfg(debug_assertions)]
            screenshot: std::env::var_os("R2MML_SCREENSHOT_PATH").map(PathBuf::from),
            #[cfg(debug_assertions)]
            screenshot_requested: false,
            #[cfg(debug_assertions)]
            created: std::time::Instant::now(),
        };
        #[cfg(debug_assertions)]
        if let Ok(page) = std::env::var("R2MML_PREVIEW_PAGE") {
            app.ui.set_preview_page(&page);
        }
        if !app.setup_required {
            app.refresh(&cc.egui_ctx, false);
        }
        Ok(app)
    }
    fn show_setup(&mut self, ctx: &egui::Context) {
        let mut choose_game = false;
        let lang = self.language;
        let background = if ctx.style().visuals.dark_mode {
            egui::Color32::from_rgb(27, 29, 34)
        } else {
            egui::Color32::from_rgb(247, 248, 251)
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(background).inner_margin(28))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(4.0);
                        ui.add(
                            egui::Image::new(egui::include_image!("../assets/r2mml-icon.png"))
                                .fit_to_exact_size(egui::Vec2::splat(68.0)),
                        );
                        ui.add_space(12.0);
                        ui.label(egui::RichText::new("R2MML").size(26.0).strong());
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new(text(
                                lang,
                                "Выберите игру",
                                "Choose your game",
                            ))
                            .size(20.0)
                            .strong(),
                        );
                        ui.add_space(8.0);
                        ui.label(text(
                            lang,
                            "Укажите приложение Risk of Rain 2.app, собранное с помощью Risk2Mac.",
                            "Select Risk of Rain 2.app built with Risk2Mac.",
                        ));
                        ui.add_space(20.0);
                        choose_game = ui
                            .add_sized(
                                [280.0, 42.0],
                                egui::Button::new(
                                    egui::RichText::new(text(
                                        lang,
                                        "Выбрать приложение…",
                                        "Choose application…",
                                    ))
                                    .size(14.0)
                                    .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(0, 122, 255))
                                .corner_radius(10),
                            )
                            .clicked();
                        if let Some(error) = &self.error {
                            ui.add_space(14.0);
                            ui.colored_label(
                                egui::Color32::from_rgb(230, 86, 70),
                                text(
                                    lang,
                                    "Не удалось выбрать игру. Проверьте приложение и попробуйте снова.",
                                    "Couldn't select this game. Check the application and try again.",
                                ),
                            );
                            ui.label(egui::RichText::new(error).small().weak());
                        }
                    });
                });
            });
        if choose_game {
            if let Err(error) = self.action(UiAction::ChooseGame, ctx) {
                self.fail(format!("{error:#}"));
            }
        }
    }
    fn refresh(&mut self, ctx: &egui::Context, force: bool) {
        if self.busy {
            return;
        }
        if force {
            // Retry failed image requests after connectivity returns. egui
            // clears bytes/images/textures; our durable icon files remain.
            ctx.forget_all_images();
        }
        self.busy = true;
        self.error = None;
        self.status = text(
            self.language,
            "Загрузка каталога Thunderstore…",
            "Loading the Thunderstore catalog…",
        );
        let cache = self.paths.cache.clone();
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = if force {
                catalog::fetch_catalog_force(&cache)
            } else {
                catalog::fetch_catalog(&cache)
            }
            .map_err(|e| format!("{e:#}"));
            let _ = sender.send(Event::Catalog(result));
            ctx.request_repaint();
        });
    }
    fn action(&mut self, action: UiAction, ctx: &egui::Context) -> Result<()> {
        match action {
            UiAction::Refresh => self.refresh(ctx, true),
            UiAction::ChooseGame => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title(&text(
                        self.language,
                        "Выберите Risk of Rain 2.app",
                        "Choose Risk of Rain 2.app",
                    ))
                    .add_filter("macOS application", &["app"])
                    .pick_file()
                {
                    self.save_game(path.to_string_lossy().into_owned(), ctx)?;
                }
            }
            UiAction::SaveGame(path) => self.save_game(path, ctx)?,
            UiAction::OpenLogs => open_path(&self.paths.root)?,
            UiAction::OpenUrl(url) => {
                let parsed = reqwest::Url::parse(&url)?;
                if !matches!(parsed.scheme(), "https" | "http") {
                    bail!("Only web links can be opened");
                }
                open_path(Path::new(&url))?;
            }
            UiAction::SelectPackage { package, version } => {
                let Some(pkg) = self
                    .catalog
                    .iter()
                    .find(|p| p.full_name == package)
                    .cloned()
                else {
                    return Ok(());
                };
                let key = format!("{package}-{version}");
                if key == self.selected_readme {
                    return Ok(());
                }
                self.selected_readme = key.clone();
                self.readme = None;
                self.changelog = None;
                let cache = self.paths.cache.clone();
                let sender = self.sender.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let result =
                        catalog::fetch_readme(&pkg, &version, &cache).map_err(|e| format!("{e:#}"));
                    let _ = sender.send(Event::Readme(key.clone(), result));
                    ctx.request_repaint();
                    let result = catalog::fetch_changelog(&pkg, &version, &cache)
                        .map_err(|e| format!("{e:#}"));
                    let _ = sender.send(Event::Changelog(key, result));
                    ctx.request_repaint();
                });
            }
            UiAction::Install { package, version } => {
                if self.busy {
                    return Ok(());
                }
                let pkg = self
                    .catalog
                    .iter()
                    .find(|p| p.full_name == package)
                    .context("Package not found")?
                    .clone();
                self.busy = true;
                self.error = None;
                self.status = text(
                    self.language,
                    "Установка с зависимостями…",
                    "Installing with dependencies…",
                );
                let paths = self.paths.clone();
                let catalog = self.catalog.clone();
                let sender = self.sender.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let progress = |message: &str| {
                        let _ = sender.send(Event::Progress(message.into()));
                        ctx.request_repaint();
                    };
                    let result = install::install(
                        &pkg,
                        &version,
                        &catalog,
                        &paths.profile,
                        &paths.cache,
                        progress,
                    )
                    .map_err(|e| format!("{e:#}"));
                    let _ = sender.send(Event::Profile(result));
                    ctx.request_repaint();
                });
            }
            UiAction::Toggle { package, enabled } => {
                if self.busy {
                    return Ok(());
                }
                if !enabled {
                    let affected = install::disable_plan(&self.profile, &package)?;
                    if affected.len() > 1 {
                        self.error = None;
                        self.ui.confirm_disable(package, affected);
                        return Ok(());
                    }
                }
                self.profile = install::toggle(&self.paths.profile, &package, enabled)?;
                self.error = None;
                self.status = text(
                    self.language,
                    "Изменение применится при следующем запуске",
                    "Change applies at the next launch",
                );
            }
            UiAction::Disable { package, mode } => {
                if self.busy {
                    return Ok(());
                }
                self.profile = install::disable(&self.paths.profile, &package, mode)?;
                self.error = None;
                self.status = text(
                    self.language,
                    "Изменение применится при следующем запуске",
                    "Change applies at the next launch",
                );
            }
            UiAction::Remove { package } => {
                if self.busy {
                    return Ok(());
                }
                self.profile = install::remove(&self.paths.profile, &package)?;
                self.status = text(
                    self.language,
                    "Мод удалён из профиля",
                    "Mod removed from the profile",
                );
            }
            UiAction::Launch { modded } => {
                if self.busy {
                    return Ok(());
                }
                let game = PathBuf::from(&self.settings.game_path);
                self.busy = true;
                self.error = None;
                self.status = if modded {
                    text(
                        self.language,
                        "Подготовка запуска с модами…",
                        "Preparing modded launch…",
                    )
                } else {
                    text(self.language, "Запуск обычной игры…", "Starting vanilla…")
                };
                let paths = self.paths.clone();
                let sender = self.sender.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let result = (|| -> Result<u32> {
                        launch::validate_game(&game)?;
                        let runtime = if modded {
                            launch::ensure_runtime(&paths.cache, &|message| {
                                let _ = sender.send(Event::Progress(message.into()));
                                ctx.request_repaint();
                            })?
                        } else {
                            paths.cache.clone()
                        };
                        launch::launch_game(&game, &paths.profile, &runtime, modded)
                    })()
                    .map_err(|e| format!("{e:#}"));
                    let _ = sender.send(Event::Launch { modded, result });
                    ctx.request_repaint();
                });
            }
        }
        Ok(())
    }
    fn save_game(&mut self, path: String, ctx: &egui::Context) -> Result<()> {
        let info = launch::validate_game(Path::new(path.trim()))?;
        let settings = Settings {
            game_path: info.app_path.to_string_lossy().into_owned(),
        };
        catalog::atomic_write(
            &self.paths.root.join("settings.json"),
            &serde_json::to_vec_pretty(&settings)?,
        )?;
        self.settings = settings;
        self.error = None;
        self.status = text(self.language, "Путь к игре сохранён", "Game location saved");
        if self.setup_required {
            self.setup_required = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(MAIN_MIN_SIZE.into()));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(MAIN_SIZE.into()));
            self.refresh(ctx, false);
        }
        Ok(())
    }
    fn fail(&mut self, message: String) {
        self.paths.log(&message);
        self.error = Some(message);
        self.status = text(
            self.language,
            "Не удалось завершить действие",
            "The action could not be completed",
        );
    }
}
impl eframe::App for ManagerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::Catalog(result) => {
                    self.busy = false;
                    match result {
                        Ok(items) => {
                            self.status = text(
                                self.language,
                                &format!("{} модов в каталоге", items.len()),
                                &format!("{} mods in the catalog", items.len()),
                            );
                            self.catalog = Arc::new(items)
                        }
                        Err(e) => self.fail(e),
                    }
                }
                Event::Profile(result) => {
                    self.busy = false;
                    match result {
                        Ok(profile) => {
                            self.profile = profile;
                            self.status = text(
                                self.language,
                                "Установлено. Можно запускать с модами.",
                                "Installed. Ready to play modded.",
                            );
                        }
                        Err(e) => self.fail(e),
                    }
                }
                Event::Launch { modded, result } => {
                    self.busy = false;
                    match result {
                        Ok(pid) => {
                            self.paths.log(&format!("Game launch process {pid}"));
                            self.status = text(self.language, "Игра запущена", "Game launched");
                        }
                        Err(e) if modded => {
                            self.paths.log(&e);
                            self.status = text(
                                self.language,
                                "Не удалось запустить игру с модами",
                                "Modded launch failed",
                            );
                            self.ui.show_modded_launch_error(e);
                            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        }
                        Err(e) => self.fail(e),
                    }
                }
                Event::Readme(key, result) => {
                    if key == self.selected_readme {
                        self.readme = Some(result.unwrap_or_else(|e| {
                            format!(
                                "{}\n\n{e}",
                                text(
                                    self.language,
                                    "Описание недоступно. Используйте ссылку на Thunderstore.",
                                    "README unavailable. Use the Thunderstore link."
                                )
                            )
                        }));
                    }
                }
                Event::Changelog(key, result) => {
                    if key == self.selected_readme {
                        self.changelog = Some(result.unwrap_or_else(|_| {
                            text(
                                self.language,
                                "История изменений недоступна для этой версии.",
                                "No changelog is available for this version.",
                            )
                        }));
                    }
                }
                Event::Progress(message) => {
                    self.status = match message.as_str() {
                        "Downloading BepInEx for macOS…" => {
                            text(self.language, "Загрузка BepInEx для macOS…", &message)
                        }
                        "Preparing the macOS mod runtime…" => {
                            text(self.language, "Подготовка среды модов для macOS…", &message)
                        }
                        _ => message,
                    };
                }
            }
        }
        if self.setup_required {
            self.show_setup(ctx);
        } else {
            let actions = self.ui.show(
                ctx,
                UiData {
                    language: self.language,
                    catalog: &self.catalog,
                    installed: &self.profile.installed,
                    game_path: &self.settings.game_path,
                    status: &self.status,
                    busy: self.busy,
                    progress: None,
                    error: self.error.as_deref(),
                    readme: self.readme.as_deref(),
                    changelog: self.changelog.as_deref(),
                },
            );
            for action in actions {
                if let Err(error) = self.action(action, ctx) {
                    self.fail(format!("{error:#}"));
                }
            }
        }
        #[cfg(debug_assertions)]
        if let Some(path) = self.screenshot.clone() {
            if !self.screenshot_requested
                && (self.setup_required || !self.catalog.is_empty())
                && self.created.elapsed().as_secs() > 12
            {
                self.screenshot_requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            for event in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                    if let Err(e) = image::save_buffer(
                        &path,
                        &bytes,
                        image.width() as u32,
                        image.height() as u32,
                        image::ColorType::Rgba8,
                    ) {
                        self.paths.log(&format!("Screenshot: {e}"));
                    }
                    self.screenshot = None;
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }
}
fn open_path(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    let status = Command::new("/usr/bin/open").arg(path).status()?;
    #[cfg(target_os = "windows")]
    let status = Command::new("explorer.exe").arg(path).status()?;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let status = Command::new("xdg-open").arg(path).status()?;
    if !status.success() {
        bail!("Could not open the requested location")
    }
    Ok(())
}

pub fn run_cli() -> Result<bool> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return Ok(false);
    }
    if args[0] == "--version" {
        println!("R2MML {}", env!("CARGO_PKG_VERSION"));
        return Ok(true);
    }
    let paths = Paths::new()?;
    let _lock = paths.lock()?;
    match args[0].as_str() {
        "--check" => {
            let game = args.get(1).context("--check requires the game .app path")?;
            let info = launch::validate_game(Path::new(game))?;
            println!("{}", serde_json::to_string_pretty(&info)?);
        }
        "--catalog" => {
            let items = catalog::fetch_catalog(&paths.cache)?;
            println!("{}", items.len());
        }
        "--install" => {
            let name = args.get(1).context("--install requires a package name")?;
            let items = catalog::fetch_catalog(&paths.cache)?;
            let pkg = items
                .iter()
                .find(|p| &p.full_name == name)
                .context("Package not found")?;
            let version = args
                .get(2)
                .map(String::as_str)
                .or_else(|| pkg.latest().map(|v| v.version_number.as_str()))
                .context("No active version")?;
            let profile =
                install::install(pkg, version, &items, &paths.profile, &paths.cache, |s| {
                    eprintln!("{s}")
                })?;
            println!("{}", serde_json::to_string_pretty(&profile)?);
        }
        "--launch-modded" | "--launch-vanilla" => {
            let game = args.get(1).context("Launch requires the game .app path")?;
            let modded = args[0] == "--launch-modded";
            let runtime = if modded {
                launch::ensure_runtime(&paths.cache, &|s| eprintln!("{s}"))?
            } else {
                paths.cache.clone()
            };
            println!(
                "{}",
                launch::launch_game(Path::new(game), &paths.profile, &runtime, modded)?
            );
        }
        _ => bail!("Unknown argument {}", args[0]),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game_fixture(root: &Path, name: &str) -> PathBuf {
        let game = root.join(format!("{name}.app"));
        let contents = game.join("Contents");
        let managed = contents.join("Resources/Data/Managed");
        fs::create_dir_all(&managed).unwrap();
        fs::create_dir_all(contents.join("MacOS")).unwrap();
        for name in [
            "RoR2.dll",
            "RoR2Arm64.ManagedSolo.dll",
            "RoR2Arm64.OfflinePlatform.dll",
        ] {
            fs::write(managed.join(name), []).unwrap();
        }
        fs::write(
            contents.join("MacOS/Risk of Rain 2 Player"),
            b"\xcf\xfa\xed\xfe\x07\x00\x00\x01",
        )
        .unwrap();
        let mut info = plist::Dictionary::new();
        info.insert("CFBundleExecutable".into(), "Risk of Rain 2 Player".into());
        plist::Value::Dictionary(info)
            .to_file_xml(contents.join("Info.plist"))
            .unwrap();
        game
    }

    #[test]
    fn saved_external_game_takes_precedence_over_applications() {
        let temp = tempfile::tempdir().unwrap();
        let saved = game_fixture(temp.path(), "External game");
        let applications = game_fixture(temp.path(), "Applications game");
        assert_eq!(
            resolve_game_path(&saved.to_string_lossy(), &applications),
            Some(saved.canonicalize().unwrap())
        );
    }

    #[test]
    fn invalid_or_missing_saved_game_falls_back_to_applications() {
        let temp = tempfile::tempdir().unwrap();
        let applications = game_fixture(temp.path(), "Risk of Rain 2");
        let invalid = temp.path().join("Unrelated.app");
        fs::create_dir(&invalid).unwrap();
        for saved in [String::new(), invalid.to_string_lossy().into_owned()] {
            assert_eq!(
                resolve_game_path(&saved, &applications),
                Some(applications.canonicalize().unwrap())
            );
        }
    }

    #[test]
    fn unrelated_app_and_missing_paths_require_game_selection() {
        let temp = tempfile::tempdir().unwrap();
        let invalid = temp.path().join("Unrelated.app");
        fs::create_dir(&invalid).unwrap();
        assert_eq!(resolve_game_path("", &invalid), None);
        assert_eq!(
            resolve_game_path(&invalid.to_string_lossy(), &temp.path().join("Missing.app")),
            None
        );
    }

    #[test]
    fn only_primary_russian_selects_russian() {
        assert!(matches!(language_code("ru-US"), Language::Ru));
        assert!(matches!(language_code("ru_RU.UTF-8"), Language::Ru));
        for code in ["en-RU", "de", "russian", "en"] {
            assert!(matches!(language_code(code), Language::En));
        }
    }
}
