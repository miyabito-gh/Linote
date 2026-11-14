//! Windows の明暗設定の取得、DPI、UTF-16 との文字列変換などの共通の補助。
//!
//! 画面部品ではない Win32 の薄い包み。`ui` の各部品と、ほかのタスク（プレビュー、
//! メモの小窓など）が共有する。
//!
//! 担当: タスク B。

use std::ffi::{c_void, OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{ERROR_SUCCESS, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

/// 96 DPI（拡大率 100%）。
pub const BASE_DPI: u32 = 96;

/// 現在のローカル時刻（年、月、日、時、分）。
pub fn local_time() -> (u16, u16, u16, u16, u16) {
    // SAFETY: 引数なしで現在時刻を読むだけ。
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    (t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute)
}

/// 文字列を NUL 終端の UTF-16 にする（Win32 の `PCWSTR` 引数用）。
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `OsStr`（パスなど）を NUL 終端の UTF-16 にする。
pub fn os_to_wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// UTF-16 を文字列にする。最初の NUL で打ち切る。不正なサロゲートは置換文字になる。
pub fn from_wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// UTF-16 を `OsString` にする（パス用。不正なサロゲートも保つ）。最初の NUL で打ち切る。
pub fn os_from_wide(s: &[u16]) -> OsString {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    OsString::from_wide(&s[..end])
}

/// Windows の「アプリのモード」が明るい（ライト）か。読めなければ `None`。
///
/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` の
/// `AppsUseLightTheme`（DWORD）。
pub fn apps_use_light_theme() -> Option<bool> {
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: 出力先は u32 1 つで、大きさを正しく渡している。
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then_some(data != 0)
}

/// `WM_SETTINGCHANGE` の `lParam` が明暗設定の変更（`"ImmersiveColorSet"`）を指すか。
pub fn is_color_setting_change(lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return false;
    }
    // SAFETY: WM_SETTINGCHANGE の lParam は NUL 終端の文字列へのポインタ（0 でなければ）。
    let s = unsafe { PCWSTR(lparam.0 as *const u16).to_string() };
    s.map(|s| s == "ImmersiveColorSet").unwrap_or(false)
}

/// プロセスを Per-Monitor V2 の DPI 対応にする。マニフェストで指定済みなら何もしない
/// （既に設定されているとエラーになるが無視する）。
pub fn enable_per_monitor_dpi() {
    // SAFETY: 引数は定義済みの定数。
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// ウィンドウの DPI。取れなければシステムの DPI。
pub fn dpi_for_window(hwnd: HWND) -> u32 {
    // SAFETY: 無効なウィンドウなら 0 が返る。
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi != 0 {
        dpi
    } else {
        system_dpi()
    }
}

pub fn system_dpi() -> u32 {
    // SAFETY: 引数なし。
    let dpi = unsafe { GetDpiForSystem() };
    if dpi == 0 {
        BASE_DPI
    } else {
        dpi
    }
}

/// 96 DPI 基準の長さ（DIP）を、指定の DPI のピクセルにする（四捨五入）。
pub const fn scale(dip: i32, dpi: u32) -> i32 {
    let v = dip as i64 * dpi as i64;
    let half = BASE_DPI as i64 / 2;
    (if v >= 0 { v + half } else { v - half } / BASE_DPI as i64) as i32
}

/// Wine の上で動いているか（`ntdll.dll` の `wine_get_version` の有無）。
/// Wine の DirectWrite は Scintilla の描画に使えないので、編集欄は GDI で描く。
pub fn is_wine() -> bool {
    use windows::core::s;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    static WINE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WINE.get_or_init(|| {
        // SAFETY: 読み込み済みの ntdll の関数の有無を調べるだけ（呼ばない）。
        unsafe {
            GetModuleHandleW(w!("ntdll.dll"))
                .map(|m| GetProcAddress(m, s!("wine_get_version")).is_some())
                .unwrap_or(false)
        }
    })
}

/// タイトルバーを暗い色にする（Windows 11 の `DWMWA_USE_IMMERSIVE_DARK_MODE`）。
/// 対応していない環境では何もしない。
pub fn set_dark_title_bar(hwnd: HWND, dark: bool) {
    let value = BOOL::from(dark);
    // SAFETY: 値は BOOL 1 つで、大きさを正しく渡している。
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &value as *const BOOL as *const c_void,
            std::mem::size_of::<BOOL>() as u32,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_round_trip() {
        let w = to_wide("日本語 abc");
        assert_eq!(w.last(), Some(&0));
        assert_eq!(from_wide(&w), "日本語 abc");
        let p = os_to_wide(OsStr::new("C:\\a\\メモ.txt"));
        assert_eq!(os_from_wide(&p), OsString::from("C:\\a\\メモ.txt"));
        assert_eq!(from_wide(&[0x61, 0, 0x62]), "a");
    }

    #[test]
    fn scaling_rounds_to_nearest() {
        assert_eq!(scale(10, 96), 10);
        assert_eq!(scale(10, 144), 15);
        assert_eq!(scale(3, 120), 4); // 3.75
        assert_eq!(scale(1, 168), 2); // 1.75
        assert_eq!(scale(-3, 120), -4);
        assert_eq!(scale(0, 192), 0);
    }

    #[test]
    fn color_setting_change_detection() {
        let s = to_wide("ImmersiveColorSet");
        assert!(is_color_setting_change(LPARAM(s.as_ptr() as isize)));
        let other = to_wide("Environment");
        assert!(!is_color_setting_change(LPARAM(other.as_ptr() as isize)));
        assert!(!is_color_setting_change(LPARAM(0)));
    }
}
