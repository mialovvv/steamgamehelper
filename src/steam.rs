use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;
use winreg::enums::*;
use winreg::RegKey;

use crate::vdf::{parse_vdf, serialize_vdf, VdfValue};

pub const STEAM_ID64_BASE: u64 = 76561197960265728;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SteamAccount {
    pub steam_id64: String,
    pub account_id32: u32,
    pub account_name: String,
    pub persona_name: String,
    pub remember_password: bool,
    pub wants_offline_mode: bool,
    pub skip_offline_warning: bool,
    pub auto_login: bool,
    pub is_most_recent: bool,
    pub is_active: bool,
    pub timestamp: u64,
    pub owned_apps_count: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SteamGame {
    pub appid: u32,
    pub name: String,
    pub install_dir: String,
    pub library_path: PathBuf,
    pub size_on_disk: u64,
    pub last_owner: Option<String>,
    pub accounts: Vec<String>, // SteamID64s that have this game
    pub selected_account: Option<String>, // SteamID64 of user's choice (or auto-picked)
    pub header_image_path: Option<PathBuf>,
}

pub struct ActiveSteamInfo {
    pub is_running: bool,
    pub pid: u32,
    pub active_user_id32: u32,
    pub active_account_name: Option<String>,
}

/// Discovers Steam installation directory from Windows registry or default paths.
pub fn find_steam_path() -> Result<PathBuf, String> {
    // 1. Try HKCU\Software\Valve\Steam
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(valve_steam) = hkcu.open_subkey("Software\\Valve\\Steam") {
        if let Ok(path_str) = valve_steam.get_value::<String, _>("SteamPath") {
            let p = PathBuf::from(path_str.replace('/', "\\"));
            if p.exists() {
                return Ok(p);
            }
        }
    }

    // 2. Try HKLM 32/64 bit
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    for key in &[
        "SOFTWARE\\WOW6432Node\\Valve\\Steam",
        "SOFTWARE\\Valve\\Steam",
    ] {
        if let Ok(steam_key) = hklm.open_subkey(key) {
            if let Ok(path_str) = steam_key.get_value::<String, _>("InstallPath") {
                let p = PathBuf::from(path_str);
                if p.exists() {
                    return Ok(p);
                }
            }
        }
    }

    // 3. Common fallback paths
    for fallback in &[
        r"C:\Program Files (x86)\Steam",
        r"C:\Program Files\Steam",
        r"D:\Steam",
        r"E:\Steam",
    ] {
        let p = PathBuf::from(fallback);
        if p.exists() {
            return Ok(p);
        }
    }

    Err("Не удалось обнаружить установку Steam. Проверьте путь к Steam.".to_string())
}

/// Checks if Steam is currently running and which user is logged in.
pub fn get_active_steam_info() -> ActiveSteamInfo {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let mut is_running = false;
    let mut pid = 0u32;
    let mut active_user_id32 = 0u32;
    let mut active_account_name = None;

    if let Ok(steam_key) = hkcu.open_subkey("Software\\Valve\\Steam") {
        if let Ok(name) = steam_key.get_value::<String, _>("AutoLoginUser") {
            if !name.is_empty() {
                active_account_name = Some(name);
            }
        }
    }

    if let Ok(proc_key) = hkcu.open_subkey("Software\\Valve\\Steam\\ActiveProcess") {
        if let Ok(p) = proc_key.get_value::<u32, _>("pid") {
            pid = p;
        }
        if let Ok(u) = proc_key.get_value::<u32, _>("ActiveUser") {
            active_user_id32 = u;
        }
    }

    // Verify if pid is truly alive
    if pid > 0 {
        is_running = is_process_alive(pid);
    }

    ActiveSteamInfo {
        is_running,
        pid,
        active_user_id32,
        active_account_name,
    }
}

fn is_process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        type HANDLE = *mut std::ffi::c_void;
        type DWORD = u32;
        type BOOL = i32;

        const PROCESS_QUERY_LIMITED_INFORMATION: DWORD = 0x1000;
        const STILL_ACTIVE: DWORD = 259;

        unsafe extern "system" {
            fn OpenProcess(desired_access: DWORD, inherit_handle: BOOL, process_id: DWORD) -> HANDLE;
            fn GetExitCodeProcess(process: HANDLE, exit_code: *mut DWORD) -> BOOL;
            fn CloseHandle(handle: HANDLE) -> BOOL;
        }

        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut exit_code: DWORD = 0;
            let res = GetExitCodeProcess(handle, &mut exit_code);
            CloseHandle(handle);
            if res != 0 && exit_code == STILL_ACTIVE {
                return true;
            }
        }
    }
    false
}

