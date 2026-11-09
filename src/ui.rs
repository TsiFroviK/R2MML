//! Native Rust interface for R2MML. All filesystem and network work belongs to the app worker.
use std::collections::BTreeSet;

use eframe::egui::{self, Color32, FontId, RichText, Stroke, Vec2};

use crate::catalog::Package;
use crate::install::InstalledMod;
use crate::launch::RUNTIME_VERSION;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    En,
    Ru,
}

impl Language {
    pub fn text<'a>(self, en: &'a str, ru: &'a str) -> &'a str {
        if self == Self::Ru { ru } else { en }
    }
}

pub struct UiData<'a> {
    pub language: Language,
    pub catalog: &'a [Package],
    pub installed: &'a [InstalledMod],
    pub game_path: &'a str,
    pub status: &'a str,
    pub busy: bool,
    pub progress: Option<f32>,
    pub error: Option<&'a str>,
    pub readme: Option<&'a str>,
    pub changelog: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub enum UiAction {
    Refresh,
    Install { package: String, version: String },
    Toggle { package: String, enabled: bool },
    Remove { package: String },
    Launch { modded: bool },
    ChooseGame,
    SaveGame(String),
    OpenUrl(String),
    OpenLogs,
    SelectPackage { package: String, version: String },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Installed,
    Online,
    Settings,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Updated,
    Downloads,
    Name,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailsTab {
    Overview,
    Changelog,
    Dependencies,
}

pub struct UiState {
    page: Page,
    search: String,
    sort: Sort,
    category: String,
    selected: Option<String>,
    details_tab: DetailsTab,
    download: Option<(String, String)>,
    remove: Option<String>,
    game_path_edit: String,
    last_game_path: String,
    styled_dark: Option<bool>,
    #[cfg(debug_assertions)]
    preview_selection: Option<bool>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            page: Page::Installed,
            search: String::new(),
            sort: Sort::Updated,
            category: String::new(),
            selected: None,
            details_tab: DetailsTab::Overview,
            download: None,
            remove: None,
            game_path_edit: String::new(),
            last_game_path: String::new(),
            styled_dark: None,
            #[cfg(debug_assertions)]
            preview_selection: None,
        }
    }
}

const BLUE: Color32 = Color32::from_rgb(0, 122, 255);

impl UiState {
    #[cfg(debug_assertions)]
    pub fn set_preview_page(&mut self, page: &str) {
        self.page = match page {
            "online" | "online-details" | "download" => Page::Online,
            "settings" => Page::Settings,
            _ => Page::Installed,
        };
        self.selected = None;
        self.search.clear();
        self.preview_selection = match page {
            "online-details" => Some(false),
            "download" => Some(true),
            _ => None,
        };
    }

