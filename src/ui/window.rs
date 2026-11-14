//! メインウィンドウ（ウィンドウクラス、配置、DPI、フォーカス）。
//!
//! - [`compute_layout`] は、上部のタブバー、中央の編集欄、右ペイン（プレビュー用。後から
//!   差し込む）、下部の差し込み欄（検索欄など）、ステータスバーの配置を計算する。
//! - [`UiFont`]、[`Canvas`]、[`BufferedPaint`] は、タブバー・ステータスバーの独自描画用の
//!   GDI の薄い包み。
//! - ウィンドウプロシージャはメッセージを [`crate::ui::app`] に渡すだけ。
//!
//! 担当: タスク B。

use std::ffi::c_void;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontIndirectW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, Ellipse, FillRect, GetStockObject,
    GetTextExtentPoint32W, GetTextMetricsW, LineTo, MoveToEx, SelectObject, SetBkMode,
    SetTextColor, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    HBITMAP, HDC, HFONT, HGDIOBJ, LOGFONTW, NULL_PEN, PS_SOLID, SRCCOPY, TEXTMETRICW, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::SystemParametersInfoForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, LoadCursorW, LoadIconW, RegisterClassExW, CS_DBLCLKS,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS,
    WM_APP, WNDCLASSEXW, WS_CLIPCHILDREN, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW,
};

use crate::core::{Error, Result};
use crate::platform::system::scale;
use crate::ui::theme::Rgb;

/// ワーカーからの結果が届いたことを知らせるアプリ定義のメッセージ（`UiWaker`）。
pub const WM_APP_WAKE: u32 = WM_APP + 1;
/// 入れ子の処理中に描画を見送ったとき、後から描き直すためのメッセージ。
pub const WM_APP_REPAINT: u32 = WM_APP + 2;
/// `WM_APP + 16` 以降はほかのタスクが自由に使ってよい。
pub const WM_APP_FIRST_FREE: u32 = WM_APP + 16;

/// メインウィンドウのクラス名。
pub const MAIN_CLASS: PCWSTR = w!("LinoteMainWindow");

/// 子ウィンドウの ID（`WM_NOTIFY` の送り元の区別には HWND を使う）。
pub const ID_EDITOR: isize = 100;
pub const ID_FACTORY: isize = 101;

/// 配置の寸法（96 DPI の値）。
pub mod dims {
    /// タブバーの高さ。
    pub const TAB_BAR_HEIGHT: i32 = 24;
    /// ステータスバーの高さ。
    pub const STATUS_HEIGHT: i32 = 24;
    /// 右ペインとの区切りの幅（ドラッグできる範囲）。
    pub const SPLITTER: i32 = 5;
    /// 編集欄・右ペインの最小幅。
    pub const MIN_PANE: i32 = 120;
}

/// メインウィンドウの各部の位置（クライアント座標）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Layout {
    pub tabs: RECT,
    pub editor: RECT,
    pub splitter: Option<RECT>,
    pub right: Option<RECT>,
    pub bottom: Option<RECT>,
    pub status: RECT,
}

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT {
        left,
        top,
        right: right.max(left),
        bottom: bottom.max(top),
    }
}

/// 配置を計算する。`right_ratio` は右ペインの幅の割合（0〜1。`None` なら右ペインなし）、
/// `bottom_height` は下部の差し込み欄の高さ（DIP。`None` ならなし）。
pub fn compute_layout(
    client: RECT,
    dpi: u32,
    right_ratio: Option<f32>,
    bottom_height: Option<i32>,
) -> Layout {
    compute_layout_with_tab_height(
        client,
        dpi,
        right_ratio,
        bottom_height,
        scale(dims::TAB_BAR_HEIGHT, dpi),
    )
}

/// UI 書体の実測に合わせたタブ高さ（ピクセル）で配置する。
pub fn compute_layout_with_tab_height(
    client: RECT,
    dpi: u32,
    right_ratio: Option<f32>,
    bottom_height: Option<i32>,
    tab_height: i32,
) -> Layout {
    compute_layout_with_bars(client, dpi, right_ratio, bottom_height, tab_height, true)
}

