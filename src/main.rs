#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::{
    Icon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VK_CONTROL, VK_SHIFT,
    VK_V,
};

fn main() {
    let temp_dir = std::env::temp_dir().join("claude-images");
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");

    let event_loop = EventLoopBuilder::new().build();

    let hotkey_manager = GlobalHotKeyManager::new().expect("hotkey manager");
    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyV);
    let hotkey_id = hotkey.id();
    hotkey_manager.register(hotkey).expect("register hotkey");
    let mut enabled = true;
    let mut registered = true;
    let mut last_path: Option<String> = None;
    let mut reregister_at: Option<Instant> = None;

    let menu = Menu::new();
    let toggle_item = CheckMenuItem::new("Enabled (Ctrl+Shift+V)", true, true, None);
    let open_folder_item = MenuItem::new("Open images folder", true, None);
    let clear_folder_item = MenuItem::new("Clear images folder", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    menu.append(&toggle_item).unwrap();
    menu.append(&PredefinedMenuItem::separator()).unwrap();
    menu.append(&open_folder_item).unwrap();
    menu.append(&clear_folder_item).unwrap();
    menu.append(&PredefinedMenuItem::separator()).unwrap();
    menu.append(&quit_item).unwrap();

    let toggle_id = toggle_item.id().clone();
    let open_folder_id = open_folder_item.id().clone();
    let clear_folder_id = clear_folder_item.id().clone();
    let quit_id = quit_item.id().clone();

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("claude-image-fixer (Ctrl+Shift+V)")
        .with_icon(build_icon())
        .build()
        .expect("tray");

    let menu_rx = MenuEvent::receiver();
    let hotkey_rx = GlobalHotKeyEvent::receiver();
    let poll_interval = Duration::from_millis(50);

    event_loop.run(move |_event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + poll_interval);

        if let Some(deadline) = reregister_at {
            if Instant::now() >= deadline {
                if enabled && !registered {
                    let _ = hotkey_manager.register(hotkey);
                    registered = true;
                }
                reregister_at = None;
            }
        }

        while let Ok(e) = hotkey_rx.try_recv() {
            if e.id == hotkey_id && e.state == HotKeyState::Pressed && enabled {
                let _ = hotkey_manager.unregister(hotkey);
                registered = false;
                if let Err(err) = handle_hotkey(&temp_dir, &mut last_path) {
                    eprintln!("hotkey error: {err}");
                }
                reregister_at = Some(Instant::now() + Duration::from_millis(300));
            }
        }

        while let Ok(e) = menu_rx.try_recv() {
            if e.id == toggle_id {
                let now_checked = toggle_item.is_checked();
                if now_checked && !enabled {
                    let _ = hotkey_manager.register(hotkey);
                    enabled = true;
                    registered = true;
                } else if !now_checked && enabled {
                    if registered {
                        let _ = hotkey_manager.unregister(hotkey);
                        registered = false;
                    }
                    enabled = false;
                }
            } else if e.id == open_folder_id {
                let _ = std::process::Command::new("explorer.exe")
                    .arg(&temp_dir)
                    .spawn();
            } else if e.id == clear_folder_id {
                if let Err(err) = clear_images(&temp_dir) {
                    eprintln!("clear error: {err}");
                }
            } else if e.id == quit_id {
                *control_flow = ControlFlow::Exit;
            }
        }
    });
}

fn handle_hotkey(
    dir: &Path,
    last_path: &mut Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut clip = arboard::Clipboard::new()?;

    let wsl = match clip.get_image() {
        Ok(img) => {
            let path = save_image(dir, img)?;
            let wsl = windows_to_wsl(&path);
            *last_path = Some(wsl.clone());
            wsl
        }
        Err(_) => match last_path {
            Some(p) => p.clone(),
            None => return Err("no image on clipboard and no cached path yet".into()),
        },
    };

    clip.set_text(&wsl)?;
    drop(clip);

    std::thread::sleep(Duration::from_millis(60));
    unsafe { send_paste() };
    Ok(())
}

fn save_image(
    dir: &Path,
    img: arboard::ImageData,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let bytes = img.bytes.into_owned();
    let hash_hex = blake3::hash(&bytes).to_hex();
    let short = &hash_hex.as_str()[..16];
    let path = dir.join(format!("clip-{short}.png"));

    if path.exists() {
        return Ok(path);
    }

    let buffer: image::RgbaImage =
        image::ImageBuffer::from_raw(img.width as u32, img.height as u32, bytes)
            .ok_or("clipboard image dimensions don't match buffer")?;
    buffer.save(&path)?;
    Ok(path)
}

fn clear_images(dir: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

fn windows_to_wsl(path: &Path) -> String {
    let mut s = path.to_string_lossy().into_owned();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        s = stripped.to_string();
    }
    if let Some((drive, rest)) = s.split_once(':') {
        if drive.len() == 1 && drive.chars().next().unwrap().is_ascii_alphabetic() {
            let drive_lc = drive.to_ascii_lowercase();
            let rest = rest.replace('\\', "/");
            return format!("/mnt/{drive_lc}{rest}");
        }
    }
    s.replace('\\', "/")
}

unsafe fn send_paste() {
    // Force a clean Ctrl+Shift+V (Linux/WSL terminal paste). The hotkey has been
    // unregistered for this brief window, so the injection won't re-fire it.
    let inputs = [
        key(VK_CONTROL, KEYEVENTF_KEYUP),
        key(VK_SHIFT, KEYEVENTF_KEYUP),
        key(VK_CONTROL, 0),
        key(VK_SHIFT, 0),
        key(VK_V, 0),
        key(VK_V, KEYEVENTF_KEYUP),
        key(VK_SHIFT, KEYEVENTF_KEYUP),
        key(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    SendInput(
        inputs.len() as u32,
        inputs.as_ptr(),
        std::mem::size_of::<INPUT>() as i32,
    );
}

fn key(vk: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn build_icon() -> Icon {
    const SIZE: u32 = 32;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let cx = SIZE as f32 / 2.0;
    let cy = SIZE as f32 / 2.0;
    let outer = SIZE as f32 / 2.0 - 0.5;
    let inner = outer - 5.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            if d <= inner {
                rgba.extend_from_slice(&[245, 245, 245, 255]);
            } else if d <= outer {
                rgba.extend_from_slice(&[40, 110, 220, 255]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("icon")
}