    pub fn show(&mut self, ctx: &egui::Context, data: UiData<'_>) -> Vec<UiAction> {
        self.configure(ctx);
        if self.last_game_path != data.game_path {
            self.game_path_edit = data.game_path.to_owned();
            self.last_game_path = data.game_path.to_owned();
        }
        let mut actions = Vec::new();
        #[cfg(debug_assertions)]
        if let Some(download) = self.preview_selection {
            if let Some(package) = data
                .catalog
                .iter()
                .find(|p| p.full_name == "KingEnderBrine-ProperSave")
                .or_else(|| data.catalog.first())
            {
                self.select(package, &mut actions);
                if download {
                    self.download = package
                        .latest()
                        .map(|version| (package.full_name.clone(), version.version_number.clone()));
                }
                self.preview_selection = None;
            }
        }
        let lang = data.language;
        let dark = ctx.style().visuals.dark_mode;
        let sidebar_fill = if dark {
            Color32::from_rgb(27, 29, 34)
        } else {
            Color32::from_rgb(242, 243, 247)
        };
        let bg = if dark {
            Color32::from_rgb(19, 21, 25)
        } else {
            Color32::from_rgb(251, 251, 253)
        };

        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::new().fill(sidebar_fill).inner_margin(12))
            .show(ctx, |ui| {
                if let Some(error) = data.error {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(
                            Color32::from_rgb(230, 86, 70),
                            lang.text("Something needs attention", "Требуется внимание"),
                        );
                        ui.label(error);
                    });
                }
                ui.horizontal(|ui| {
                    ui.set_min_height(28.0);
                    if data.busy {
                        ui.spinner();
                    }
                    ui.label(
                        RichText::new(if data.status.is_empty() {
                            lang.text("Ready", "Готово")
                        } else {
                            data.status
                        })
                        .small(),
                    );
                    if let Some(progress) = data.progress {
                        ui.add(
                            egui::ProgressBar::new(progress.clamp(0.0, 1.0)).desired_width(140.0),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_sized(
                                [145.0, 28.0],
                                egui::Button::new(
                                    RichText::new(lang.text("Data & logs", "Данные и журналы"))
                                        .size(11.0),
                                ),
                            )
                            .clicked()
                        {
                            actions.push(UiAction::OpenLogs);
                        }
                    });
                });
            });

        egui::SidePanel::left("navigation")
            .exact_width(224.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(sidebar_fill).inner_margin(18))
            .show(ctx, |ui| {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    logo(ui, 42.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("R2MML").size(24.0).strong());
                        ui.label(RichText::new("Risk of Rain 2").size(11.0).weak());
                    });
                });
                ui.add_space(26.0);
                ui.add_enabled_ui(!data.busy && !data.game_path.is_empty(), |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        if ui
                            .add_sized(
                                [151.0, 38.0],
                                egui::Button::new(
                                    RichText::new(lang.text("▶  Play modded", "▶  С модами"))
                                        .color(Color32::WHITE),
                                )
                                .fill(BLUE)
                                .corner_radius(9),
                            )
                            .clicked()
                        {
                            actions.push(UiAction::Launch { modded: true });
                        }
                        ui.menu_button(RichText::new("...").size(17.0), |ui| {
                            if ui
                                .button(lang.text("▶  Play vanilla", "▶  Без модов"))
                                .clicked()
                            {
                                actions.push(UiAction::Launch { modded: false });
                                ui.close();
                            }
                        });
                    });
                });
                ui.add_space(28.0);
                ui.label(
                    RichText::new(lang.text("LIBRARY", "БИБЛИОТЕКА"))
                        .size(10.0)
                        .strong()
                        .weak(),
                );
                ui.add_space(7.0);
                if navigation(
                    ui,
                    lang.text("Installed", "Установлено"),
                    Some(data.installed.len()),
                    self.page == Page::Installed,
                ) {
                    self.page = Page::Installed;
                    self.search.clear();
                    self.category.clear();
                }
                if navigation(
                    ui,
                    lang.text("Online", "Каталог"),
                    Some(data.catalog.len()),
                    self.page == Page::Online,
                ) {
                    self.page = Page::Online;
                    self.search.clear();
                }
                ui.add_space(18.0);
                ui.separator();
                ui.add_space(10.0);
                if navigation(
                    ui,
                    lang.text("Settings", "Настройки"),
                    None,
                    self.page == Page::Settings,
                ) {
                    self.page = Page::Settings;
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("R2MML {}", env!("CARGO_PKG_VERSION")))
                            .size(11.0)
                            .weak(),
                    );
                });
            });

        if self.page != Page::Settings {
            if let Some(name) = self.selected.clone() {
                let package = data.catalog.iter().find(|p| p.full_name == name);
                let installed = data.installed.iter().find(|p| p.full_name == name);
                if package.is_some() || installed.is_some() {
                    egui::SidePanel::right("details")
                        .default_width(350.0)
                        .min_width(290.0)
                        .max_width(470.0)
                        .frame(egui::Frame::new().fill(bg).inner_margin(22))
                        .show(ctx, |ui| {
                            self.details(ui, &data, package, installed, &mut actions)
                        });
                }
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(bg).inner_margin(24))
            .show(ctx, |ui| match self.page {
                Page::Settings => self.settings(ui, &data, &mut actions),
                Page::Installed | Page::Online => self.library(ui, &data, &mut actions),
            });
        self.download_dialog(ctx, &data, &mut actions);
        self.remove_dialog(ctx, &data, &mut actions);
        actions
    }

    fn configure(&mut self, ctx: &egui::Context) {
        let dark = ctx.style().visuals.dark_mode;
        if self.styled_dark == Some(dark) {
            return;
        }
        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(10.0, 10.0);
        style.spacing.button_padding = Vec2::new(12.0, 8.0);
        style.spacing.interact_size.y = 30.0;
        style.visuals.selection.bg_fill = BLUE.gamma_multiply(0.23);
        style.visuals.selection.stroke = Stroke::new(1.0_f32, BLUE);
        style.visuals.hyperlink_color = BLUE;
        style.visuals.override_text_color = Some(if dark {
            Color32::from_rgb(231, 233, 238)
        } else {
            Color32::from_rgb(33, 35, 39)
        });
        style.visuals.weak_text_color = Some(if dark {
            Color32::from_rgb(147, 151, 156)
        } else {
            Color32::from_rgb(102, 107, 114)
        });
        style.visuals.widgets.inactive.corner_radius = 8.into();
        style.visuals.widgets.hovered.corner_radius = 8.into();
        style.visuals.widgets.active.corner_radius = 8.into();
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(13.0));
        style
            .text_styles
            .insert(egui::TextStyle::Small, FontId::proportional(11.0));
        style
            .text_styles
            .insert(egui::TextStyle::Heading, FontId::proportional(26.0));
        ctx.set_style(style);
        self.styled_dark = Some(dark);
    }

    fn library(&mut self, ui: &mut egui::Ui, data: &UiData<'_>, actions: &mut Vec<UiAction>) {
        let lang = data.language;
        let online = self.page == Page::Online;
        ui.horizontal(|ui| {
            ui.heading(if online {
                lang.text("Discover mods", "Найдите новые моды")
            } else {
                lang.text("Your collection", "Ваша коллекция")
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !data.busy,
                        egui::Button::new(lang.text("Refresh", "Обновить")),
                    )
                    .clicked()
                {
                    actions.push(UiAction::Refresh);
                }
            });
        });
        ui.label(
            RichText::new(if online {
                lang.text(
                    "Thunderstore · Risk of Rain 2",
                    "Thunderstore · Risk of Rain 2",
                )
            } else {
                lang.text(
                    "Choose what joins your next run.",
                    "Выберите моды для следующего забега.",
                )
            })
            .weak(),
        );
        ui.add_space(15.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text(lang.text("Search mods, authors…", "Поиск модов, авторов…"))
                .desired_width(f32::INFINITY)
                .margin(Vec2::new(12.0, 9.0)),
        );
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("sort")
                .selected_text(match self.sort {
                    Sort::Updated => lang.text("Recently updated", "Недавно обновлённые"),
                    Sort::Downloads => lang.text("Most downloaded", "По скачиваниям"),
                    Sort::Name => lang.text("Name A–Z", "По имени"),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.sort,
                        Sort::Updated,
                        lang.text("Recently updated", "Недавно обновлённые"),
                    );
                    ui.selectable_value(
                        &mut self.sort,
                        Sort::Downloads,
                        lang.text("Most downloaded", "По скачиваниям"),
                    );
                    ui.selectable_value(
                        &mut self.sort,
                        Sort::Name,
                        lang.text("Name A–Z", "По имени"),
                    );
                });
            if online {
                let categories: BTreeSet<&str> = data
                    .catalog
                    .iter()
                    .flat_map(|p| p.categories.iter().map(String::as_str))
                    .collect();
                egui::ComboBox::from_id_salt("category")
                    .selected_text(if self.category.is_empty() {
                        lang.text("All categories", "Все категории")
                    } else {
                        &self.category
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.category,
                            String::new(),
                            lang.text("All categories", "Все категории"),
                        );
                        for category in categories {
                            ui.selectable_value(&mut self.category, category.to_owned(), category);
                        }
                    });
            }
        });
        ui.add_space(10.0);
        let needle = self.search.to_lowercase();
        if online {
            let mut packages: Vec<&Package> = data
                .catalog
                .iter()
                .filter(|p| {
                    let hay = format!(
                        "{} {} {}",
                        p.name,
                        p.owner,
                        p.latest().map(|v| v.description.as_str()).unwrap_or("")
                    )
                    .to_lowercase();
                    hay.contains(&needle)
                        && (self.category.is_empty() || p.categories.contains(&self.category))
                })
                .collect();
            packages.sort_by(|a, b| match self.sort {
                Sort::Updated => b
                    .date_updated
                    .cmp(&a.date_updated)
                    .then_with(|| a.name.cmp(&b.name)),
                Sort::Downloads => b
                    .total_downloads()
                    .cmp(&a.total_downloads())
                    .then_with(|| a.name.cmp(&b.name)),
                Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            });
            ui.label(
                RichText::new(format!(
                    "{} {}",
                    packages.len(),
                    lang.text("packages", "модов")
                ))
                .small()
                .weak(),
            );
            if packages.is_empty() {
                empty(
                    ui,
                    lang,
                    if data.catalog.is_empty() {
                        lang.text("A whole new way to play", "Новые возможности для игры")
                    } else {
                        lang.text("No matches", "Ничего не найдено")
                    },
                    if data.catalog.is_empty() {
                        lang.text(
                            "Refresh to load the Thunderstore catalog.",
                            "Нажмите «Обновить», чтобы загрузить каталог Thunderstore.",
                        )
                    } else {
                        lang.text(
                            "Try another name or category.",
                            "Попробуйте другое имя или категорию.",
                        )
                    },
                );
            }
            egui::ScrollArea::vertical()
                .id_salt("online_scroll")
                .auto_shrink([false, false])
                .show_rows(ui, 82.0, packages.len(), |ui, range| {
                    for index in range {
                        let package = packages[index];
                        let version = package.latest();
                        let installed = data
                            .installed
                            .iter()
                            .any(|p| p.full_name == package.full_name);
                        if package_row(
                            ui,
                            &package.full_name,
                            &package.name,
                            &package.owner,
                            version.map(|v| v.version_number.as_str()).unwrap_or(""),
                            version.map(|v| v.icon.as_str()).unwrap_or(""),
                            version.map(|v| v.description.as_str()).unwrap_or(""),
                            self.selected.as_deref() == Some(&package.full_name),
                            if installed {
                                Some(lang.text("Installed", "Установлен"))
                            } else {
                                None
                            },
                            None,
                            data.busy,
                            lang,
                        )
                        .0
                        {
                            self.select(package, actions);
                        }
                    }
                });
        } else {
            let mut installed: Vec<&InstalledMod> = data
                .installed
                .iter()
                .filter(|p| {
                    format!("{} {}", p.name, p.owner)
                        .to_lowercase()
                        .contains(&needle)
                })
                .collect();
            installed.sort_by(|a, b| {
                let a_catalog = data.catalog.iter().find(|p| p.full_name == a.full_name);
                let b_catalog = data.catalog.iter().find(|p| p.full_name == b.full_name);
                let order = match self.sort {
                    Sort::Updated => b_catalog
                        .map(|p| &p.date_updated)
                        .cmp(&a_catalog.map(|p| &p.date_updated)),
                    Sort::Downloads => b_catalog
                        .map(|p| p.total_downloads())
                        .cmp(&a_catalog.map(|p| p.total_downloads())),
                    Sort::Name => std::cmp::Ordering::Equal,
                };
                order.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            ui.label(
                RichText::new(format!(
                    "{} {}",
                    installed.len(),
                    lang.text("installed", "установлено")
                ))
                .small()
                .weak(),
            );
            if installed.is_empty() {
                empty(
                    ui,
                    lang,
                    lang.text("Make it your game", "Играйте по-своему"),
                    lang.text(
                        "Find a mod in Online. Its dependencies come along automatically.",
                        "Выберите мод в каталоге. Зависимости установятся автоматически.",
                    ),
                );
                if ui
                    .button(lang.text("Browse Thunderstore", "Открыть каталог"))
                    .clicked()
                {
                    self.page = Page::Online;
                    self.search.clear();
                }
            }
            egui::ScrollArea::vertical()
                .id_salt("installed_scroll")
                .auto_shrink([false, false])
                .show_rows(ui, 82.0, installed.len(), |ui, range| {
                    for index in range {
                        let item = installed[index];
                        let (selected, toggle) = package_row(
                            ui,
                            &item.full_name,
                            &item.name,
                            &item.owner,
                            if item.full_name == "bbepis-BepInExPack" {
                                RUNTIME_VERSION
                            } else {
                                &item.version
                            },
                            &item.icon,
                            &item.description,
                            self.selected.as_deref() == Some(&item.full_name),
                            if item.full_name == "bbepis-BepInExPack" {
                                Some(lang.text("macOS runtime", "Среда macOS"))
                            } else if item.explicit {
                                None
                            } else {
                                Some(lang.text("Dependency", "Зависимость"))
                            },
                            Some(item.enabled),
                            data.busy,
                            lang,
                        );
                        if let Some(enabled) = toggle {
                            actions.push(UiAction::Toggle {
                                package: item.full_name.clone(),
                                enabled,
                            });
                        }
                        if selected {
                            self.selected = Some(item.full_name.clone());
                            self.details_tab = DetailsTab::Overview;
                            actions.push(UiAction::SelectPackage {
                                package: item.full_name.clone(),
                                version: item.version.clone(),
                            });
                        }
                    }
                });
        }
    }

    fn select(&mut self, package: &Package, actions: &mut Vec<UiAction>) {
        self.selected = Some(package.full_name.clone());
        self.details_tab = DetailsTab::Overview;
        if let Some(version) = package.latest() {
            actions.push(UiAction::SelectPackage {
                package: package.full_name.clone(),
                version: version.version_number.clone(),
            });
        }
    }

    fn details(
        &mut self,
        ui: &mut egui::Ui,
        data: &UiData<'_>,
        package: Option<&Package>,
        installed: Option<&InstalledMod>,
        actions: &mut Vec<UiAction>,
    ) {
        let lang = data.language;
        let runtime_package = package
            .map(|p| p.full_name.as_str())
            .or_else(|| installed.map(|p| p.full_name.as_str()))
            == Some("bbepis-BepInExPack");
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(lang.text("MOD DETAILS", "О МОДЕ"))
                    .small()
                    .weak(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("×")
                    .on_hover_text(lang.text("Close details", "Закрыть описание"))
                    .clicked()
                {
                    self.selected = None;
                }
            });
        });
        ui.add_space(12.0);
        let version = package.and_then(|p| {
            if self.page == Page::Installed {
                installed
                    .and_then(|item| p.versions.iter().find(|v| v.version_number == item.version))
                    .or_else(|| p.latest())
            } else {
                p.latest()
            }
        });
        let name = package
            .map(|p| p.name.as_str())
            .or_else(|| installed.map(|p| p.name.as_str()))
            .unwrap_or("");
        let owner = package
            .map(|p| p.owner.as_str())
            .or_else(|| installed.map(|p| p.owner.as_str()))
            .unwrap_or("");
        let icon = version
            .map(|v| v.icon.as_str())
            .or_else(|| installed.map(|p| p.icon.as_str()))
            .unwrap_or("");
        mod_icon(ui, icon, name, 62.0);
        ui.add_space(9.0);
        ui.label(RichText::new(name.replace('_', " ")).size(25.0).strong());
        ui.label(RichText::new(format!("{} {}", lang.text("by", "Автор:"), owner)).weak());
        ui.add_space(10.0);
        if let Some(version) = version {
            ui.label(&version.description);
            ui.add_space(10.0);
            if let Some(package) = package {
                ui.horizontal(|ui| {
                    metric(
                        ui,
                        &compact_count(package.total_downloads()),
                        lang.text("Downloads", "Скачивания"),
                    );
                    ui.separator();
                    metric(
                        ui,
                        &version.version_number,
                        if runtime_package && self.page == Page::Installed {
                            lang.text("Dependency version", "Версия зависимости")
                        } else if self.page == Page::Installed {
                            lang.text("Installed version", "Установленная версия")
                        } else {
                            lang.text("Latest version", "Последняя версия")
                        },
                    );
                });
                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!(
                        "{} {}",
                        lang.text("Updated", "Обновлён"),
                        package
                            .date_updated
                            .split('T')
                            .next()
                            .unwrap_or(&package.date_updated)
                    ))
                    .small()
                    .weak(),
                );
            }
        } else if let Some(installed) = installed {
            ui.label(&installed.description);
        }
        if runtime_package {
            ui.add_space(12.0);
            egui::Frame::group(ui.style())
                .corner_radius(9)
                .show(ui, |ui| {
                    ui.label(RichText::new(format!("BepInEx {RUNTIME_VERSION} · macOS")).strong());
                    ui.label(
                        RichText::new(lang.text(
                            "R2MML provides this dependency with the native macOS runtime.",
                            "R2MML предоставляет эту зависимость через нативную среду macOS.",
                        ))
                        .small()
                        .weak(),
                    );
                    if let Some(item) = installed {
                        ui.label(
                            RichText::new(format!(
                                "{} {}",
                                lang.text("Dependency version:", "Версия зависимости:"),
                                item.version
                            ))
                            .small(),
                        );
                    }
                });
        }
        ui.add_space(15.0);
        ui.add_enabled_ui(!data.busy, |ui| {
            if let Some(package) = package {
                if let Some(version) = version {
                    if ui
                        .add_sized(
                            [ui.available_width(), 38.0],
                            egui::Button::new(
                                RichText::new(if installed.is_some() {
                                    lang.text("Change version…", "Выбрать версию…")
                                } else {
                                    lang.text("Download…", "Скачать…")
                                })
                                .color(Color32::WHITE),
                            )
                            .fill(BLUE),
                        )
                        .clicked()
                    {
                        self.download =
                            Some((package.full_name.clone(), version.version_number.clone()));
                    }
                }
                if ui
                    .button(lang.text("View on Thunderstore ↗", "Открыть в Thunderstore ↗"))
                    .clicked()
                {
                    actions.push(UiAction::OpenUrl(package.package_url.clone()));
                }
            }
            if let Some(item) = installed {
                ui.add_space(4.0);
                if ui
                    .button(
                        RichText::new(lang.text("Uninstall", "Удалить"))
                            .color(Color32::from_rgb(230, 86, 70)),
                    )
                    .clicked()
                {
                    self.remove = Some(item.full_name.clone());
                }
            }
        });
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.details_tab, DetailsTab::Overview, "README");
            ui.selectable_value(
                &mut self.details_tab,
                DetailsTab::Changelog,
                lang.text("Changelog", "Изменения"),
            );
            let count = installed
                .map(|p| p.dependencies.len())
                .or_else(|| version.map(|v| v.dependencies.len()))
                .unwrap_or(0);
            ui.selectable_value(
                &mut self.details_tab,
                DetailsTab::Dependencies,
                format!("{} ({count})", lang.text("Dependencies", "Зависимости")),
            );
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("readme_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| match self.details_tab {
                DetailsTab::Overview => match data.readme {
                    Some(text) => markdown(ui, text, actions),
                    None => {
                        ui.label(
                            RichText::new(lang.text(
                                "Select a mod to load its README.",
                                "Описание мода загружается при выборе.",
                            ))
                            .weak(),
                        );
                    }
                },
                DetailsTab::Changelog => match data.changelog {
                    Some(text) if !text.trim().is_empty() => markdown(ui, text, actions),
                    Some(_) => {
                        ui.label(lang.text(
                            "No changelog was provided for this version.",
                            "Для этой версии список изменений не опубликован.",
                        ));
                    }
                    None => {
                        ui.label(
                            RichText::new(lang.text("Loading changelog…", "Загрузка изменений…"))
                                .weak(),
                        );
                    }
                },
                DetailsTab::Dependencies => {
                    let dependencies = installed
                        .map(|p| p.dependencies.as_slice())
                        .or_else(|| version.map(|v| v.dependencies.as_slice()))
                        .unwrap_or(&[]);
                    if dependencies.is_empty() {
                        ui.label(lang.text(
                            "No additional dependencies.",
                            "Дополнительных зависимостей нет.",
                        ));
                    }
                    for dependency in dependencies {
                        egui::Frame::group(ui.style())
                            .corner_radius(8)
                            .show(ui, |ui| {
                                ui.label(dependency);
                            });
                    }
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(lang.text(
                            "Required packages are installed automatically.",
                            "Необходимые пакеты устанавливаются автоматически.",
                        ))
                        .small()
                        .weak(),
                    );
                }
            });
    }

    fn settings(&mut self, ui: &mut egui::Ui, data: &UiData<'_>, actions: &mut Vec<UiAction>) {
        let lang = data.language;
        ui.heading(lang.text("Settings", "Настройки"));
        ui.label(
            RichText::new(lang.text(
                "A separate home for your mods.",
                "Отдельное место для ваших модов.",
            ))
            .weak(),
        );
        ui.add_space(26.0);
        egui::Frame::group(ui.style())
            .inner_margin(20)
            .corner_radius(14)
            .show(ui, |ui| {
                ui.set_min_width((ui.available_width() - 4.0).max(250.0));
                ui.label(
                    RichText::new(lang.text("Risk2Mac application", "Приложение Risk2Mac"))
                        .strong(),
                );
                ui.label(lang.text(
                    "Choose the native Risk of Rain 2.app created with Risk2Mac.",
                    "Выберите нативное приложение Risk of Rain 2.app, собранное через Risk2Mac.",
                ));
                ui.add_space(8.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.game_path_edit)
                        .desired_width(f32::INFINITY)
                        .hint_text("/Applications/Risk of Rain 2.app"),
                );
                ui.add_enabled_ui(!data.busy, |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .button(lang.text("Choose application…", "Выбрать приложение…"))
                            .clicked()
                        {
                            actions.push(UiAction::ChooseGame);
                        }
                        if ui
                            .button(lang.text("Save path", "Сохранить путь"))
                            .clicked()
                        {
                            actions.push(UiAction::SaveGame(self.game_path_edit.trim().to_owned()));
                        }
                    });
                });
            });
        ui.add_space(16.0);
        ui.label(format!(
            "{}: {}",
            lang.text("Interface language", "Язык интерфейса"),
            lang.text("English · follows your Mac", "Русский · язык вашего Mac")
        ));
        ui.label(RichText::new(lang.text("Russian is used when it is the first system language; all other languages use English.", "Русский используется, если он первый язык системы; для остальных языков используется английский.")).small().weak());
        ui.add_space(16.0);
        if ui
            .button(lang.text("Open data & logs", "Данные и журналы"))
            .clicked()
        {
            actions.push(UiAction::OpenLogs);
        }
    }

    fn download_dialog(
        &mut self,
        ctx: &egui::Context,
        data: &UiData<'_>,
        actions: &mut Vec<UiAction>,
    ) {
        let Some((package_name, mut selected_version)) = self.download.clone() else {
            return;
        };
        let Some(package) = data.catalog.iter().find(|p| p.full_name == package_name) else {
            self.download = None;
            return;
        };
        let lang = data.language;
        let mut close = false;
        let modal =
            egui::Modal::new(egui::Id::new("download_modal")).show(ctx, |ui| {
                ui.set_width(440.0);
                ui.heading(lang.text("Add to your collection", "Добавить в коллекцию"));
                ui.label(
                    RichText::new(package.name.replace('_', " "))
                        .size(19.0)
                        .strong(),
                );
                ui.label(
                    RichText::new(lang.text(
                        "Choose a version. R2MML will include its required dependencies.",
                        "Выберите версию. R2MML также установит необходимые зависимости.",
                    ))
                    .weak(),
                );
                ui.add_space(16.0);
                egui::ComboBox::from_id_salt("download_version")
                    .width(200.0)
                    .selected_text(&selected_version)
                    .show_ui(ui, |ui| {
                        for version in &package.versions {
                            if version.is_active {
                                ui.selectable_value(
                                    &mut selected_version,
                                    version.version_number.clone(),
                                    &version.version_number,
                                );
                            }
                        }
                    });
                if let Some(version) = package
                    .versions
                    .iter()
                    .find(|v| v.version_number == selected_version)
                {
                    let latest = package
                        .latest()
                        .is_some_and(|v| v.version_number == selected_version);
                    if latest {
                        ui.colored_label(
                            Color32::from_rgb(45, 160, 95),
                            lang.text("Latest version", "Последняя версия"),
                        );
                    }
                    ui.label(format!(
                        "{} · {} {}",
                        size_label(version.file_size),
                        version.dependencies.len(),
                        lang.text("dependencies", "зависимостей")
                    ));
                }
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !data.busy,
                            egui::Button::new(
                                RichText::new(lang.text(
                                    "Install with dependencies",
                                    "Установить с зависимостями",
                                ))
                                .color(Color32::WHITE),
                            )
                            .fill(BLUE),
                        )
                        .clicked()
                    {
                        actions.push(UiAction::Install {
                            package: package_name.clone(),
                            version: selected_version.clone(),
                        });
                        close = true;
                    }
                    if ui.button(lang.text("Cancel", "Отмена")).clicked() {
                        close = true;
                    }
                });
            });
        self.download = if close || modal.should_close() {
            None
        } else {
            Some((package_name, selected_version))
        };
    }

    fn remove_dialog(
        &mut self,
        ctx: &egui::Context,
        data: &UiData<'_>,
        actions: &mut Vec<UiAction>,
    ) {
        let Some(name) = self.remove.clone() else {
            return;
        };
        let lang = data.language;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("remove_modal")).show(ctx, |ui| {
            ui.set_width(410.0);
            ui.heading(lang.text("Remove this mod?", "Удалить этот мод?"));
            ui.label(&name);
            ui.label(lang.text("Its configuration files will be kept. A package needed by another installed mod cannot be removed.", "Файлы настроек сохранятся. Пакет, необходимый другому установленному моду, удалить нельзя."));
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!data.busy, egui::Button::new(lang.text("Uninstall", "Удалить"))).clicked() { actions.push(UiAction::Remove { package: name.clone() }); close = true; }
                if ui.button(lang.text("Cancel", "Отмена")).clicked() { close = true; }
            });
        });
        if close || modal.should_close() {
            self.remove = None;
        }
    }
}