/// 文書を狭めるバーの実寸と、ステータスバー表示に応じた配置。
/// 非表示時の status は高さ0にし、編集欄・差し込み欄を下端まで広げる。
pub fn compute_layout_with_bars(
    client: RECT,
    dpi: u32,
    right_ratio: Option<f32>,
    bottom_height: Option<i32>,
    tab_height: i32,
    status_visible: bool,
) -> Layout {
    let (l, t, r, b) = (client.left, client.top, client.right, client.bottom);
    let tab_h = tab_height.max(0).min((b - t).max(0));
    let status_h = if status_visible {
        scale(dims::STATUS_HEIGHT, dpi).min((b - t - tab_h).max(0))
    } else {
        0
    };
    let tabs = rect(l, t, r, t + tab_h);
    let status = rect(l, b - status_h, r, b);
    let mut body_bottom = status.top;
    let bottom = bottom_height.map(|h| {
        let h = scale(h, dpi).min((body_bottom - tabs.bottom).max(0));
        body_bottom -= h;
        rect(l, body_bottom, r, body_bottom + h)
    });
    let body_top = tabs.bottom;
    let (editor, splitter, right) = match right_ratio {
        None => (rect(l, body_top, r, body_bottom), None, None),
        Some(ratio) => {
            let width = r - l;
            let split = scale(dims::SPLITTER, dpi);
            let min = scale(dims::MIN_PANE, dpi);
            let mut right_w = (width as f32 * ratio.clamp(0.0, 1.0)).round() as i32;
            right_w = right_w.clamp(
                min.min(width / 2),
                (width - split - min).max(min.min(width / 2)),
            );
            let right_left = (r - right_w).max(l);
            let split_left = (right_left - split).max(l);
            (
                rect(l, body_top, split_left, body_bottom),
                Some(rect(split_left, body_top, right_left, body_bottom)),
                Some(rect(right_left, body_top, r, body_bottom)),
            )
        }
    };
    Layout {
        tabs,
        editor,
        splitter,
        right,
        bottom,
        status,
    }
}

/// 区切りをドラッグした位置 `x`（クライアント座標）から右ペインの割合を求める。
pub fn ratio_from_splitter_x(client: RECT, x: i32) -> f32 {
    let width = (client.right - client.left).max(1) as f32;
    ((client.right - x) as f32 / width).clamp(0.1, 0.9)
}

pub fn contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

// ------------------------------------------------------------------ GDI の包み

/// 画面部品の書体（Windows のメッセージ用の書体を DPI に合わせて作る）。
#[derive(Debug)]
pub struct UiFont {
    font: HFONT,
    text_height: i32,
}

impl UiFont {
    pub fn for_dpi(dpi: u32) -> UiFont {
        let mut metrics = NONCLIENTMETRICSW {
            cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        // SAFETY: 出力先の大きさを cbSize に入れて渡している。
        let ok = unsafe {
            SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                metrics.cbSize,
                Some(&mut metrics as *mut NONCLIENTMETRICSW as *mut c_void),
                0,
                dpi,
            )
        }
        .is_ok();
        let mut lf = if ok {
            metrics.lfMessageFont
        } else {
            let mut lf = LOGFONTW {
                lfHeight: -scale(12, dpi),
                ..Default::default()
            };
            let name: Vec<u16> = "Segoe UI".encode_utf16().collect();
            lf.lfFaceName[..name.len()].copy_from_slice(&name);
            lf
        };
        lf.lfQuality = windows::Win32::Graphics::Gdi::CLEARTYPE_QUALITY;
        // SAFETY: 有効な LOGFONTW。
        let font = unsafe { CreateFontIndirectW(&lf) };
        // SAFETY: 測定専用のDCへ選択し、元の書体を戻してから破棄する。
        let text_height = unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                lf.lfHeight.abs()
            } else {
                let old = SelectObject(dc, font.into());
                let mut text = TEXTMETRICW::default();
                let measured = GetTextMetricsW(dc, &mut text).as_bool();
                SelectObject(dc, old);
                let _ = DeleteDC(dc);
                if measured {
                    text.tmHeight
                } else {
                    lf.lfHeight.abs()
                }
            }
        };
        UiFont { font, text_height }
    }

    /// UI の文字高と上下 4 DIP の余白。大きな OS 書体を切らず、標準時は 24 DIP。
    pub fn tab_height(&self, dpi: u32) -> i32 {
        tab_height_for_text(self.text_height, dpi)
    }

    pub fn handle(&self) -> HFONT {
        self.font
    }

    /// Measure with the same UI font used by controls. A width enables word wrapping.
    pub fn measure_text(&self, text: &str, max_width: Option<i32>) -> SIZE {
        let mut text: Vec<u16> = text.encode_utf16().collect();
        if text.is_empty() {
            text.push(b' ' as u16);
        }
        let mut rect = RECT {
            right: max_width.unwrap_or(0).max(1),
            ..Default::default()
        };
        // SAFETY: measurement-only DC, with its original font restored before deletion.
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return SIZE {
                    cx: 0,
                    cy: self.text_height,
                };
            }
            let old = SelectObject(dc, self.font.into());
            let flags = windows::Win32::Graphics::Gdi::DT_CALCRECT
                | DT_NOPREFIX
                | if max_width.is_some() {
                    windows::Win32::Graphics::Gdi::DT_WORDBREAK
                } else {
                    DT_SINGLELINE
                };
            DrawTextW(dc, &mut text, &mut rect, flags);
            SelectObject(dc, old);
            let _ = DeleteDC(dc);
        }
        SIZE {
            cx: rect.right - rect.left,
            cy: rect.bottom - rect.top,
        }
    }
}

