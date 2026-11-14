//! ホットキー登録とクリップボードの往復（Wine＋Xvfb。作れない環境では飛ばす）。
//! `xvfb-run -a env WINEDEBUG=-all cargo test --test platform_hotkey_clipboard`
#![cfg(windows)]

use windows::core::w;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};

use linote::command::KeyChord;
use linote::platform::clipboard;
use linote::platform::hotkey::{GlobalHotkey, HotkeyStatus};

fn window() -> Option<HWND> {
    // SAFETY: 標準の STATIC クラスで作る。
    unsafe {
        let h = GetModuleHandleW(None).ok()?;
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("STATIC"),
            w!("t"),
            WS_OVERLAPPED,
            0,
            0,
            10,
            10,
            None,
            None,
            Some(h.into()),
            None,
        )
        .ok()
    }
}

#[test]
fn clipboard_round_trip_keeps_text() {
    let Some(hwnd) = window() else {
        eprintln!("skip: no window");
        return;
    };
    let text = "日本語\tタブ\r\n二行目\n三行目";
    if clipboard::write_text(Some(hwnd), text).is_err() {
        eprintln!("skip: clipboard unavailable");
        return;
    }
    assert!(clipboard::has_text());
    assert_eq!(
        clipboard::read_text(Some(hwnd)).unwrap().as_deref(),
        Some(text)
    );
    // SAFETY: 自分で作ったウィンドウ。
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

#[test]
fn hotkey_register_conflict_and_reset() {
    let Some(hwnd) = window() else {
        eprintln!("skip: no window");
        return;
    };
    let mut a = GlobalHotkey::new(hwnd, 1);
    let mut b = GlobalHotkey::new(hwnd, 2);
    let chord = KeyChord::parse("Ctrl+Alt+Shift+F11").unwrap();
    if !matches!(a.register(chord), HotkeyStatus::Registered(_)) {
        eprintln!("skip: RegisterHotKey unavailable");
        return;
    }
    // 同じキーの二重登録は競合として報告され、別のキーへ再設定できる。
    assert!(matches!(
        b.set_from_text("Ctrl+Alt+Shift+F11"),
        HotkeyStatus::Conflict(_)
    ));
    assert!(matches!(
        b.set_from_text("Ctrl+Alt+Shift+F10"),
        HotkeyStatus::Registered(_)
    ));
    assert!(matches!(b.set_from_text("bogus"), HotkeyStatus::Invalid(_)));
    assert!(matches!(b.set_from_text("F5"), HotkeyStatus::Invalid(_)));
    assert_eq!(b.set_from_text(""), &HotkeyStatus::Unset);
    drop(a);
    assert!(matches!(
        b.set_from_text("Ctrl+Alt+Shift+F11"),
        HotkeyStatus::Registered(_)
    ));
    // SAFETY: 自分で作ったウィンドウ。
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}