fn navigation(ui: &mut egui::Ui, name: &str, count: Option<usize>, selected: bool) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            ui.is_enabled(),
            selected,
            count
                .map(|value| format!("{name}, {value}"))
                .unwrap_or_else(|| name.to_owned()),
        )
    });
    let background = if selected {
        BLUE.gamma_multiply(if ui.visuals().dark_mode { 0.30 } else { 0.14 })
    } else if response.hovered() {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 8.0, background);
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            8.0,
            Stroke::new(1.0_f32, BLUE),
            egui::StrokeKind::Inside,
        );
    }
    let mut label_right = rect.right() - 12.0;
    if let Some(count) = count {
        let color = if selected {
            Color32::from_rgb(23, 48, 77)
        } else {
            Color32::WHITE
        };
        let galley =
            ui.painter()
                .layout_no_wrap(count.to_string(), FontId::proportional(11.0), color);
        let badge_width = (galley.size().x + 14.0).max(22.0);
        let badge = egui::Rect::from_center_size(
            egui::pos2(rect.right() - 12.0 - badge_width / 2.0, rect.center().y),
            Vec2::new(badge_width, 22.0),
        );
        ui.painter().rect_filled(
            badge,
            5.0,
            if selected {
                Color32::from_rgb(232, 241, 255)
            } else {
                BLUE
            },
        );
        ui.painter()
            .galley(badge.center() - galley.size() / 2.0, galley, color);
        label_right = badge.left() - 10.0;
    }
    let label_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 12.0, rect.top()),
        egui::pos2(label_right, rect.bottom()),
    );
    ui.painter().with_clip_rect(label_rect).text(
        egui::pos2(label_rect.left(), label_rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        FontId::proportional(13.0),
        ui.visuals().text_color(),
    );
    response.clicked()
}