fn tab_height_for_text(text_height: i32, dpi: u32) -> i32 {
    (text_height.max(0) + scale(8, dpi)).max(scale(dims::TAB_BAR_HEIGHT, dpi))
}

impl Drop for UiFont {
    fn drop(&mut self) {
        if !self.font.is_invalid() {
            // SAFETY: 自分が作った書体を 1 回だけ消す。
            unsafe {
                let _ = DeleteObject(self.font.into());
            }
        }
    }
}

/// 描画先の DC に対する簡単な描画命令。座標は DC の座標。
pub struct Canvas {
    pub hdc: HDC,
    pub dpi: u32,
}

impl Canvas {
    pub fn px(&self, dip: i32) -> i32 {
        scale(dip, self.dpi)
    }

    pub fn fill(&self, r: &RECT, color: Rgb) {
        // SAFETY: 作ったブラシは使い終わったら消す。
        unsafe {
            let brush = CreateSolidBrush(COLORREF(color.colorref()));
            FillRect(self.hdc, r, brush);
            let _ = DeleteObject(brush.into());
        }
    }

    pub fn set_font(&self, font: &UiFont) {
        // SAFETY: 書体は描画の間 UiFont が持ち続ける。前の書体は BufferedPaint が戻す。
        unsafe {
            SelectObject(self.hdc, font.handle().into());
        }
    }

    /// 文字列の幅（ピクセル）。
    pub fn measure(&self, text: &str) -> i32 {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut size = SIZE::default();
        // SAFETY: 出力先は SIZE 1 つ。
        unsafe {
            let _ = GetTextExtentPoint32W(self.hdc, &wide, &mut size);
        }
        size.cx
    }

    /// 1 行の文字列を描く（縦は中央、はみ出す部分は「…」）。
    pub fn text(&self, r: &RECT, text: &str, color: Rgb, align_right: bool) {
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        if wide.is_empty() {
            return;
        }
        let mut r = *r;
        let align = if align_right { DT_RIGHT } else { DT_LEFT };
        // SAFETY: 文字列と矩形は呼び出しの間有効。
        unsafe {
            SetBkMode(self.hdc, TRANSPARENT);
            SetTextColor(self.hdc, COLORREF(color.colorref()));
            DrawTextW(
                self.hdc,
                &mut wide,
                &mut r,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX | align,
            );
        }
    }

    /// 塗りつぶした円（直径 `d`、中心 `(cx, cy)`）。
    pub fn dot(&self, cx: i32, cy: i32, d: i32, color: Rgb) {
        // SAFETY: 作ったブラシは使い終わったら戻して消す。
        unsafe {
            let brush = CreateSolidBrush(COLORREF(color.colorref()));
            let old_brush = SelectObject(self.hdc, brush.into());
            let old_pen = SelectObject(self.hdc, GetStockObject(NULL_PEN));
            let r0 = d / 2;
            let _ = Ellipse(self.hdc, cx - r0, cy - r0, cx - r0 + d + 1, cy - r0 + d + 1);
            SelectObject(self.hdc, old_pen);
            SelectObject(self.hdc, old_brush);
            let _ = DeleteObject(brush.into());
        }
    }

