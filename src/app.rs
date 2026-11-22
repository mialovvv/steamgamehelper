use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, vec2, Align, Align2, CentralPanel, Color32, ColorImage, ComboBox, CornerRadius,
    Frame, Layout, Margin, Panel, Pos2, Rect, RichText, ScrollArea, Sense, Stroke,
    TextureHandle, TextureOptions, Ui,
};

use crate::steam::{
    find_steam_path, get_active_steam_info, launch_game, load_accounts, load_games,
    prepare_steam_autologin, set_account_offline_mode, shutdown_steam_cleanly, ActiveSteamInfo,
    SteamAccount, SteamGame,
};

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ActiveTab {
    Games,
    Accounts,
    Settings,
}

enum BackgroundMessage {
    StatusUpdate(String, bool),
    RefreshNeeded,
}

// Graphite Neutral Palette (from Unslop tokens)
const COLOR_SIDEBAR: Color32 = Color32::from_rgb(22, 22, 24);
const COLOR_BACKGROUND: Color32 = Color32::from_rgb(27, 27, 30);
const COLOR_CARD: Color32 = Color32::from_rgb(34, 34, 38);
const COLOR_CARD_HOVER: Color32 = Color32::from_rgb(40, 40, 45);
const COLOR_HAIRLINE: Color32 = Color32::from_rgba_premultiplied(18, 18, 18, 18);
const COLOR_TEXT_PRIMARY: Color32 = Color32::from_rgb(237, 237, 237);
const COLOR_TEXT_SECONDARY: Color32 = Color32::from_rgb(161, 161, 166);
const COLOR_TEXT_MUTED: Color32 = Color32::from_rgb(117, 117, 122);
const COLOR_ACCENT: Color32 = Color32::from_rgb(0, 120, 212);
const COLOR_ONLINE: Color32 = Color32::from_rgb(76, 195, 138);
const COLOR_OFFLINE: Color32 = Color32::from_rgb(212, 136, 0);

pub struct SteamHelperApp {
    steam_path: Option<PathBuf>,
    accounts: Vec<SteamAccount>,
    games: Vec<SteamGame>,
    active_info: ActiveSteamInfo,

    // UI State
    active_tab: ActiveTab,
    search_query: String,
    selected_account_filter: Option<String>,
    status_toast: Option<(String, bool, Instant)>,
    is_busy: bool,
    busy_message: String,

    chosen_game_accounts: HashMap<u32, String>,
    image_cache: HashMap<u32, TextureHandle>,

    tx: Sender<BackgroundMessage>,
    rx: Receiver<BackgroundMessage>,
}

