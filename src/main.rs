#![windows_subsystem = "windows"]

mod app;
mod steam;
mod vdf;

use app::SteamHelperApp;
use eframe::egui::{self, vec2, ViewportBuilder};
use std::sync::Arc;

fn main() -> eframe::Result<()> {
    // Check if CLI arguments were passed for quick launch (e.g. steamgamehelper.exe 1245620)
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let target = if args[1] == "--launch" || args[1] == "-l" {
            args.get(2)
        } else if !args[1].starts_with('-') {
            Some(&args[1])
        } else {
            None
        };

        if let Some(target_str) = target {
            if let Ok(appid) = target_str.parse::<u32>() {
                if let Ok(path) = steam::find_steam_path() {
                    let active = steam::get_active_steam_info();
                    if let Ok(accounts) = steam::load_accounts(&path, active.active_user_id32) {
                        if let Ok(games) = steam::load_games(&path, &accounts) {
                            if let Some(game) = games.iter().find(|g| g.appid == appid) {
                                if let Some(owner_sid) = game.accounts.first() {
                                    if let Some(acc) = accounts.iter().find(|a| &a.steam_id64 == owner_sid) {
                                        let need_switch = !active.is_running
                                            || active.active_user_id32 != acc.account_id32
                                            || active.active_account_name.as_deref() != Some(&acc.account_name);

                                        if need_switch && active.is_running {
                                            steam::shutdown_steam_cleanly(&path);
                                        }
                                        if need_switch {
                                            let _ = steam::prepare_steam_autologin(&path, &acc.steam_id64, &acc.account_name);
                                        }
                                        let _ = steam::launch_game(&path, Some(appid));
                                        return Ok(());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let native_options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size(vec2(1060.0, 720.0))
            .with_min_inner_size(vec2(820.0, 520.0))
            .with_title("Steam Game Helper - Авто-выбор аккаунта и автономный режим"),
        ..Default::default()
    };

    eframe::run_native(
        "Steam Game Helper",
        native_options,
        Box::new(|cc| {
            // Load Windows Segoe UI font for crisp UI and full Cyrillic support
            let mut fonts = egui::FontDefinitions::default();
            for font_path in &[r"C:\Windows\Fonts\segoeui.ttf", r"C:\Windows\Fonts\arial.ttf"] {
                if let Ok(font_bytes) = std::fs::read(font_path) {
                    fonts.font_data.insert(
                        "system_font".to_owned(),
                        Arc::new(egui::FontData::from_owned(font_bytes)),
                    );
                    fonts
                        .families
                        .entry(egui::FontFamily::Proportional)
                        .or_default()
                        .insert(0, "system_font".to_owned());
                    fonts
                        .families
                        .entry(egui::FontFamily::Monospace)
                        .or_default()
                        .insert(0, "system_font".to_owned());
                    break;
                }
            }
            cc.egui_ctx.set_fonts(fonts);

            Ok(Box::new(SteamHelperApp::new(cc)))
        }),
    )
}