    /// 線（幅 `width` ピクセル）。
    pub fn line(&self, x0: i32, y0: i32, x1: i32, y1: i32, width: i32, color: Rgb) {
        // SAFETY: 作ったペンは使い終わったら戻して消す。
        unsafe {
            let pen = CreatePen(PS_SOLID, width.max(1), COLORREF(color.colorref()));
            let old = SelectObject(self.hdc, pen.into());
            let _ = MoveToEx(self.hdc, x0, y0, None);
            let _ = LineTo(self.hdc, x1, y1);
            SelectObject(self.hdc, old);
            let _ = DeleteObject(pen.into());
        }
    }
}

/// ちらつきを防ぐための裏画面。`target` の DC の `area` に、終わったら転送する。
/// 描画は (0, 0) を `area` の左上とする座標で行う。
pub struct BufferedPaint {
    target: HDC,
    area: RECT,
    dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    old_font: HGDIOBJ,
}

impl BufferedPaint {
    pub fn new(target: HDC, area: RECT) -> Option<BufferedPaint> {
        let (w, h) = (area.right - area.left, area.bottom - area.top);
        if w <= 0 || h <= 0 {
            return None;
        }
        // SAFETY: 作った DC とビットマップは Drop で戻して消す。
        unsafe {
            let dc = CreateCompatibleDC(Some(target));
            if dc.is_invalid() {
                return None;
            }
            let bitmap = CreateCompatibleBitmap(target, w, h);
            if bitmap.is_invalid() {
                let _ = DeleteDC(dc);
                return None;
            }
            let old_bitmap = SelectObject(dc, bitmap.into());
            let old_font = SelectObject(
                dc,
                GetStockObject(windows::Win32::Graphics::Gdi::DEFAULT_GUI_FONT),
            );
            Some(BufferedPaint {
                target,
                area,
                dc,
                bitmap,
                old_bitmap,
                old_font,
            })
        }
    }

    pub fn canvas(&self, dpi: u32) -> Canvas {
        Canvas { hdc: self.dc, dpi }
    }

    /// 描画する範囲の大きさ（(0, 0) 始まり）。
    pub fn local_rect(&self) -> RECT {
        rect(
            0,
            0,
            self.area.right - self.area.left,
            self.area.bottom - self.area.top,
        )
    }
}