impl SteamHelperApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = channel();
        let steam_path = find_steam_path().ok();
        let active_info = get_active_steam_info();

        let mut app = Self {
            steam_path,
            accounts: Vec::new(),
            games: Vec::new(),
            active_info,
            active_tab: ActiveTab::Games,
            search_query: String::new(),
            selected_account_filter: None,
            status_toast: None,
            is_busy: false,
            busy_message: String::new(),
            chosen_game_accounts: HashMap::new(),
            image_cache: HashMap::new(),
            tx,
            rx,
        };

        app.refresh_data();
        app
    }

    pub fn refresh_data(&mut self) {
        self.active_info = get_active_steam_info();
        let path = match self.steam_path.clone() {
            Some(p) => p,
            None => {
                self.set_toast("Steam не обнаружен в системе".to_string(), false);
                return;
            }
        };

        match load_accounts(&path, self.active_info.active_user_id32) {
            Ok(accs) => {
                self.accounts = accs;
            }
            Err(err) => {
                self.set_toast(format!("Не удалось прочитать аккаунты: {}", err), false);
            }
        }

        match load_games(&path, &self.accounts) {
            Ok(g) => {
                for game in &g {
                    if !self.chosen_game_accounts.contains_key(&game.appid) {
                        if let Some(first_acc) = game.accounts.first() {
                            self.chosen_game_accounts.insert(game.appid, first_acc.clone());
                        }
                    }
                }
                self.games = g;
            }
            Err(err) => {
                self.set_toast(format!("Не удалось загрузить игры: {}", err), false);
            }
        }
    }

    fn set_toast(&mut self, msg: String, is_success: bool) {
        self.status_toast = Some((msg, is_success, Instant::now()));
    }

    fn get_account_by_id(&self, steam_id64: &str) -> Option<&SteamAccount> {
        self.accounts.iter().find(|a| a.steam_id64 == steam_id64)
    }

    fn get_active_account(&self) -> Option<&SteamAccount> {
        self.accounts.iter().find(|a| a.is_active)
            .or_else(|| {
                if let Some(ref name) = self.active_info.active_account_name {
                    self.accounts.iter().find(|a| &a.account_name == name)
                } else {
                    None
                }
            })
    }

    fn toggle_account_offline(&mut self, steam_id64: &str) {
        let Some(path) = self.steam_path.clone() else { return };
        let Some(acc) = self.get_account_by_id(steam_id64).cloned() else { return };

        let new_state = !acc.wants_offline_mode;
        match set_account_offline_mode(&path, steam_id64, new_state) {
            Ok(()) => {
                let state_str = if new_state { "автономный режим" } else { "режим онлайн" };
                self.set_toast(
                    format!("{}: установлен {}", acc.persona_name, state_str),
                    true,
                );
                self.refresh_data();
            }
            Err(e) => {
                self.set_toast(format!("Ошибка: {}", e), false);
            }
        }
    }

    fn switch_to_account_and_launch(
        &mut self,
        target_account: SteamAccount,
        appid: Option<u32>,
        game_name: Option<String>,
    ) {
        let Some(path) = self.steam_path.clone() else { return };
        self.is_busy = true;
        let title = game_name.unwrap_or_else(|| "Steam".to_string());
        self.busy_message = format!("Запуск {}...", title);

        let tx = self.tx.clone();
        let target_steam_id = target_account.steam_id64.clone();
        let target_account_name = target_account.account_name.clone();

        thread::spawn(move || {
            let active = get_active_steam_info();
            let need_switch = !active.is_running
                || active.active_user_id32 != target_account.account_id32
                || active.active_account_name.as_deref() != Some(&target_account_name);

            if need_switch && active.is_running {
                tx.send(BackgroundMessage::StatusUpdate(
                    "Завершение текущего сеанса...".to_string(),
                    true,
                )).ok();
                shutdown_steam_cleanly(&path);
            }

            if need_switch {
                tx.send(BackgroundMessage::StatusUpdate(
                    format!("Переключение на {}...", target_account_name),
                    true,
                )).ok();

                if let Err(e) = prepare_steam_autologin(&path, &target_steam_id, &target_account_name) {
                    tx.send(BackgroundMessage::StatusUpdate(format!("Ошибка авто-входа: {}", e), false)).ok();
                    return;
                }
            }

            tx.send(BackgroundMessage::StatusUpdate(
                format!("Запуск {}...", title),
                true,
            )).ok();

            match launch_game(&path, appid) {
                Ok(()) => {
                    tx.send(BackgroundMessage::StatusUpdate(
                        format!("{} запущена", title),
                        true,
                    )).ok();
                }
                Err(e) => {
                    tx.send(BackgroundMessage::StatusUpdate(format!("Ошибка запуска: {}", e), false)).ok();
                }
            }

            thread::sleep(Duration::from_millis(1500));
            tx.send(BackgroundMessage::RefreshNeeded).ok();
        });
    }

    /// Quiet, craft-focused navigation button (No emojis, precise typography, 4px grid)
    fn render_nav_button(ui: &mut Ui, label: &str, badge_count: Option<usize>, is_selected: bool) -> bool {
        let desired_size = vec2(ui.available_width(), 36.0);
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
        let is_hovered = response.hovered();

        let painter = ui.painter();

        if is_selected {
            painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgba_unmultiplied(255, 255, 255, 20));
            // Quiet indicator on left edge
            let pill_rect = Rect::from_min_size(
                Pos2::new(rect.left() + 2.0, rect.center().y - 8.0),
                vec2(3.0, 16.0),
            );
            painter.rect_filled(pill_rect, CornerRadius::same(1), Color32::from_rgb(96, 205, 255));
        } else if is_hovered {
            painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgba_unmultiplied(255, 255, 255, 10));
        }

        let text_color = if is_selected {
            COLOR_TEXT_PRIMARY
        } else if is_hovered {
            Color32::from_rgb(220, 220, 224)
        } else {
            COLOR_TEXT_SECONDARY
        };

        let text_pos = Pos2::new(rect.left() + 14.0, rect.center().y);
        painter.text(
            text_pos,
            Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(13.5),
            text_color,
        );

        if let Some(count) = badge_count {
            let badge_pos = Pos2::new(rect.right() - 14.0, rect.center().y);
            painter.text(
                badge_pos,
                Align2::RIGHT_CENTER,
                count.to_string(),
                egui::FontId::proportional(12.0),
                COLOR_TEXT_MUTED,
            );
        }

        response.clicked()
    }

    fn render_sidebar(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);

        // Sidebar Header: Clean wordmark without decorative icons
        ui.vertical(|ui| {
            ui.label(RichText::new("Steam Helper").size(15.0).strong().color(COLOR_TEXT_PRIMARY));
            ui.label(RichText::new(format!("{} игр · {} аккаунта", self.games.len(), self.accounts.len()))
                .size(11.5)
                .color(COLOR_TEXT_MUTED));
        });

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(10.0);

        // Navigation Items
        if Self::render_nav_button(ui, "Библиотека", Some(self.games.len()), self.active_tab == ActiveTab::Games) {
            self.active_tab = ActiveTab::Games;
        }
        ui.add_space(2.0);

        if Self::render_nav_button(ui, "Аккаунты", Some(self.accounts.len()), self.active_tab == ActiveTab::Accounts) {
            self.active_tab = ActiveTab::Accounts;
        }
        ui.add_space(2.0);

        if Self::render_nav_button(ui, "Параметры", None, self.active_tab == ActiveTab::Settings) {
            self.active_tab = ActiveTab::Settings;
        }

        // Bottom section: Quiet status widget
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(8.0);

            Frame::NONE
                .fill(COLOR_CARD)
                .stroke(Stroke::new(1.0, COLOR_HAIRLINE))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());

                    if self.active_info.is_running {
                        let active_acc = self.get_active_account();
                        let display_name = active_acc
                            .map(|a| a.persona_name.as_str())
                            .or_else(|| self.active_info.active_account_name.as_deref())
                            .unwrap_or("Steam");

                        let is_offline = active_acc.map(|a| a.wants_offline_mode).unwrap_or(false);

                        ui.horizontal(|ui| {
                            let (dot_color, status_text) = if is_offline {
                                (COLOR_OFFLINE, "Автономный")
                            } else {
                                (COLOR_ONLINE, "В сети")
                            };

                            ui.label(RichText::new("●").size(9.0).color(dot_color));
                            ui.label(RichText::new(display_name).size(12.5).strong().color(COLOR_TEXT_PRIMARY));
                            ui.label(RichText::new(status_text).size(11.0).color(dot_color));
                        });

                        ui.add_space(6.0);
                        let close_btn = egui::Button::new(RichText::new("Закрыть Steam").size(11.5).color(COLOR_TEXT_SECONDARY))
                            .fill(Color32::from_rgb(44, 44, 48))
                            .stroke(Stroke::new(1.0, COLOR_HAIRLINE))
                            .corner_radius(CornerRadius::same(5));

                        if ui.add_sized([ui.available_width(), 26.0], close_btn).clicked() {
                            if let Some(ref path) = self.steam_path {
                                let p = path.clone();
                                let tx = self.tx.clone();
                                thread::spawn(move || {
                                    shutdown_steam_cleanly(&p);
                                    tx.send(BackgroundMessage::StatusUpdate("Steam закрыт".to_string(), true)).ok();
                                    tx.send(BackgroundMessage::RefreshNeeded).ok();
                                });
                                self.set_toast("Завершение Steam...".to_string(), true);
                            }
                        }
                    } else {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").size(9.0).color(COLOR_TEXT_MUTED));
                            ui.label(RichText::new("Steam не запущен").size(12.0).color(COLOR_TEXT_MUTED));
                        });

                        ui.add_space(6.0);
                        let start_btn = egui::Button::new(RichText::new("Запустить").size(11.5).color(COLOR_TEXT_PRIMARY))
                            .fill(Color32::from_rgb(44, 44, 48))
                            .stroke(Stroke::new(1.0, COLOR_HAIRLINE))
                            .corner_radius(CornerRadius::same(5));

                        if ui.add_sized([ui.available_width(), 26.0], start_btn).clicked() {
                            if let Some(ref path) = self.steam_path {
                                let _ = launch_game(path, None);
                                self.set_toast("Запуск Steam...".to_string(), true);
                            }
                        }
                    }
                });
        });
    }

    fn render_games_tab(&mut self, ui: &mut Ui) {
        // Pre-load textures
        for game in &self.games {
            if !self.image_cache.contains_key(&game.appid) {
                if let Some(ref img_path) = game.header_image_path {
                    if let Ok(img) = image::open(img_path) {
                        let img = img.resize(240, 120, image::imageops::FilterType::Triangle);
                        let rgba = img.to_rgba8();
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        let col_img = ColorImage::from_rgba_unmultiplied(size, &rgba.into_raw());
                        let tex = ui.ctx().load_texture(
                            format!("cover_{}", game.appid),
                            col_img,
                            TextureOptions::LINEAR,
                        );
                        self.image_cache.insert(game.appid, tex);
                    }
                }
            }
        }

        // Top Header Row
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Библиотека").size(20.0).strong().color(COLOR_TEXT_PRIMARY));
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Refresh button
                let refresh_btn = egui::Button::new(RichText::new("Обновить").size(12.5).color(COLOR_TEXT_PRIMARY))
                    .fill(Color32::from_rgb(42, 42, 46))
                    .stroke(Stroke::new(1.0, COLOR_HAIRLINE))
                    .corner_radius(CornerRadius::same(5));
                if ui.add(refresh_btn).clicked() {
                    self.refresh_data();
                    self.set_toast("Библиотека обновлена".to_string(), true);
                }

                ui.add_space(8.0);

                // Account Filter
                let current_filter_name = match &self.selected_account_filter {
                    Some(sid) => self
                        .get_account_by_id(sid)
                        .map(|a| a.persona_name.as_str())
                        .unwrap_or("Выбран"),
                    None => "Все аккаунты",
                };

                ComboBox::from_id_salt("games_acc_filter")
                    .selected_text(current_filter_name)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(self.selected_account_filter.is_none(), "Все аккаунты").clicked() {
                            self.selected_account_filter = None;
                        }
                        for acc in &self.accounts {
                            let label = format!("{} (@{})", acc.persona_name, acc.account_name);
                            let is_selected = self.selected_account_filter.as_deref() == Some(&acc.steam_id64);
                            if ui.selectable_label(is_selected, label).clicked() {
                                self.selected_account_filter = Some(acc.steam_id64.clone());
                            }
                        }
                    });

                ui.add_space(8.0);

                // Search box
                ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .hint_text("Поиск игры...")
                        .desired_width(180.0),
                );

                if !self.search_query.is_empty() {
                    if ui.button("×").clicked() {
                        self.search_query.clear();
                    }
                }
            });
        });

        ui.add_space(12.0);

        // Games List
        let mut launch_target: Option<(SteamAccount, u32, String)> = None;
        let mut account_choice_update: Option<(u32, String)> = None;
        let mut toast_action: Option<(String, bool)> = None;

        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let query_lower = self.search_query.to_lowercase();

            for game in &self.games {
                if !self.search_query.is_empty()
                    && !game.name.to_lowercase().contains(&query_lower)
                    && !game.appid.to_string().contains(&query_lower)
                {
                    continue;
                }

                if let Some(ref filter_sid) = self.selected_account_filter {
                    if !game.accounts.contains(filter_sid) {
                        continue;
                    }
                }

                let current_chosen_sid = self
                    .chosen_game_accounts
                    .get(&game.appid)
                    .cloned()
                    .or_else(|| game.accounts.first().cloned());

                let card_frame = Frame::NONE
                    .fill(COLOR_CARD)
                    .stroke(Stroke::new(1.0, COLOR_HAIRLINE))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::same(10));

                card_frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // 1. Strict thumbnail rect (120 x 58 px)
                        let thumb_size = vec2(120.0, 58.0);
                        if let Some(tex) = self.image_cache.get(&game.appid) {
                            ui.add(egui::Image::new(tex).fit_to_exact_size(thumb_size).corner_radius(CornerRadius::same(4)));
                        } else {
                            let (rect, _) = ui.allocate_exact_size(thumb_size, Sense::hover());
                            let painter = ui.painter();
                            painter.rect_filled(rect, CornerRadius::same(4), Color32::from_rgb(44, 44, 48));
                            // Draw game monogram initials (first letter)
                            let initial = game.name.chars().next().map(|c| c.to_string()).unwrap_or_else(|| "G".to_string());
                            painter.text(
                                rect.center(),
                                Align2::CENTER_CENTER,
                                initial,
                                egui::FontId::proportional(16.0),
                                COLOR_TEXT_MUTED,
                            );
                        }

                        ui.add_space(12.0);

                        // 2. Info Column
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&game.name).size(14.5).strong().color(COLOR_TEXT_PRIMARY));

                            ui.horizontal(|ui| {
                                let size_gb = game.size_on_disk as f64 / (1024.0 * 1024.0 * 1024.0);
                                let size_str = if size_gb >= 1.0 {
                                    format!("{:.1} ГБ", size_gb)
                                } else {
                                    format!("{:.0} МБ", game.size_on_disk as f64 / (1024.0 * 1024.0))
                                };
                                ui.label(RichText::new(format!("{} · AppID {}", size_str, game.appid)).size(11.5).color(COLOR_TEXT_MUTED));
                            });

                            ui.add_space(2.0);

                            // Account selector row
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Аккаунт:").size(12.0).color(COLOR_TEXT_SECONDARY));

                                if game.accounts.len() > 1 {
                                    let sel_sid = current_chosen_sid.as_deref().unwrap_or("");
                                    let sel_name = self
                                        .get_account_by_id(sel_sid)
                                        .map(|a| format!("{} (@{})", a.persona_name, a.account_name))
                                        .unwrap_or_else(|| "Выбрать".to_string());

                                    ComboBox::from_id_salt(format!("g_acc_{}", game.appid))
                                        .selected_text(sel_name)
                                        .show_ui(ui, |ui| {
                                            for sid in &game.accounts {
                                                if let Some(acc) = self.get_account_by_id(sid) {
                                                    let is_sel = current_chosen_sid.as_deref() == Some(sid);
                                                    let mode_tag = if acc.wants_offline_mode { " [Автономный]" } else { " [Онлайн]" };
                                                    let item_label = format!("{} (@{}){}", acc.persona_name, acc.account_name, mode_tag);
                                                    if ui.selectable_label(is_sel, item_label).clicked() {
                                                        account_choice_update = Some((game.appid, sid.clone()));
                                                    }
                                                }
                                            }
                                        });
                                } else if let Some(sid) = game.accounts.first() {
                                    if let Some(acc) = self.get_account_by_id(sid) {
                                        ui.label(RichText::new(format!("{} (@{})", acc.persona_name, acc.account_name)).size(12.0).color(COLOR_TEXT_PRIMARY));
                                    }
                                }

                                // Status dot (No colored pill bubbles)
                                if let Some(ref sel_sid) = current_chosen_sid {
                                    if let Some(acc) = self.get_account_by_id(sel_sid) {
                                        let (dot_color, status_text) = if acc.wants_offline_mode {
                                            (COLOR_OFFLINE, "Автономный")
                                        } else {
                                            (COLOR_ONLINE, "В сети")
                                        };

                                        ui.add_space(4.0);
                                        ui.label(RichText::new("●").size(8.0).color(dot_color));
                                        ui.label(RichText::new(status_text).size(11.5).color(COLOR_TEXT_MUTED));
                                    }
                                }
                            });
                        });

                        // 3. Play Button (Crisp deliberate action)
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let is_busy = self.is_busy;
                            let play_btn = egui::Button::new(RichText::new("Играть").size(13.0).strong().color(Color32::WHITE))
                                .fill(COLOR_ACCENT)
                                .corner_radius(CornerRadius::same(5))
                                .min_size(vec2(84.0, 30.0));

                            if ui.add_enabled(!is_busy, play_btn).clicked() {
                                let chosen_sid = current_chosen_sid
                                    .as_deref()
                                    .or_else(|| game.accounts.first().map(|s| s.as_str()));

                                let target_acc = chosen_sid
                                    .and_then(|sid| self.get_account_by_id(sid))
                                    .cloned()
                                    .or_else(|| self.accounts.first().cloned());

                                if let Some(acc) = target_acc {
                                    launch_target = Some((acc, game.appid, game.name.clone()));
                                } else {
                                    toast_action = Some(("Аккаунт не выбран".to_string(), false));
                                }
                            }
                        });
                    });
                });

                ui.add_space(4.0);
            }
        });

        if let Some((appid, sid)) = account_choice_update {
            self.chosen_game_accounts.insert(appid, sid);
        }

        if let Some((msg, ok)) = toast_action {
            self.set_toast(msg, ok);
        }

        if let Some((target_acc, appid, name)) = launch_target {
            self.switch_to_account_and_launch(target_acc, Some(appid), Some(name));
        }
    }

    fn render_accounts_tab(&mut self, ui: &mut Ui) {
        ui.vertical(|ui| {
            ui.label(RichText::new("Аккаунты").size(20.0).strong().color(COLOR_TEXT_PRIMARY));
            ui.label(RichText::new("Управление профилями и автономным режимом").size(12.5).color(COLOR_TEXT_MUTED));
        });

        ui.add_space(14.0);

        let mut toggle_offline_id: Option<String> = None;
        let mut switch_acc: Option<SteamAccount> = None;

        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for acc in &self.accounts {
                let is_current = acc.is_active;

                let card_stroke = if is_current {
                    Stroke::new(1.0, Color32::from_rgb(0, 120, 212))
                } else {
                    Stroke::new(1.0, COLOR_HAIRLINE)
                };

                let card_frame = Frame::NONE
                    .fill(COLOR_CARD)
                    .stroke(card_stroke)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::same(12));

                card_frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Monogram avatar circle
                        let (avatar_rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
                        let painter = ui.painter();
                        painter.rect_filled(
                            avatar_rect,
                            CornerRadius::same(18),
                            if is_current { Color32::from_rgb(0, 90, 160) } else { Color32::from_rgb(46, 46, 50) },
                        );
                        let monogram = acc.persona_name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "U".to_string());
                        painter.text(
                            avatar_rect.center(),
                            Align2::CENTER_CENTER,
                            monogram,
                            egui::FontId::proportional(15.0),
                            COLOR_TEXT_PRIMARY,
                        );

                        ui.add_space(12.0);

                        // Account Information
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&acc.persona_name).size(14.5).strong().color(COLOR_TEXT_PRIMARY));
                                ui.label(RichText::new(format!("@{}", acc.account_name)).size(12.5).color(COLOR_TEXT_MUTED));

                                if is_current {
                                    ui.label(RichText::new("● активен").size(11.0).color(COLOR_ONLINE));
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("SteamID: {}", acc.steam_id64)).size(11.5).color(COLOR_TEXT_MUTED));
                                if acc.owned_apps_count > 0 {
                                    ui.label(RichText::new(format!("· {} игр", acc.owned_apps_count)).size(11.5).color(COLOR_TEXT_MUTED));
                                }
                            });
                        });

                        // Right side controls: Quiet toggle & switch button
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if !is_current {
                                let btn = egui::Button::new(RichText::new("Войти").size(12.0).color(COLOR_TEXT_PRIMARY))
                                    .fill(Color32::from_rgb(44, 44, 48))
                                    .stroke(Stroke::new(1.0, COLOR_HAIRLINE))
                                    .corner_radius(CornerRadius::same(5))
                                    .min_size(vec2(90.0, 28.0));
                                if ui.add_enabled(!self.is_busy, btn).clicked() {
                                    switch_acc = Some(acc.clone());
                                }
                            } else {
                                ui.label(RichText::new("Текущий").size(12.0).color(COLOR_TEXT_MUTED));
                            }

                            ui.add_space(12.0);

                            // Offline Toggle Button
                            let (status_dot, mode_title, btn_title) = if acc.wants_offline_mode {
                                (COLOR_OFFLINE, "Автономный", "В онлайн")
                            } else {
                                (COLOR_ONLINE, "В сети", "В автономный")
                            };

                            let toggle_btn = egui::Button::new(RichText::new(btn_title).size(11.5).color(COLOR_TEXT_PRIMARY))
                                .fill(Color32::from_rgb(44, 44, 48))
                                .stroke(Stroke::new(1.0, COLOR_HAIRLINE))
                                .corner_radius(CornerRadius::same(5))
                                .min_size(vec2(105.0, 28.0));

                            if ui.add(toggle_btn).clicked() {
                                toggle_offline_id = Some(acc.steam_id64.clone());
                            }

                            ui.horizontal(|ui| {
                                ui.label(RichText::new("●").size(8.0).color(status_dot));
                                ui.label(RichText::new(mode_title).size(12.0).color(COLOR_TEXT_SECONDARY));
                            });
                        });
                    });
                });

                ui.add_space(5.0);
            }
        });

        if let Some(sid) = toggle_offline_id {
            self.toggle_account_offline(&sid);
        }

        if let Some(acc) = switch_acc {
            self.switch_to_account_and_launch(acc, None, None);
        }
    }

    fn render_settings_tab(&mut self, ui: &mut Ui) {
        ui.vertical(|ui| {
            ui.label(RichText::new("Параметры").size(20.0).strong().color(COLOR_TEXT_PRIMARY));
            ui.label(RichText::new("Сведения об установке Steam").size(12.5).color(COLOR_TEXT_MUTED));
        });

        ui.add_space(14.0);

        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let card_frame = Frame::NONE
                .fill(COLOR_CARD)
                .stroke(Stroke::new(1.0, COLOR_HAIRLINE))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::same(16));

            card_frame.show(ui, |ui| {
                ui.label(RichText::new("Интеграция с системой").size(14.5).strong().color(COLOR_TEXT_PRIMARY));
                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    ui.label(RichText::new("Каталог Steam:").color(COLOR_TEXT_MUTED));
                    if let Some(ref p) = self.steam_path {
                        ui.label(RichText::new(p.to_string_lossy()).color(COLOR_TEXT_PRIMARY));
                    } else {
                        ui.label(RichText::new("Не найден").color(Color32::from_rgb(220, 80, 80)));
                    }
                });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Процесс Steam:").color(COLOR_TEXT_MUTED));
                    if self.active_info.is_running {
                        ui.label(RichText::new(format!("Запущен (PID {})", self.active_info.pid)).color(COLOR_ONLINE));
                    } else {
                        ui.label(RichText::new("Не запущен").color(COLOR_TEXT_MUTED));
                    }
                });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Активный пользователь:").color(COLOR_TEXT_MUTED));
                    ui.label(RichText::new(format!("ID {}", self.active_info.active_user_id32)).color(COLOR_TEXT_PRIMARY));
                });

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(14.0);

                ui.label(RichText::new("Принцип работы").size(13.5).strong().color(COLOR_TEXT_PRIMARY));
                ui.add_space(6.0);
                ui.label(RichText::new("• Библиотека считывается из appmanifest_*.acf во всех обнаруженных разделах Steam.").size(12.0).color(COLOR_TEXT_SECONDARY));
                ui.label(RichText::new("• Привязка игр к аккаунтам происходит по данным userdata, LastOwner и истории запусков.").size(12.0).color(COLOR_TEXT_SECONDARY));
                ui.label(RichText::new("• Переключение аккаунта перед запуском обновляет AutoLoginUser в реестре Windows и параметры входа в loginusers.vdf.").size(12.0).color(COLOR_TEXT_SECONDARY));
                ui.label(RichText::new("• Автономный режим переключает флаги WantsOfflineMode и SkipOfflineModeWarning индивидуально для каждого профиля.").size(12.0).color(COLOR_TEXT_SECONDARY));
            });
        });
    }

    fn render_toast_notification(&mut self, ui: &mut Ui) {
        if let Some((ref msg, is_success, time)) = self.status_toast {
            if time.elapsed() > Duration::from_secs(3) {
                self.status_toast = None;
                return;
            }

            let border_color = if is_success { COLOR_ONLINE } else { Color32::from_rgb(220, 80, 80) };

            let rect = ui.max_rect();
            let toast_pos = rect.left_bottom() + vec2(235.0, -36.0);

            egui::Area::new(egui::Id::unique("status_toast_area"))
                .fixed_pos(toast_pos)
                .anchor(Align2::LEFT_BOTTOM, vec2(0.0, 0.0))
                .show(ui.ctx(), |ui| {
                    Frame::NONE
                        .fill(COLOR_CARD)
                        .stroke(Stroke::new(1.0, border_color))
                        .corner_radius(CornerRadius::same(5))
                        .inner_margin(Margin::symmetric(14, 8))
                        .show(ui, |ui| {
                            ui.label(RichText::new(msg).size(12.5).color(COLOR_TEXT_PRIMARY));
                        });
                });
        }
    }

    fn check_background_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                BackgroundMessage::StatusUpdate(text, is_success) => {
                    self.set_toast(text.clone(), is_success);
                    self.busy_message = text;
                }
                BackgroundMessage::RefreshNeeded => {
                    self.is_busy = false;
                    self.refresh_data();
                }
            }
        }
    }
}

