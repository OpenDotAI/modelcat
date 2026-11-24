use crate::vm;
use anyhow::Result;
use tauri::menu::{Menu, MenuItem, MenuItemKind, Submenu};
use tauri::window::Color;
use tauri::{Manager, Theme, WebviewUrl, WebviewWindowBuilder};

#[cfg(target_os = "macos")]
fn install_quit_shortcut(app: &tauri::AppHandle) {
    static APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();
    extern "C" fn quit() {
        if let Some(app) = APP.get() {
            vm::stop_workspace(app, true);
        }
    }
    extern "C" {
        fn modelcat_install_quit_shortcut(quit: extern "C" fn());
    }
    if APP.set(app.clone()).is_ok() {
        // Tauri calls setup on the AppKit main thread.
        unsafe { modelcat_install_quit_shortcut(quit) };
    }
}

#[derive(Clone, Copy, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    fn theme(self) -> Option<Theme> {
        match self {
            Self::System => None,
            Self::Light => Some(Theme::Light),
            Self::Dark => Some(Theme::Dark),
        }
    }
}

fn saved_appearance(app: &tauri::AppHandle) -> Result<Appearance> {
    let file = app.path().app_config_dir()?.join("appearance.json");
    match std::fs::read(file) {
        Ok(data) => Ok(serde_json::from_slice(&data)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Appearance::System),
        Err(error) => Err(error.into()),
    }
}