fn logo(ui: &mut egui::Ui, size: f32) {
    ui.add(
        egui::Image::new(egui::include_image!("../assets/r2mml-icon.png"))
            .fit_to_exact_size(Vec2::splat(size)),
    );
}

fn mod_icon(ui: &mut egui::Ui, url: &str, name: &str, size: f32) {
    if url.starts_with("https://") {
        ui.add(
            egui::Image::new(url)
                .fit_to_exact_size(Vec2::splat(size))
                .corner_radius(9),
        );
    } else {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 9.0, BLUE.gamma_multiply(0.18));
        let initials: String = name
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(2)
            .collect();
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            initials,
            FontId::proportional(size * 0.32),
            BLUE,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn package_row(
    ui: &mut egui::Ui,
    id: &str,
    name: &str,
    owner: &str,
    version: &str,
    icon: &str,
    description: &str,
    selected: bool,
    badge: Option<&str>,
    enabled: Option<bool>,
    busy: bool,
    lang: Language,
) -> (bool, Option<bool>) {
    let mut toggle = None;
    let selected_fill = BLUE.gamma_multiply(if ui.visuals().dark_mode { 0.19 } else { 0.10 });
    let normal_fill = if ui.visuals().dark_mode {
        Color32::from_rgb(29, 31, 37)
    } else {
        Color32::WHITE
    };
    let response = ui
        .push_id(id, |ui| {
            egui::Frame::new()
                .fill(if selected { selected_fill } else { normal_fill })
                .corner_radius(12)
                .inner_margin(12)
                .stroke(Stroke::new(
                    1.0_f32,
                    if selected {
                        BLUE.gamma_multiply(0.45)
                    } else {
                        ui.visuals()
                            .widgets
                            .noninteractive
                            .bg_stroke
                            .color
                            .gamma_multiply(0.35)
                    },
                ))
                .show(ui, |ui| {
                    ui.set_min_width((ui.available_width() - 2.0).max(100.0));
                    ui.horizontal(|ui| {
                        mod_icon(ui, icon, name, 44.0);
                        let reserved = if enabled.is_some() { 48.0 } else { 8.0 };
                        ui.allocate_ui_with_layout(
                            Vec2::new((ui.available_width() - reserved).max(80.0), 44.0),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                ui.spacing_mut().item_spacing.y = 3.0;
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(name.replace('_', " ")).strong(),
                                    )
                                    .truncate(),
                                );
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "{owner}  ·  {version}{}",
                                            badge.map(|s| format!("  ·  {s}")).unwrap_or_default()
                                        ))
                                        .size(11.0)
                                        .weak(),
                                    )
                                    .truncate(),
                                );
                                if !description.is_empty() {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(description).size(11.0).weak(),
                                        )
                                        .truncate(),
                                    );
                                }
                            },
                        );
                        if let Some(value) = enabled {
                            ui.add_enabled_ui(!busy, |ui| {
                                let (rect, response) = ui.allocate_exact_size(
                                    Vec2::new(34.0, 22.0),
                                    egui::Sense::click(),
                                );
                                let fill = if value {
                                    BLUE
                                } else {
                                    ui.visuals().widgets.inactive.bg_fill
                                };
                                ui.painter().rect_filled(rect, 11.0, fill);
                                ui.painter().circle_filled(
                                    egui::pos2(
                                        if value {
                                            rect.right() - 11.0
                                        } else {
                                            rect.left() + 11.0
                                        },
                                        rect.center().y,
                                    ),
                                    8.0,
                                    Color32::WHITE,
                                );
                                if response.clicked() {
                                    toggle = Some(!value);
                                }
                                response.on_hover_text(if value {
                                    lang.text("Disable", "Отключить")
                                } else {
                                    lang.text("Enable", "Включить")
                                });
                            });
                        }
                    });
                })
                .response
                .interact(egui::Sense::click())
        })
        .inner;
    (response.clicked(), toggle)
}

