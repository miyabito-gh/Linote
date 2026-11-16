//! タブにマウスを置いたときの小さな案内（独自描画の吹き出し）。
//!
//! 標準のツールチップは使わない。メニューやタブと同じ配色（[`crate::ui::theme::Theme`]）で
//! 描き、明暗のテーマの切り替えに追従するため、表示のたびに現在のテーマの色を取り込み、
//! テーマや DPI が替わったら呼び出し側が [`TabTip::hide`] で隠す。
//!
//! - マウスを受けない（`HTTRANSPARENT`）ので、吹き出しの上にマウスが来てもタブの
//!   ホバーの状態は変わらない。フォーカスも奪わない。
//! - 表示の位置は、タブの真下。作業領域からはみ出さないよう収める。

use std::cell::{Cell, RefCell};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, SelectObject, HFONT,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, LoadCursorW, RegisterClassExW, SetWindowPos, ShowWindow,
    HTTRANSPARENT, HWND_TOPMOST, IDC_ARROW, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE,
    WM_ERASEBKGND, WM_NCHITTEST, WM_PAINT, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::platform::system::scale;
use crate::ui::theme::{Rgb, Theme};
use crate::ui::window::{Canvas, UiFont};

const CLASS: PCWSTR = w!("LinoteTabTip");

/// 吹き出しの最大の幅（96 DPI の値）。これを超える文字列は末尾を省略する。
const MAX_WIDTH_DIP: i32 = 560;
/// 文字と枠の間（96 DPI の値）。
const PAD_X_DIP: i32 = 8;
const PAD_Y_DIP: i32 = 4;
/// タブとの間隔（96 DPI の値）。
const GAP_DIP: i32 = 2;

/// 描画に必要な値（表示のときに控える。UI スレッドだけが触る）。
struct Paint {
    text: String,
    bg: Rgb,
    fg: Rgb,
    border: Rgb,
    font: HFONT,
    dpi: u32,
}

thread_local! {
    static PAINT: RefCell<Option<Paint>> = const { RefCell::new(None) };
}

/// タブの案内の窓（初めて表示するときに作る）。
#[derive(Default)]
pub struct TabTip {
    hwnd: Cell<HWND>,
    visible: Cell<bool>,
}

impl TabTip {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_visible(&self) -> bool {
        self.visible.get()
    }

    /// `anchor`（画面座標のタブの矩形）の真下に `text` を出す。
    pub fn show(
        &self,
        owner: HWND,
        anchor: RECT,
        text: &str,
        theme: &Theme,
        font: &UiFont,
        dpi: u32,
    ) {
        if text.is_empty() || !self.ensure_window(owner) {
            return;
        }
        let max_w = scale(MAX_WIDTH_DIP, dpi);
        let size = font.measure_text(text, None);
        let (pad_x, pad_y) = (scale(PAD_X_DIP, dpi), scale(PAD_Y_DIP, dpi));
        let border = 1;
        let width = (size.cx + 2 * (pad_x + border)).min(max_w);
        let height = size.cy + 2 * (pad_y + border);
        // 作業領域（タブのあるモニター）に収める。
        let work = work_area(POINT {
            x: anchor.left,
            y: anchor.bottom,
        });
        let mut x = anchor.left;
        let mut y = anchor.bottom + scale(GAP_DIP, dpi);
        if x + width > work.right {
            x = work.right - width;
        }
        x = x.max(work.left);
        if y + height > work.bottom {
            // 下に収まらなければ、タブの上に出す。
            y = (anchor.top - height - scale(GAP_DIP, dpi)).max(work.top);
        }
        let ui = &theme.ui;
        PAINT.with(|p| {
            *p.borrow_mut() = Some(Paint {
                text: text.to_owned(),
                bg: ui.bar_bg,
                fg: ui.tab_active_fg,
                border: ui.tab_separator,
                font: font.handle(),
                dpi,
            });
        });
        // SAFETY: 自分が作った有効なウィンドウ。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd.get(),
                Some(HWND_TOPMOST),
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.hwnd.get(), SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(Some(self.hwnd.get()), None, true);
        }
        self.visible.set(true);
    }

    /// 隠す（出ていなければ何もしない）。テーマや DPI の変更、クリック、タブの変更で呼ぶ。
    pub fn hide(&self) {
        if !self.visible.replace(false) {
            return;
        }
        // SAFETY: 自分が作った有効なウィンドウ。
        unsafe {
            let _ = ShowWindow(self.hwnd.get(), SW_HIDE);
        }
        PAINT.with(|p| *p.borrow_mut() = None);
    }

    fn ensure_window(&self, owner: HWND) -> bool {
        if !self.hwnd.get().is_invalid() {
            return true;
        }
        if !register_class() {
            return false;
        }
        // SAFETY: クラスは登録済み。owner は呼び出し側が渡した有効なウィンドウ。
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                CLASS,
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                Some(owner),
                None,
                GetModuleHandleW(None).ok().map(Into::into),
                None,
            )
        };
        match hwnd {
            Ok(h) => {
                self.hwnd.set(h);
                true
            }
            Err(_) => false,
        }
    }
}

/// `p` のあるモニターの作業領域。取れなければ十分に広い領域。
fn work_area(p: POINT) -> RECT {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: 構造体は呼び出しの間有効で、cbSize を設定している。
    let ok = unsafe {
        let m = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        GetMonitorInfoW(m, &mut info).as_bool()
    };
    if ok {
        info.rcWork
    } else {
        RECT {
            left: 0,
            top: 0,
            right: i32::MAX / 2,
            bottom: i32::MAX / 2,
        }
    }
}

fn register_class() -> bool {
    static REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *REGISTERED.get_or_init(|| {
        // SAFETY: クラスの情報は呼び出しの間有効。
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_proc),
                hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class) != 0
        }
    })
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // マウスを受けない（吹き出しの下のタブがそのままマウスを受ける）。
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            PAINT.with(|p| {
                if let Some(p) = p.borrow().as_ref() {
                    paint(hwnd, hdc, p);
                }
            });
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint(hwnd: HWND, hdc: windows::Win32::Graphics::Gdi::HDC, p: &Paint) {
    let mut client = RECT::default();
    if windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client).is_err() {
        return;
    }
    let c = Canvas { hdc, dpi: p.dpi };
    let old = SelectObject(hdc, p.font.into());
    // 1px の枠、内側は背景。
    c.fill(&client, p.border);
    let inner = RECT {
        left: client.left + 1,
        top: client.top + 1,
        right: client.right - 1,
        bottom: client.bottom - 1,
    };
    c.fill(&inner, p.bg);
    let text_rect = RECT {
        left: inner.left + c.px(PAD_X_DIP),
        right: inner.right - c.px(PAD_X_DIP),
        ..inner
    };
    c.text(&text_rect, &p.text, p.fg, false);
    SelectObject(hdc, old);
}