#[tauri::command]
fn desktop_appearance(window: tauri::WebviewWindow, mode: Option<Appearance>) -> Result<Appearance, String> {
    if window.label() != "main" {
        return Err("请在 ModelCat 设置中更改外观".into());
    }
    let app = window.app_handle();
    let result = (|| -> Result<Appearance> {
        let Some(mode) = mode else { return saved_appearance(app); };
        let dir = app.path().app_config_dir()?;
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!("appearance-{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, serde_json::to_vec(&mode)?)?;
        if let Err(error) = std::fs::rename(&tmp, dir.join("appearance.json")) {
            let _ = std::fs::remove_file(tmp);
            return Err(error.into());
        }
        window.set_theme(mode.theme())?;
        window.set_background_color(Some(canvas_color(window.theme()?)))?;
        Ok(mode)
    })();
    result.map_err(|error| format!("无法访问主题偏好：{error}"))
}

fn canvas_color(theme: Theme) -> Color {
    if theme == Theme::Dark {
        Color(25, 35, 46, 255)
    } else {
        Color(245, 246, 247, 255)
    }
}

fn open_login(url: &tauri::Url) {
    if cfg!(debug_assertions)
        && std::env::var_os("MODELCAT_OAUTH_PREVIEW").is_some()
        && url.scheme() == "http"
        && url.host_str() == Some("claude-oauth.fixture.invalid")
        && url.path() == "/authorize"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
    {
        let Some(root) = std::env::var_os("MODELCAT_VM_DEV_DATA") else {
            return;
        };
        let profile = std::path::PathBuf::from(root).join("oauth-preview-chrome");
        let mut profile_arg = std::ffi::OsString::from("--user-data-dir=");
        profile_arg.push(profile);
        let result = std::process::Command::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
            .arg(profile_arg)
            .args([
                "--proxy-server=http://127.0.0.1:18779",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
                "--disable-quic",
                url.as_str(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Err(error) = result {
            eprintln!("Could not open the Chrome preview: {error}");
        }
    } else if url.scheme() == "https" {
        let _ = std::process::Command::new("open").arg(url.as_str()).spawn();
    }
}
pub fn run() -> Result<()> {
    let state = std::sync::Arc::new(vm::VmState::default());
    let result = tauri::Builder::default()
        .manage(state)
        .manage(crate::codex_login::CodexLogin::default())
        .manage(crate::codex_login::AuthorizationWindows::default())
        .invoke_handler(tauri::generate_handler![
            desktop_appearance,
            vm::vm_default_directory,
            vm::vm_workspace_directory,
            vm::vm_choose_directory,
            vm::vm_platform_gateway,
            vm::vm_platform_verified,
            vm::vm_verify_platform,
            vm::vm_logout_platform,
            vm::vm_attach_workspace,
            vm::vm_start,
            vm::vm_open,
            vm::vm_stop,
            vm::vm_tool,
            vm::vm_resources,
            vm::vm_usage,
            vm::vm_stop_on_exit,
            crate::codex_login::workspace_authorization,
            crate::codex_login::close_workspace_authorization,
            crate::codex_login::codex_profiles,
            crate::codex_login::codex_account
        ])
        .menu(|app| {
            let menu = Menu::default(app)?;
            let settings = MenuItem::with_id(app, "desktop-settings", "设置…", true, Some("CmdOrCtrl+,"))?;
            let stop = MenuItem::with_id(app, "stop-workspace", "停止工作区", true, None::<&str>)?;
            let tools = MenuItem::with_id(app, "workspace-tools", "工具管理", true, None::<&str>)?;
            if let Some(MenuItemKind::Submenu(application)) = menu.items()?.first() {
                #[cfg(target_os = "macos")]
                {
                    // Route Command-Q through the same graceful shutdown as closing the window.
                    application.remove_at(application.items()?.len() - 1)?;
                    let quit = MenuItem::with_id(app, "desktop-quit", "退出 ModelCat", true, Some("Cmd+Q"))?;
                    application.append(&quit)?;
                }
                application.insert(&settings, 2)?;
            }
            menu.append(&Submenu::with_items(app, "工作区", true, &[&tools, &stop])?)?;
            Ok(menu)
        })
        .on_menu_event(|app, event| {
            if event.id() == "desktop-quit" {
                vm::stop_workspace(app, true);
            }
            if event.id() == "stop-workspace" {
                vm::stop_workspace(app, false);
            }
            let page = match event.id().as_ref() {
                "desktop-settings" => "settings",
                "workspace-tools" => "tools",
                _ => return,
            };
            if let Some(window) = app.get_webview_window("main") {
                if window.url().is_ok_and(|url| url.scheme() == "tauri") {
                    let _ = window.eval(format!("window.modelcatOpenPage?.('{page}')"));
                } else {
                    let url = tauri::Url::parse(&format!("tauri://localhost/vm.html?page={page}")).unwrap();
                    let _ = window.navigate(url);
                }
                let _ = window.set_focus();
            }
        })
        .setup(|app| {
            let title = if cfg!(debug_assertions) {
                "ModelCat · 开发预览"
            } else {
                "ModelCat"
            };
            let appearance = saved_appearance(app.handle()).unwrap_or_default();
            app.set_theme(appearance.theme());
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("vm.html".into()))
                .title(title)
                .inner_size(1120.0, 780.0)
                .min_inner_size(800.0, 600.0)
                .incognito(true)
                .disable_drag_drop_handler()
                .on_navigation(|target| {
                    if (target.scheme() == "tauri" && target.host_str() == Some("localhost"))
                        || (target.scheme() == "http"
                            && target.host_str() == Some("127.0.0.1")
                            && target.port() == Some(vm::workspace_host_port()))
                    {
                        true
                    } else {
                        open_login(target);
                        false
                    }
                })
                .on_new_window(|target, _| {
                    open_login(&target);
                    tauri::webview::NewWindowResponse::Deny
                })
                .on_page_load(|_, payload| {
                    if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                        eprintln!("ModelCat page loaded");
                    }
                })
                .build()?;
            if let Ok(theme) = window.theme() {
                let _ = window.set_background_color(Some(canvas_color(theme)));
            }
            #[cfg(target_os = "macos")]
            install_quit_shortcut(app.handle());
            tauri::async_runtime::spawn(vm::monitor_workspace(app.handle().clone()));
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                vm::stop_workspace(window.app_handle(), true);
            }
            if let tauri::WindowEvent::ThemeChanged(theme) = event {
                let _ = window.set_background_color(Some(canvas_color(*theme)));
                if let Some(webview) = window.app_handle().get_webview_window("main") {
                    let _ = webview.set_background_color(Some(canvas_color(*theme)));
                }
            }
        })
        .build(tauri::generate_context!())?;
    result.run(|app, event| {
        match event {
            tauri::RunEvent::ExitRequested { code: None, api, .. } => {
                api.prevent_exit();
                vm::stop_workspace(app, true);
            }
            // macOS can skip ExitRequested for a system quit such as Dock → Quit.
            tauri::RunEvent::Exit => vm::stop_workspace_on_system_exit(app),
            _ => {}
        }
    });
    Ok(())
}