impl eframe::App for SteamHelperApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.check_background_messages();

        // Graphite Neutral Theme (Clean craft, no slop)
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = COLOR_SIDEBAR;
        visuals.panel_fill = COLOR_BACKGROUND;
        visuals.widgets.noninteractive.bg_fill = COLOR_CARD;
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(44, 44, 48);
        visuals.widgets.hovered.bg_fill = COLOR_CARD_HOVER;
        visuals.widgets.active.bg_fill = COLOR_ACCENT;
        visuals.selection.bg_fill = COLOR_ACCENT;
        visuals.widgets.inactive.corner_radius = CornerRadius::same(5);
        visuals.widgets.hovered.corner_radius = CornerRadius::same(5);
        visuals.widgets.active.corner_radius = CornerRadius::same(5);
        ui.ctx().set_visuals(visuals);

        // Sidebar
        Panel::left("steam_sidebar")
            .exact_size(210.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                Frame::NONE
                    .fill(COLOR_SIDEBAR)
                    .stroke(Stroke::new(1.0, COLOR_HAIRLINE))
                    .inner_margin(Margin::same(12)),
            )
            .show(ui, |ui| {
                self.render_sidebar(ui);
            });

        // Content Area
        CentralPanel::default()
            .frame(
                Frame::NONE
                    .fill(COLOR_BACKGROUND)
                    .inner_margin(Margin::symmetric(24, 20)),
            )
            .show(ui, |ui| match self.active_tab {
                ActiveTab::Games => self.render_games_tab(ui),
                ActiveTab::Accounts => self.render_accounts_tab(ui),
                ActiveTab::Settings => self.render_settings_tab(ui),
            });

        self.render_toast_notification(ui);

        ui.ctx().request_repaint_after(Duration::from_millis(500));
    }
}