impl Drop for BufferedPaint {
    fn drop(&mut self) {
        let (w, h) = (
            self.area.right - self.area.left,
            self.area.bottom - self.area.top,
        );
        // SAFETY: new で作ったものを転送して戻し、消す。
        unsafe {
            let _ = BitBlt(
                self.target,
                self.area.left,
                self.area.top,
                w,
                h,
                Some(self.dc),
                0,
                0,
                SRCCOPY,
            );
            SelectObject(self.dc, self.old_font);
            SelectObject(self.dc, self.old_bitmap);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

// ------------------------------------------------------------------ ウィンドウ

/// メインウィンドウのクラスを登録する（2 回目以降は何もしない）。
pub fn register_main_class(instance: HINSTANCE) -> Result<()> {
    static REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ok = *REGISTERED.get_or_init(|| {
        // SAFETY: クラスの情報は呼び出しの間有効。
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_DBLCLKS | CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(main_wndproc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                // EXE に埋め込んだアイコン（res/linote.rc の ID 1）。小さい方は Windows が
                // 同じ .ico から合う大きさを選ぶ。
                hIcon: LoadIconW(
                    Some(instance),
                    PCWSTR(std::ptr::without_provenance::<u16>(1)),
                )
                .unwrap_or_default(),
                lpszClassName: MAIN_CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class) != 0
        }
    });
    if ok {
        Ok(())
    } else {
        Err(Error::unavailable(
            "メインウィンドウ",
            "ウィンドウクラスを登録できませんでした",
        ))
    }
}

/// メインウィンドウを作る（まだ表示しない）。
pub fn create_main_window(instance: HINSTANCE) -> Result<HWND> {
    // SAFETY: 登録済みのクラスで作る。
    unsafe {
        CreateWindowExW(
            WS_EX_APPWINDOW,
            MAIN_CLASS,
            w!("Linote"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .map_err(|e| Error::unavailable("メインウィンドウ", format!("作れませんでした: {e}")))
}

extern "system" fn main_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match crate::ui::app::dispatch(hwnd, msg, wparam, lparam) {
        Some(result) => result,
        // SAFETY: 既定の処理。
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(w: i32, h: i32) -> RECT {
        rect(0, 0, w, h)
    }

    #[test]
    fn layout_stacks_tabs_editor_and_status() {
        let l = compute_layout(client(800, 600), 96, None, None);
        assert_eq!(l.tabs, rect(0, 0, 800, 24));
        assert_eq!(l.status, rect(0, 576, 800, 600));
        assert_eq!(l.editor, rect(0, 24, 800, 576));
        assert!(l.right.is_none() && l.splitter.is_none() && l.bottom.is_none());
    }

    #[test]
    fn layout_scales_with_dpi() {
        let l = compute_layout(client(1600, 1200), 192, None, Some(40));
        assert_eq!(l.tabs.bottom, 48);
        assert_eq!(l.status.top, 1200 - 48);
        let bottom = l.bottom.unwrap();
        assert_eq!((bottom.top, bottom.bottom), (1200 - 48 - 80, 1200 - 48));
        assert_eq!(l.editor.bottom, bottom.top);
    }

    #[test]
    fn layout_with_right_pane() {
        let l = compute_layout(client(1000, 600), 96, Some(0.4), None);
        let right = l.right.unwrap();
        let split = l.splitter.unwrap();
        assert_eq!(right.right, 1000);
        assert_eq!(right.left, 600);
        assert_eq!(split.right, right.left);
        assert_eq!(l.editor.right, split.left);
        assert_eq!(split.right - split.left, 5);
        // 極端な割合でも編集欄と右ペインは最小幅を保つ。
        let l = compute_layout(client(1000, 600), 96, Some(1.0), None);
        assert!(l.editor.right - l.editor.left >= dims::MIN_PANE);
        let l = compute_layout(client(1000, 600), 96, Some(0.0), None);
        assert!(l.right.unwrap().right - l.right.unwrap().left >= dims::MIN_PANE);
    }

    #[test]
    fn tiny_windows_do_not_produce_negative_rects() {
        let l = compute_layout(client(50, 20), 144, Some(0.5), Some(100));
        for r in [
            l.tabs,
            l.editor,
            l.status,
            l.right.unwrap(),
            l.bottom.unwrap(),
        ] {
            assert!(r.right >= r.left && r.bottom >= r.top, "{r:?}");
        }
    }

    #[test]
    fn hidden_status_bar_gives_its_height_to_the_document_at_each_dpi() {
        for dpi in [96, 144, 192] {
            for bottom in [None, Some(70)] {
                let shown = compute_layout_with_bars(
                    client(800, 600),
                    dpi,
                    Some(0.4),
                    bottom,
                    scale(24, dpi),
                    true,
                );
                let hidden = compute_layout_with_bars(
                    client(800, 600),
                    dpi,
                    Some(0.4),
                    bottom,
                    scale(24, dpi),
                    false,
                );
                assert_eq!(hidden.status.top, hidden.status.bottom);
                assert_eq!(hidden.status.bottom, 600);
                assert_eq!(
                    hidden.editor.bottom - shown.editor.bottom,
                    scale(dims::STATUS_HEIGHT, dpi)
                );
                assert_eq!(hidden.right.unwrap().bottom, hidden.editor.bottom);
                if let Some(panel) = hidden.bottom {
                    assert_eq!(panel.bottom, 600);
                }
            }
        }
    }

    #[test]
    fn tab_height_tracks_ui_text_and_dpi_without_clipping() {
        assert_eq!(tab_height_for_text(15, 96), 24);
        assert_eq!(tab_height_for_text(20, 96), 28);
        assert_eq!(tab_height_for_text(30, 192), 48);
        assert_eq!(tab_height_for_text(42, 192), 58);
        let l = compute_layout_with_tab_height(client(800, 600), 96, None, None, 28);
        assert_eq!(l.tabs.bottom, 28);
        assert_eq!(l.editor.top, 28);
        let tiny = compute_layout_with_tab_height(client(10, 4), 96, None, None, 28);
        assert_eq!(tiny.tabs.bottom, 4);
    }

    #[test]
    fn splitter_ratio() {
        assert!((ratio_from_splitter_x(client(1000, 10), 600) - 0.4).abs() < 1e-6);
        assert_eq!(ratio_from_splitter_x(client(1000, 10), 0), 0.9);
        assert_eq!(ratio_from_splitter_x(client(1000, 10), 1000), 0.1);
    }
}