/// Loads all Steam accounts from config/loginusers.vdf.
pub fn load_accounts(steam_path: &Path, active_user_id32: u32) -> Result<Vec<SteamAccount>, String> {
    let loginusers_path = steam_path.join("config").join("loginusers.vdf");
    if !loginusers_path.exists() {
        return Err(format!("Файл {:?} не найден", loginusers_path));
    }

    let content = fs::read_to_string(&loginusers_path)
        .map_err(|e| format!("Ошибка чтения loginusers.vdf: {}", e))?;
    let parsed = parse_vdf(&content).map_err(|e| format!("Ошибка парсинга loginusers.vdf: {}", e))?;

    let Some(users_val) = parsed.get("users") else {
        return Ok(Vec::new());
    };
    let Some(users_map) = users_val.as_object() else {
        return Ok(Vec::new());
    };

    let mut accounts = Vec::new();

    for (steam_id64, val) in users_map {
        let sid: u64 = steam_id64.parse().unwrap_or(0);
        let account_id32 = if sid > STEAM_ID64_BASE {
            (sid - STEAM_ID64_BASE) as u32
        } else {
            0
        };

        let account_name = val.get_str("AccountName").unwrap_or("").to_string();
        let persona_name = val.get_str("PersonaName").unwrap_or(&account_name).to_string();
        let remember_password = val.get_str("RememberPassword").unwrap_or("0") == "1";
        let wants_offline_mode = val.get_str("WantsOfflineMode").unwrap_or("0") == "1";
        let skip_offline_warning = val.get_str("SkipOfflineModeWarning").unwrap_or("0") == "1";
        let auto_login = val.get_str("AutoLogin").unwrap_or("0") == "1";
        let is_most_recent = val.get_str("MostRecent").unwrap_or("0") == "1";
        let timestamp: u64 = val.get_str("timestamp").and_then(|s| s.parse().ok()).unwrap_or(0);

        let is_active = account_id32 > 0 && account_id32 == active_user_id32;

        // Scan count of games in user's userdata folder
        let mut owned_count = 0;
        let userdata_dir = steam_path.join("userdata").join(account_id32.to_string());
        if userdata_dir.exists() {
            let libcache = userdata_dir.join("config").join("librarycache");
            if libcache.exists() {
                if let Ok(entries) = fs::read_dir(libcache) {
                    owned_count = entries.filter_map(Result::ok).count();
                }
            }
            if owned_count == 0 {
                if let Ok(entries) = fs::read_dir(&userdata_dir) {
                    owned_count = entries
                        .filter_map(Result::ok)
                        .filter(|e| {
                            e.file_name()
                                .to_str()
                                .map(|s| s.chars().all(|c| c.is_ascii_digit()) && s != "7" && s != "0")
                                .unwrap_or(false)
                        })
                        .count();
                }
            }
        }

        accounts.push(SteamAccount {
            steam_id64: steam_id64.clone(),
            account_id32,
            account_name,
            persona_name,
            remember_password,
            wants_offline_mode,
            skip_offline_warning,
            auto_login,
            is_most_recent,
            is_active,
            timestamp,
            owned_apps_count: owned_count,
        });
    }

    // Sort accounts: active first, then most recent, then by timestamp desc
    accounts.sort_by(|a, b| {
        b.is_active
            .cmp(&a.is_active)
            .then_with(|| b.is_most_recent.cmp(&a.is_most_recent))
            .then_with(|| b.timestamp.cmp(&a.timestamp))
    });

    Ok(accounts)
}