fn empty(ui: &mut egui::Ui, _lang: Language, title: &str, description: &str) {
    ui.add_space(55.0);
    ui.label(RichText::new(title).size(23.0).strong());
    ui.add_space(8.0);
    ui.label(RichText::new(description).weak());
    ui.add_space(16.0);
}

fn metric(ui: &mut egui::Ui, number: &str, caption: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(number).size(18.0).strong());
        ui.label(RichText::new(caption).small().weak());
    });
}

fn compact_count(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}K", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

fn size_label(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else {
        format!("{:.0} KB", bytes as f64 / 1_024.0)
    }
}

// A deliberately small, non-HTML README renderer. Remote markup never executes.
fn markdown(ui: &mut egui::Ui, text: &str, actions: &mut Vec<UiAction>) {
    let mut code = false;
    for line in text.lines().take(1_200) {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            code = !code;
            continue;
        }
        if code {
            ui.label(RichText::new(line).monospace().size(11.0));
            continue;
        }
        if trimmed.is_empty() {
            ui.add_space(6.0);
            continue;
        }
        if trimmed.starts_with('<') {
            continue;
        }
        if let Some(title) = trimmed
            .strip_prefix("### ")
            .or_else(|| trimmed.strip_prefix("## "))
            .or_else(|| trimmed.strip_prefix("# "))
        {
            ui.add_space(6.0);
            ui.label(RichText::new(title).size(18.0).strong());
            continue;
        }
        if trimmed.starts_with("![") {
            continue;
        }
        if let Some(link) = trimmed.strip_prefix('[') {
            if let Some((label, rest)) = link.split_once("](") {
                if let Some(url) = rest.strip_suffix(')') {
                    if url.starts_with("https://") {
                        if ui.link(label).clicked() {
                            actions.push(UiAction::OpenUrl(url.to_owned()));
                        }
                        continue;
                    }
                }
            }
        }
        ui.label(trimmed.replace("**", "").replace("__", "").replace('`', ""));
    }
}