/// Sets Offline Mode for a specific account in loginusers.vdf.
pub fn set_account_offline_mode(
    steam_path: &Path,
    target_steam_id64: &str,
    enable_offline: bool,
) -> Result<(), String> {
    let loginusers_path = steam_path.join("config").join("loginusers.vdf");
    if !loginusers_path.exists() {
        return Err(format!("Файл {:?} не найден", loginusers_path));
    }

    let content = fs::read_to_string(&loginusers_path)
        .map_err(|e| format!("Ошибка чтения loginusers.vdf: {}", e))?;
    let mut parsed = parse_vdf(&content).map_err(|e| format!("Ошибка парсинга loginusers.vdf: {}", e))?;

    let Some(users_val) = parsed.get_mut("users") else {
        return Err("Секция 'users' не найдена в loginusers.vdf".to_string());
    };
    let Some(users_map) = users_val.as_object_mut() else {
        return Err("Секция 'users' имеет некорректный формат".to_string());
    };

    let Some(target_val) = users_map.get_mut(target_steam_id64) else {
        return Err(format!("Аккаунт с SteamID64 {} не найден в loginusers.vdf", target_steam_id64));
    };
    let Some(target_obj) = target_val.as_object_mut() else {
        return Err("Блок пользователя имеет некорректный формат".to_string());
    };

    let offline_val = if enable_offline { "1" } else { "0" };
    target_obj.insert("WantsOfflineMode".to_string(), VdfValue::String(offline_val.to_string()));
    target_obj.insert("SkipOfflineModeWarning".to_string(), VdfValue::String(offline_val.to_string()));

    // Make a backup copy before saving
    let bak_path = steam_path.join("config").join("loginusers.vdf.bak");
    let _ = fs::copy(&loginusers_path, &bak_path);

    let serialized = serialize_vdf(&parsed, 0);
    fs::write(&loginusers_path, serialized)
        .map_err(|e| format!("Ошибка записи loginusers.vdf: {}", e))?;

    Ok(())
}

/// Prepares Steam for logging into `target_account`:
/// Updates HKCU\Software\Valve\Steam\AutoLoginUser and marks target account as active in loginusers.vdf.
pub fn prepare_steam_autologin(
    steam_path: &Path,
    target_steam_id64: &str,
    target_account_name: &str,
) -> Result<(), String> {
    // 1. Update Windows Registry
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (valve_steam, _) = hkcu
        .create_subkey("Software\\Valve\\Steam")
        .map_err(|e| format!("Ошибка доступа к реестру Steam: {}", e))?;
    valve_steam
        .set_value("AutoLoginUser", &target_account_name)
        .map_err(|e| format!("Ошибка установки AutoLoginUser в реестре: {}", e))?;
    let _ = valve_steam.set_value("RememberPassword", &1u32);

    // 2. Update loginusers.vdf
    let loginusers_path = steam_path.join("config").join("loginusers.vdf");
    if loginusers_path.exists() {
        if let Ok(content) = fs::read_to_string(&loginusers_path) {
            if let Ok(mut parsed) = parse_vdf(&content) {
                if let Some(users_val) = parsed.get_mut("users") {
                    if let Some(users_map) = users_val.as_object_mut() {
                        for (sid, uval) in users_map.iter_mut() {
                            if let Some(uobj) = uval.as_object_mut() {
                                if sid == target_steam_id64 {
                                    uobj.insert("AutoLogin".to_string(), VdfValue::String("1".to_string()));
                                    uobj.insert("MostRecent".to_string(), VdfValue::String("1".to_string()));
                                } else {
                                    uobj.insert("AutoLogin".to_string(), VdfValue::String("0".to_string()));
                                    uobj.insert("MostRecent".to_string(), VdfValue::String("0".to_string()));
                                }
                            }
                        }
                        let _ = fs::copy(&loginusers_path, steam_path.join("config").join("loginusers.vdf.bak"));
                        let serialized = serialize_vdf(&parsed, 0);
                        let _ = fs::write(&loginusers_path, serialized);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Shuts down Steam cleanly with a grace period, killing it if unresponsive.
pub fn shutdown_steam_cleanly(steam_path: &Path) {
    let steam_exe = steam_path.join("steam.exe");
    if steam_exe.exists() {
        let mut cmd = Command::new(&steam_exe);
        cmd.arg("-shutdown");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let _ = cmd.spawn();
    }

    // Wait up to 3 seconds for Steam to close
    for _ in 0..15 {
        thread::sleep(Duration::from_millis(200));
        let active = get_active_steam_info();
        if !active.is_running {
            return;
        }
    }

    // If still running, kill it silently without console window
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("taskkill");
        cmd.args(&["/F", "/IM", "steam.exe"]);
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        let _ = cmd.output();
        thread::sleep(Duration::from_millis(500));
    }
}

/// Launches Steam or a specific game through Steam.
pub fn launch_game(steam_path: &Path, appid: Option<u32>) -> Result<(), String> {
    let steam_exe = steam_path.join("steam.exe");
    if !steam_exe.exists() {
        return Err(format!("Исполняемый файл {:?} не найден", steam_exe));
    }

    let mut cmd = Command::new(steam_exe);
    if let Some(id) = appid {
        cmd.arg("-applaunch").arg(id.to_string());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd.spawn()
        .map_err(|e| format!("Не удалось запустить Steam: {}", e))?;

    Ok(())
}

/// Discovers all library folders from config/libraryfolders.vdf and scans installed games.
pub fn load_games(steam_path: &Path, accounts: &[SteamAccount]) -> Result<Vec<SteamGame>, String> {
    let mut library_dirs = Vec::new();
    library_dirs.push(steam_path.to_path_buf());

    let libfolders_path = steam_path.join("config").join("libraryfolders.vdf");
    if libfolders_path.exists() {
        if let Ok(content) = fs::read_to_string(&libfolders_path) {
            if let Ok(parsed) = parse_vdf(&content) {
                if let Some(folders_val) = parsed.get("libraryfolders") {
                    if let Some(folders_map) = folders_val.as_object() {
                        for (_, folder_val) in folders_map {
                            if let Some(p) = folder_val.get_str("path") {
                                let path = PathBuf::from(p);
                                if path.exists() && !library_dirs.contains(&path) {
                                    library_dirs.push(path);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Pre-build account game sets from userdata
    let mut account_games_map: BTreeMap<String, HashSet<u32>> = BTreeMap::new();
    for acc in accounts {
        let mut apps = HashSet::new();
        let udir = steam_path.join("userdata").join(acc.account_id32.to_string());

        // 1. check librarycache
        let libcache = udir.join("config").join("librarycache");
        if let Ok(entries) = fs::read_dir(libcache) {
            for entry in entries.filter_map(Result::ok) {
                let name = entry.file_name();
                let s = name.to_string_lossy();
                if s.ends_with(".json") {
                    if let Ok(id) = s.trim_end_matches(".json").parse::<u32>() {
                        apps.insert(id);
                    }
                }
            }
        }

        // 2. check user app subdirectories
        if let Ok(entries) = fs::read_dir(&udir) {
            for entry in entries.filter_map(Result::ok) {
                let name = entry.file_name();
                let s = name.to_string_lossy();
                if let Ok(id) = s.parse::<u32>() {
                    if id != 7 && id != 0 {
                        apps.insert(id);
                    }
                }
            }
        }

        // 3. check localconfig.vdf
        let localconfig_path = udir.join("config").join("localconfig.vdf");
        if let Ok(local_text) = fs::read_to_string(localconfig_path) {
            for word in local_text.split(|c: char| !c.is_ascii_digit()) {
                if word.len() >= 3 && word.len() <= 8 {
                    if let Ok(id) = word.parse::<u32>() {
                        if id > 10 {
                            apps.insert(id);
                        }
                    }
                }
            }
        }

        account_games_map.insert(acc.steam_id64.clone(), apps);
    }

    let mut games = Vec::new();
    let mut seen_appids = HashSet::new();

    for lib_dir in library_dirs {
        let steamapps = lib_dir.join("steamapps");
        if !steamapps.exists() {
            continue;
        }

        let entries = match fs::read_dir(&steamapps) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.filter_map(Result::ok) {
            let fname = entry.file_name();
            let fname_str = fname.to_string_lossy();
            if fname_str.starts_with("appmanifest_") && fname_str.ends_with(".acf") {
                if let Ok(content) = fs::read_to_string(entry.path()) {
                    if let Ok(parsed) = parse_vdf(&content) {
                        let state = parsed.get("AppState").or_else(|| parsed.get("appstate"));
                        let Some(state) = state else { continue };

                        let appid: u32 = state
                            .get_str("appid")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0);
                        if appid == 0 || seen_appids.contains(&appid) {
                            continue;
                        }

                        let name = state
                            .get_str("name")
                            .unwrap_or(&format!("Steam App {}", appid))
                            .to_string();

                        // Filter out Steam internal tools/redistributables if needed
                        if appid == 228980 {
                            // "Steamworks Common Redistributables" - skip
                            continue;
                        }

                        let installdir = state.get_str("installdir").unwrap_or("").to_string();
                        let size_on_disk: u64 = state
                            .get_str("SizeOnDisk")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0);
                        let last_owner = state.get_str("LastOwner").map(|s| s.to_string());

                        // Look up cached cover image in appcache/librarycache
                        let mut header_path = None;
                        let cache_dir = steam_path.join("appcache").join("librarycache").join(appid.to_string());
                        if cache_dir.exists() {
                            // 1. Try preferred standard file names
                            for img_name in &["header.jpg", "library_header.jpg", "library_600x900.jpg", "library_hero.jpg"] {
                                let p = cache_dir.join(img_name);
                                if p.exists() {
                                    header_path = Some(p);
                                    break;
                                }
                            }
                            // 2. If not found, pick any valid .jpg (excluding blur/logo)
                            if header_path.is_none() {
                                if let Ok(entries) = fs::read_dir(&cache_dir) {
                                    for entry in entries.filter_map(Result::ok) {
                                        let fname = entry.file_name();
                                        let s = fname.to_string_lossy().to_lowercase();
                                        if s.ends_with(".jpg") && !s.contains("blur") && !s.contains("logo") {
                                            header_path = Some(entry.path());
                                            break;
                                        }
                                    }
                                }
                            }
                        }

                        // Determine which accounts have this game
                        let mut game_accounts = Vec::new();
                        for acc in accounts {
                            let mut has_game = false;
                            if let Some(ref lo) = last_owner {
                                if lo == &acc.steam_id64 {
                                    has_game = true;
                                }
                            }
                            if !has_game {
                                if let Some(set) = account_games_map.get(&acc.steam_id64) {
                                    if set.contains(&appid) {
                                        has_game = true;
                                    }
                                }
                            }
                            if has_game {
                                game_accounts.push(acc.steam_id64.clone());
                            }
                        }

                        // If no specific account was detected, fallback to last_owner if it exists in accounts
                        if game_accounts.is_empty() {
                            if let Some(ref lo) = last_owner {
                                if accounts.iter().any(|a| &a.steam_id64 == lo) {
                                    game_accounts.push(lo.clone());
                                }
                            }
                        }

                        // If still empty and there are accounts, assign to primary account so user can still launch
                        if game_accounts.is_empty() && !accounts.is_empty() {
                            game_accounts.push(accounts[0].steam_id64.clone());
                        }

                        let default_pick = game_accounts.first().cloned();

                        seen_appids.insert(appid);
                        games.push(SteamGame {
                            appid,
                            name,
                            install_dir: installdir,
                            library_path: lib_dir.clone(),
                            size_on_disk,
                            last_owner,
                            accounts: game_accounts,
                            selected_account: default_pick,
                            header_image_path: header_path,
                        });
                    }
                }
            }
        }
    }

    // Sort games alphabetically by name
    games.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    Ok(games)
}
