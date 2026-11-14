//! 巨大ファイルの表示（DirectWrite で表示範囲だけを描く）。中核は [`crate::bigfile`]。
//! 折り返しと構文着色は既定でオフ。読み取り専用であることを画面で示す（ステータスバーの
//! 「読み取り専用」は [`BigViewStatus::read_only`] を見て `ui::app` が出す）。
//!
//! - 独自の子ウィンドウ。表示範囲の行だけを [`BigFile::rows_forward`] で取り、Direct2D／
//!   DirectWrite で描く。Direct2D を作れない環境（Wine など）では GDI で描く。
//! - 縦スクロールは先頭行のバイト位置で持つ。スクロールバーはファイル全体のバイト位置に
//!   比例させた推定（索引の作成中も使える）。行番号は索引が済んだ範囲で出す。
//! - 等幅の桁割り（[`logic`]）で位置を求める: 全角は 2 桁、タブは 4 桁。
//! - 長い行は `segment_bytes` 単位の表示行に分かれている。描画とキャッシュは表示中の
//!   行だけ（高々 画面の行数 + 1）。
//! - 検索はワーカースレッドで [`BigFile::find`] を呼び、結果は `WM_APP` のメッセージで UI へ
//!   戻す。世代番号が違う結果は捨てる。
//!
//! 呼び出し側（`ui::app`）は [`BigView`] の API を使い、[`BigViewEvent`] をコールバックで受ける。
//! コールバックは `Fn` で、呼ばれるとき内部の借用は解放済み（その中から API を呼んでよい）。
//!
//! 担当: タスク L。

pub mod logic;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, DeleteObject, EndPaint, GetTextExtentPoint32W, InvalidateRect,
    SelectObject, SetBkMode, SetTextColor, TextOutW, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, FF_DONTCARE, FIXED_PITCH, FW_NORMAL, HFONT, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
    TRANSPARENT,
};
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL, VK_DOWN, VK_END, VK_HOME,
    VK_LEFT, VK_NEXT, VK_PRIOR, VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetScrollInfo,
    GetWindowLongPtrW, LoadCursorW, MoveWindow, PostMessageW, RegisterClassExW, SetWindowLongPtrW,
    ShowWindow, CS_DBLCLKS, GWLP_USERDATA, HMENU, IDC_IBEAM, SB_BOTTOM, SB_ENDSCROLL, SB_HORZ,
    SB_LINEDOWN, SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP, SB_THUMBPOSITION, SB_THUMBTRACK, SB_TOP,
    SB_VERT, SCROLLINFO, SIF_ALL, SIF_TRACKPOS, SW_HIDE, SW_SHOW, WHEEL_DELTA, WM_APP, WM_CREATE,
    WM_ERASEBKGND, WM_GETDLGCODE, WM_HSCROLL, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_PAINT, WM_SETFOCUS, WM_SIZE,
    WM_VSCROLL, WNDCLASSEXW, WS_CHILD, WS_CLIPSIBLINGS, WS_EX_NOPARENTNOTIFY, WS_HSCROLL,
    WS_VISIBLE, WS_VSCROLL,
};

use crate::bigfile::{BigFile, Direction, IndexStatus, Row, SearchPattern, DEFAULT_COPY_MAX_BYTES};
use crate::core::{CancelToken, Error, Result, TaskContext};
use crate::document::{TextRange, TextSource};
use crate::platform::clipboard;
use crate::platform::system::{dpi_for_window, scale, BASE_DPI};
use crate::ui::theme::{Rgb, Theme};

use logic::*;

/// ウィンドウクラス名。
const CLASS: PCWSTR = w!("LinoteBigView");
/// 検索の結果がワーカーから届いた。
const WM_BIGVIEW_FOUND: u32 = WM_APP + 100;
/// 本文の書体の候補（先に見つかったものを使う）。
const FONT_CANDIDATES: [&str; 4] = ["Cascadia Mono", "Consolas", "MS Gothic", "Courier New"];
/// 本文の大きさ（ポイント）。
const FONT_POINTS: f32 = 11.0;
/// ホイール 1 ノッチで進む行数。
const WHEEL_LINES: usize = 3;
/// 描くときの余白（96 DPI の px）。
const PAD_X: i32 = 6;

// ------------------------------------------------------------------ 公開する型

/// ステータスバー向けの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BigViewStatus {
    /// 位置（カーソル、なければ先頭行）を含む行番号（1 始まり）。索引がまだなら `None`。
    pub line: Option<u64>,
    /// その位置のバイト数（先頭から）。
    pub offset: u64,
    pub file_len: u64,
    /// 選択しているバイト数。
    pub selection_bytes: u64,
    pub index: IndexStatus,
    /// 常に真（巨大ファイルは読み取り専用）。
    pub read_only: bool,
    pub searching: bool,
}

/// 通知を受けるコールバック。
pub type Callback = Rc<dyn Fn(&BigViewEvent)>;

/// ビューから呼び出し側への通知。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BigViewEvent {
    /// ステータスバーを更新する。
    Status(BigViewStatus),
    /// Ctrl+G が押された。行番号の入力欄を出して [`BigView::goto_line`] を呼ぶ。
    GotoLineRequested,
    /// 短い状態表示（「コピーしました」など）。
    Message(String),
    /// 読み込みの失敗（外部変更など）。
    Error(String),
    /// 検索が終わった。`found` が偽なら見つからなかった。`wrapped` は端で折り返した。
    Found { found: bool, wrapped: bool },
    /// 検索の開始（`true`）と終了（`false`）。
    SearchState { running: bool },
    /// ビューにフォーカスが入った。
    Focused,
}

/// [`BigView::goto_line`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GotoResult {
    Done,
    /// 行がない。確定した総行数。
    OutOfRange {
        total: u64,
    },
    /// 索引がその行まで済んでいない。
    IndexPending,
}

/// タブの切り替えで保存・復元する表示の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BigViewState {
    pub top: u64,
    pub x_cols: u32,
    pub anchor: u64,
    pub caret: u64,
}

/// 巨大ファイルのビュー（ウィンドウのハンドル。コピーして持ち回れる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BigView {
    hwnd: HWND,
}

// ------------------------------------------------------------------ 内部の状態

struct RowView {
    row: Row,
    text: String,
    line: Option<u64>,
}

struct FindDone {
    generation: u64,
    result: Result<(Option<TextRange>, bool)>,
}

struct Metrics {
    cell_w: f32,
    line_h: i32,
}

struct Inner {
    hwnd: HWND,
    source: Option<Arc<BigFile>>,
    theme: Theme,
    /// 行番号の欄を出すか（表示の設定の `line_numbers`）。
    line_numbers: bool,
    dpi: u32,
    force_gdi: bool,
    surface: Option<Surface>,
    surface_tried: bool,
    cell_w: f32,
    line_h: i32,
    top: u64,
    x_cols: u32,
    rows: Vec<RowView>,
    rows_dirty: bool,
    anchor: u64,
    caret: u64,
    dragging: bool,
    hit: Option<TextRange>,
    focused: bool,
    error: Option<String>,
    max_cols: u32,
    width: i32,
    height: i32,
    generation: u64,
    find_cancel: Option<CancelToken>,
    searching: bool,
    found: Arc<Mutex<Option<FindDone>>>,
    callback: Option<Callback>,
    events: Vec<BigViewEvent>,
    last_status: Option<BigViewStatus>,
}

struct Shell {
    inner: RefCell<Inner>,
}

// ------------------------------------------------------------------ 描画の面

/// 描画命令。座標は px。
trait Painter {
    fn fill(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgb);
    fn text(&mut self, x: f32, y: f32, text: &str, color: Rgb);
}

enum Surface {
    D2d(D2dSurface),
    Gdi(GdiSurface),
}

struct D2dSurface {
    target: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    format: IDWriteTextFormat,
    size: (u32, u32),
}

struct GdiSurface {
    font: HFONT,
}

impl Drop for GdiSurface {
    fn drop(&mut self) {
        // SAFETY: 自分で作った書体。
        unsafe {
            let _ = DeleteObject(self.font.into());
        }
    }
}

fn d2d_color(c: Rgb) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.0 as f32 / 255.0,
        g: c.1 as f32 / 255.0,
        b: c.2 as f32 / 255.0,
        a: 1.0,
    }
}

struct D2dPainter<'a>(&'a D2dSurface);

impl Painter for D2dPainter<'_> {
    fn fill(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgb) {
        // SAFETY: BeginDraw と EndDraw の間に呼ぶ。
        unsafe {
            self.0.brush.SetColor(&d2d_color(color));
            self.0.target.FillRectangle(
                &D2D_RECT_F {
                    left: x,
                    top: y,
                    right: x + w,
                    bottom: y + h,
                },
                &self.0.brush,
            );
        }
    }

    fn text(&mut self, x: f32, y: f32, text: &str, color: Rgb) {
        if text.is_empty() {
            return;
        }
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: 文字列は呼び出しの間有効。
        unsafe {
            self.0.brush.SetColor(&d2d_color(color));
            self.0.target.DrawText(
                &wide,
                &self.0.format,
                &D2D_RECT_F {
                    left: x,
                    top: y,
                    right: x + 100_000.0,
                    bottom: y + 1000.0,
                },
                &self.0.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
}

struct GdiPainter {
    hdc: windows::Win32::Graphics::Gdi::HDC,
}

impl Painter for GdiPainter {
    fn fill(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgb) {
        use windows::Win32::Graphics::Gdi::{CreateSolidBrush, FillRect};
        let r = RECT {
            left: x.round() as i32,
            top: y.round() as i32,
            right: (x + w).round() as i32,
            bottom: (y + h).round() as i32,
        };
        // SAFETY: 作ったブラシは消す。
        unsafe {
            let brush = CreateSolidBrush(COLORREF(color.colorref()));
            FillRect(self.hdc, &r, brush);
            let _ = DeleteObject(brush.into());
        }
    }

    fn text(&mut self, x: f32, y: f32, text: &str, color: Rgb) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: 書体は選択済み。
        unsafe {
            SetBkMode(self.hdc, TRANSPARENT);
            SetTextColor(self.hdc, COLORREF(color.colorref()));
            let _ = TextOutW(self.hdc, x.round() as i32, y.round() as i32, &wide);
        }
    }
}

fn pick_font(factory: &IDWriteFactory) -> &'static str {
    // SAFETY: 出力先はローカル変数。
    unsafe {
        let mut collection = None;
        if factory
            .GetSystemFontCollection(&mut collection, false)
            .is_ok()
        {
            if let Some(collection) = collection {
                for name in FONT_CANDIDATES {
                    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
                    let (mut index, mut exists) = (0u32, false.into());
                    if collection
                        .FindFamilyName(PCWSTR(wide.as_ptr()), &mut index, &mut exists)
                        .is_ok()
                        && exists.as_bool()
                    {
                        return name;
                    }
                }
            }
        }
    }
    "Consolas"
}

fn font_px(dpi: u32) -> f32 {
    FONT_POINTS * dpi as f32 / 72.0
}

/// Direct2D と DirectWrite の面を作る。作れなければ `None`。
fn create_d2d(hwnd: HWND, px: f32, size: (u32, u32)) -> Option<(D2dSurface, Metrics)> {
    // SAFETY: 作ったオブジェクトは COM の参照カウントで管理される。
    unsafe {
        let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
        let family = pick_font(&dw);
        let wide: Vec<u16> = family.encode_utf16().chain(Some(0)).collect();
        let format = dw
            .CreateTextFormat(
                PCWSTR(wide.as_ptr()),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                px,
                w!("ja-jp"),
            )
            .ok()?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP).ok()?;
        let sample: Vec<u16> = "MMMMMMMMMM".encode_utf16().collect();
        let layout = dw
            .CreateTextLayout(&sample, &format, 10_000.0, 1000.0)
            .ok()?;
        let mut m = Default::default();
        layout.GetMetrics(&mut m).ok()?;
        if m.width <= 0.0 || m.height <= 0.0 {
            return None;
        }
        let metrics = Metrics {
            cell_w: m.width / 10.0,
            line_h: m.height.ceil() as i32,
        };
        let props = D2D1_RENDER_TARGET_PROPERTIES::default();
        let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: D2D_SIZE_U {
                width: size.0.max(1),
                height: size.1.max(1),
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        let target = d2d.CreateHwndRenderTarget(&props, &hprops).ok()?;
        // 座標は px で扱う（DPI は書体の大きさで合わせる）。
        target.SetDpi(96.0, 96.0);
        let brush = target
            .CreateSolidColorBrush(&d2d_color(Rgb(0, 0, 0)), None)
            .ok()?;
        Some((
            D2dSurface {
                target,
                brush,
                format,
                size,
            },
            metrics,
        ))
    }
}

fn create_gdi(hwnd: HWND, px: f32) -> Option<(GdiSurface, Metrics)> {
    use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC};
    // SAFETY: 取った DC は返す。作った書体は GdiSurface が持つ。
    unsafe {
        let font = CreateFontW(
            -(px.round() as i32),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (FIXED_PITCH.0 | FF_DONTCARE.0) as u32,
            w!("Consolas"),
        );
        if font.is_invalid() {
            return None;
        }
        let hdc = GetDC(Some(hwnd));
        let old = SelectObject(hdc, font.into());
        let mut size = Default::default();
        let wide: Vec<u16> = "MMMMMMMMMM".encode_utf16().collect();
        let _ = GetTextExtentPoint32W(hdc, &wide, &mut size);
        SelectObject(hdc, old);
        ReleaseDC(Some(hwnd), hdc);
        if size.cx <= 0 || size.cy <= 0 {
            let _ = DeleteObject(font.into());
            return None;
        }
        Some((
            GdiSurface { font },
            Metrics {
                cell_w: size.cx as f32 / 10.0,
                line_h: size.cy,
            },
        ))
    }
}

// ------------------------------------------------------------------ Inner

impl Inner {
    fn new(hwnd: HWND) -> Self {
        Inner {
            hwnd,
            source: None,
            theme: Theme::dark(),
            line_numbers: true,
            dpi: dpi_for_window(hwnd),
            force_gdi: std::env::var_os("LINOTE_BIGVIEW_GDI").is_some(),
            surface: None,
            surface_tried: false,
            cell_w: 8.0,
            line_h: 16,
            top: 0,
            x_cols: 0,
            rows: Vec::new(),
            rows_dirty: true,
            anchor: 0,
            caret: 0,
            dragging: false,
            hit: None,
            focused: false,
            error: None,
            max_cols: 0,
            width: 0,
            height: 0,
            generation: 0,
            find_cancel: None,
            searching: false,
            found: Arc::new(Mutex::new(None)),
            callback: None,
            events: Vec::new(),
            last_status: None,
        }
    }

    fn metrics(&mut self) -> Metrics {
        self.ensure_surface();
        self.metrics_cached()
    }

    fn metrics_cached(&self) -> Metrics {
        Metrics {
            cell_w: self.cell_w,
            line_h: self.line_h,
        }
    }

    fn ensure_surface(&mut self) {
        if self.surface.is_some() || self.surface_tried {
            return;
        }
        self.surface_tried = true;
        self.read_client();
        let px = font_px(self.dpi);
        let size = (self.width.max(1) as u32, self.height.max(1) as u32);
        let made = if self.force_gdi {
            None
        } else {
            create_d2d(self.hwnd, px, size)
        };
        let (surface, metrics) = match made {
            Some((s, m)) => (Surface::D2d(s), m),
            None => match create_gdi(self.hwnd, px) {
                Some((s, m)) => (Surface::Gdi(s), m),
                None => {
                    return;
                }
            },
        };
        self.cell_w = metrics.cell_w;
        self.line_h = metrics.line_h;
        self.surface = Some(surface);
    }

    fn renderer(&self) -> &'static str {
        match self.surface {
            Some(Surface::D2d(_)) => "Direct2D",
            Some(Surface::Gdi(_)) => "GDI",
            None => "none",
        }
    }

    fn read_client(&mut self) {
        let mut r = RECT::default();
        // SAFETY: 出力先はローカル変数。
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        self.width = r.right - r.left;
        self.height = r.bottom - r.top;
    }

    fn gutter_w(&self, m: &Metrics) -> f32 {
        if !self.line_numbers {
            // 行番号なし: 本文の左の余白だけ。
            return scale(PAD_X, self.dpi) as f32;
        }
        let digits = match &self.source {
            Some(s) => gutter_digits(s.index_status().lines.max(1)),
            None => 4,
        };
        (digits as f32 + 1.0) * m.cell_w + scale(PAD_X, self.dpi) as f32
    }

    fn fit(&mut self) -> usize {
        self.read_client();
        let m = self.metrics();
        rows_that_fit(self.height, m.line_h)
    }

    fn view_cols(&mut self) -> u32 {
        self.read_client();
        let m = self.metrics();
        let text_w = self.width as f32 - self.gutter_w(&m);
        (text_w / m.cell_w).floor().max(1.0) as u32
    }

    fn emit(&mut self, e: BigViewEvent) {
        self.events.push(e);
    }

    fn fail(&mut self, e: &Error) {
        let msg = e.to_string();
        self.error = Some(msg.clone());
        self.rows.clear();
        self.emit(BigViewEvent::Error(msg));
    }

    /// 表示する行を読み直す。
    fn reload(&mut self) {
        self.rows_dirty = false;
        self.rows.clear();
        let Some(src) = self.source.clone() else {
            return;
        };
        let fit = self.fit();
        let mut attempt = 0;
        loop {
            match self.load_rows(&src, fit + 1) {
                Ok(rows) => {
                    self.error = None;
                    // 末尾を越えてスクロールしたら、画面いっぱいになるまで戻す。
                    let reached_end = rows.last().is_some_and(|r| r.row.last);
                    if reached_end && rows.len() < fit && self.top > 0 && attempt == 0 {
                        if let Ok(back) = src.rows_backward(self.top, fit - rows.len()) {
                            if let Some(first) = back.first() {
                                self.top = first.start;
                                attempt += 1;
                                continue;
                            }
                        }
                    }
                    self.rows = rows;
                }
                Err(e) => self.fail(&e),
            }
            break;
        }
        let cols = self
            .rows
            .iter()
            .map(|r| total_cols(&r.text))
            .max()
            .unwrap_or(0);
        self.max_cols = self.max_cols.max(cols);
        self.update_scrollbars();
        self.emit_status();
    }

    fn load_rows(&self, src: &BigFile, n: usize) -> Result<Vec<RowView>> {
        let rows = src.rows_forward(self.top, n)?;
        let mut line = match rows.first() {
            Some(r) => src.line_of_offset(r.start)?,
            None => None,
        };
        let mut out = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            if i > 0 && row.starts_line {
                line = line.map(|l| l + 1);
            }
            let text = src.row_text(row)?;
            out.push(RowView {
                row: *row,
                text,
                line: if row.starts_line { line } else { None },
            });
        }
        Ok(out)
    }

    fn selection(&self) -> Option<TextRange> {
        let (lo, hi) = normalize(self.anchor, self.caret);
        (lo < hi).then(|| TextRange::new(lo, hi))
    }

    fn status(&self) -> Option<BigViewStatus> {
        let src = self.source.as_ref()?;
        let offset = self.caret_or_top();
        let index = src.index_status();
        let line = src.line_of_offset(offset).ok().flatten().map(|l| l + 1);
        Some(BigViewStatus {
            line,
            offset,
            file_len: src.len(),
            selection_bytes: self.selection().map_or(0, |r| r.end - r.start),
            index,
            read_only: true,
            searching: self.searching,
        })
    }

    fn caret_or_top(&self) -> u64 {
        if self.selection().is_some() || self.caret != 0 || self.anchor != 0 {
            self.caret
        } else {
            self.top
        }
    }

    fn emit_status(&mut self) {
        let s = self.status();
        if s != self.last_status {
            self.last_status = s;
            if let Some(s) = s {
                self.emit(BigViewEvent::Status(s));
            }
        }
    }

    fn update_scrollbars(&mut self) {
        let len = self.source.as_ref().map_or(0, |s| s.len());
        let visible: u64 = self.rows.iter().map(|r| r.row.len().max(1)).sum();
        let v = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_ALL,
            nMin: 0,
            nMax: SCROLL_RANGE,
            nPage: scroll_page(visible, len),
            nPos: scroll_pos(self.top, len),
            nTrackPos: 0,
        };
        let view_cols = self.view_cols();
        let hmax = hscroll_max(self.max_cols, view_cols);
        self.x_cols = self.x_cols.min(hmax);
        let h = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_ALL,
            nMin: 0,
            nMax: (hmax + view_cols.saturating_sub(1).max(1)) as i32,
            nPage: view_cols.saturating_sub(1).max(1),
            nPos: self.x_cols as i32,
            nTrackPos: 0,
        };
        // SAFETY: 自分のウィンドウ。
        unsafe {
            SetScrollInfo(self.hwnd, SB_VERT, &v, true);
            SetScrollInfo(self.hwnd, SB_HORZ, &h, true);
        }
    }

    fn invalidate(&self) {
        // SAFETY: 自分のウィンドウ。
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn refresh(&mut self) {
        self.rows_dirty = true;
        self.reload();
        self.invalidate();
    }

    // ---- スクロール

    fn set_top(&mut self, offset: u64) {
        let Some(src) = self.source.clone() else {
            return;
        };
        match src.row_at(offset.min(src.len())) {
            Ok(r) => self.top = r.start,
            Err(e) => return self.fail(&e),
        }
        self.refresh();
    }

    fn scroll_rows(&mut self, delta: isize) {
        let Some(src) = self.source.clone() else {
            return;
        };
        let n = delta.unsigned_abs();
        let target = if delta > 0 {
            match src.rows_forward(self.top, n + 1) {
                Ok(rows) => rows.get(n).map(|r| r.start),
                Err(e) => return self.fail(&e),
            }
        } else {
            match src.rows_backward(self.top, n) {
                Ok(rows) => rows.first().map(|r| r.start),
                Err(e) => return self.fail(&e),
            }
        };
        if let Some(t) = target {
            self.top = t;
            self.refresh();
        }
    }

    fn scroll_to_end(&mut self) {
        let fit = self.fit();
        let Some(src) = self.source.clone() else {
            return;
        };
        match src.last_rows(fit) {
            Ok(rows) => {
                self.top = rows.first().map_or(0, |r| r.start);
                self.refresh();
            }
            Err(e) => self.fail(&e),
        }
    }

    fn scroll_x(&mut self, cols: i64) {
        let max = hscroll_max(self.max_cols, self.view_cols());
        self.x_cols = (self.x_cols as i64 + cols).clamp(0, max as i64) as u32;
        self.update_scrollbars();
        self.invalidate();
    }

    fn goto_offset(&mut self, offset: u64, select_row: bool) {
        let Some(src) = self.source.clone() else {
            return;
        };
        let fit = self.fit();
        let offset = offset.min(src.len());
        let row = match src.row_at(offset) {
            Ok(r) => r,
            Err(e) => return self.fail(&e),
        };
        // 見つけた位置が上寄りにならないよう、少し手前から表示する。
        self.top = match src.rows_backward(row.start, fit / 3) {
            Ok(back) => back.first().map_or(row.start, |r| r.start),
            Err(_) => row.start,
        };
        if select_row {
            self.anchor = row.start;
            self.caret = row.start;
        }
        self.x_cols = 0;
        self.refresh();
    }

    fn goto_line(&mut self, line: u64) -> GotoResult {
        let Some(src) = self.source.clone() else {
            return GotoResult::IndexPending;
        };
        if line == 0 {
            return GotoResult::OutOfRange { total: 0 };
        }
        match src.line_start(line - 1) {
            Ok(Some(off)) => {
                self.hit = None;
                self.goto_offset(off, true);
                GotoResult::Done
            }
            Ok(None) => {
                let idx = src.index_status();
                if idx.complete {
                    GotoResult::OutOfRange { total: idx.lines }
                } else {
                    GotoResult::IndexPending
                }
            }
            Err(e) => {
                self.fail(&e);
                GotoResult::IndexPending
            }
        }
    }

    // ---- 選択とコピー

    fn select_all(&mut self) {
        let len = self.source.as_ref().map_or(0, |s| s.len());
        self.anchor = 0;
        self.caret = len;
        self.emit_status();
        self.invalidate();
    }

    fn selected_text(&self) -> Result<String> {
        let Some(src) = &self.source else {
            return Ok(String::new());
        };
        let Some(range) = self.selection() else {
            return Ok(String::new());
        };
        let (lo, hi) = copy_range(range.start, range.end, DEFAULT_COPY_MAX_BYTES).ok_or(
            Error::LimitExceeded {
                what: "コピーする範囲".to_owned(),
                limit: DEFAULT_COPY_MAX_BYTES,
            },
        )?;
        src.read_text(TextRange::new(lo, hi), DEFAULT_COPY_MAX_BYTES)
    }

    fn copy(&mut self) -> Result<usize> {
        let text = self.selected_text()?;
        if text.is_empty() {
            return Ok(0);
        }
        clipboard::write_text(Some(self.hwnd), &text)?;
        let n = text.len();
        self.emit(BigViewEvent::Message(format!(
            "{} バイトをコピーしました",
            n
        )));
        Ok(n)
    }

    fn copy_with_message(&mut self) {
        match self.selection() {
            None => self.emit(BigViewEvent::Message("選択範囲がありません".into())),
            Some(_) => {
                if let Err(e) = self.copy() {
                    let msg = match e {
                        Error::LimitExceeded { limit, .. } => format!(
                            "選択範囲が大きすぎてコピーできません（上限 {} MiB）",
                            limit / (1024 * 1024)
                        ),
                        other => format!("コピーできませんでした: {other}"),
                    };
                    self.emit(BigViewEvent::Message(msg));
                }
            }
        }
    }

    /// 画面の位置から、ファイルのバイト位置を求める。
    fn offset_at(&mut self, x: i32, y: i32) -> Option<u64> {
        let m = self.metrics();
        if self.rows.is_empty() {
            return None;
        }
        let i = (y.max(0) / m.line_h.max(1)) as usize;
        let i = i.min(self.rows.len() - 1);
        let rv = &self.rows[i];
        let col = (x as f32 - self.gutter_w(&m)) / m.cell_w + self.x_cols as f32;
        let byte = byte_at_col(&rv.text, col);
        Some((rv.row.start + byte as u64).min(rv.row.end))
    }

    // ---- 検索

    fn find(&mut self, pattern: Arc<SearchPattern>, direction: Direction) -> bool {
        let Some(src) = self.source.clone() else {
            return false;
        };
        if let Some(t) = self.find_cancel.take() {
            t.cancel();
        }
        self.generation += 1;
        let generation = self.generation;
        let cancel = CancelToken::new();
        self.find_cancel = Some(cancel.clone());
        let from = match (direction, self.hit.or(self.selection())) {
            (Direction::Forward, Some(r)) => {
                if r.end > r.start {
                    r.end
                } else {
                    (r.end + 1).min(src.len())
                }
            }
            (Direction::Backward, Some(r)) => r.start,
            (Direction::Forward, None) => self.top,
            (Direction::Backward, None) => self.top,
        };
        self.searching = true;
        self.emit(BigViewEvent::SearchState { running: true });
        self.emit_status();
        let found = Arc::clone(&self.found);
        let hwnd = self.hwnd.0 as isize;
        crate::core::WorkerPool::global().spawn(move || {
            let mut ctx = TaskContext::detached();
            ctx.cancel = cancel;
            let result = search_with_wrap(&src, &pattern, from, direction, &ctx);
            if let Ok(mut slot) = found.lock() {
                *slot = Some(FindDone { generation, result });
            }
            // SAFETY: ウィンドウが無ければ失敗するだけ。
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(hwnd as *mut _)),
                    WM_BIGVIEW_FOUND,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        });
        true
    }

    fn on_found(&mut self) {
        let done = self.found.lock().ok().and_then(|mut s| s.take());
        let Some(done) = done else { return };
        if done.generation != self.generation {
            return; // 古い検索の結果
        }
        self.searching = false;
        self.find_cancel = None;
        self.emit(BigViewEvent::SearchState { running: false });
        match done.result {
            Ok((Some(range), wrapped)) => {
                self.hit = Some(range);
                self.anchor = range.start;
                self.caret = range.end;
                self.goto_offset(range.start, false);
                self.emit(BigViewEvent::Found {
                    found: true,
                    wrapped,
                });
            }
            Ok((None, _)) => {
                self.emit(BigViewEvent::Found {
                    found: false,
                    wrapped: false,
                });
                self.emit_status();
            }
            Err(Error::Cancelled) => self.emit_status(),
            Err(e) => self.fail(&e),
        }
        self.invalidate();
    }

    fn cancel_find(&mut self) {
        if let Some(t) = self.find_cancel.take() {
            t.cancel();
        }
        self.generation += 1;
        if self.searching {
            self.searching = false;
            self.emit(BigViewEvent::SearchState { running: false });
        }
    }

    // ---- 描画

    fn paint(&mut self) {
        // SAFETY: BeginPaint と EndPaint を対にする。
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        self.ensure_surface();
        if self.rows_dirty {
            self.reload();
        }
        let m = self.metrics_cached();
        let mut dropped = false;
        match self.surface.take() {
            Some(Surface::D2d(mut s)) => {
                self.read_client();
                let want = (self.width.max(1) as u32, self.height.max(1) as u32);
                // SAFETY: 描画対象は自分のウィンドウ。
                unsafe {
                    if s.size != want {
                        let _ = s.target.Resize(&D2D_SIZE_U {
                            width: want.0,
                            height: want.1,
                        });
                        s.size = want;
                    }
                    s.target.BeginDraw();
                    s.target.Clear(Some(&d2d_color(self.theme.editor.bg)));
                    self.draw_scene(&mut D2dPainter(&s), &m);
                    if s.target.EndDraw(None, None).is_err() {
                        dropped = true; // 作り直し（D2DERR_RECREATE_TARGET など）
                    }
                }
                if !dropped {
                    self.surface = Some(Surface::D2d(s));
                }
            }
            Some(Surface::Gdi(s)) => {
                self.read_client();
                let area = RECT {
                    left: 0,
                    top: 0,
                    right: self.width,
                    bottom: self.height,
                };
                if let Some(buf) = crate::ui::window::BufferedPaint::new(hdc, area) {
                    let canvas = buf.canvas(self.dpi);
                    canvas.fill(&buf.local_rect(), self.theme.editor.bg);
                    // SAFETY: 書体は GdiSurface が持つ。
                    unsafe {
                        SelectObject(canvas.hdc, s.font.into());
                    }
                    self.draw_scene(&mut GdiPainter { hdc: canvas.hdc }, &m);
                }
                self.surface = Some(Surface::Gdi(s));
            }
            None => {}
        }
        if dropped {
            self.surface_tried = false;
        }
        // SAFETY: BeginPaint と対。
        unsafe {
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    fn draw_scene(&self, p: &mut dyn Painter, m: &Metrics) {
        let ed = self.theme.editor;
        let gutter = self.gutter_w(m);
        let lh = m.line_h as f32;
        let pad = scale(PAD_X, self.dpi) as f32;
        let sel = self.selection();
        let sel_bg = if self.focused {
            ed.selection_bg
        } else {
            ed.inactive_selection_bg
        };
        let hit_bg = ed.bg.mix(self.theme.ui.accent, 0.5);
        let origin = gutter - self.x_cols as f32 * m.cell_w;
        for (i, rv) in self.rows.iter().enumerate() {
            let y = i as f32 * lh;
            let row = &rv.row;
            if sel.is_none() && row.contains(self.caret) && self.caret != 0 {
                p.fill(gutter, y, self.width as f32, lh, ed.caret_line_bg);
            }
            for (range, bg) in [(sel, sel_bg), (self.hit, hit_bg)] {
                let Some(r) = range else { continue };
                if let Some((a, b, nl)) = selection_in_row(row.start, row.end, r.start, r.end) {
                    let x0 = origin + col_of_byte(&rv.text, a) as f32 * m.cell_w;
                    let mut x1 = origin + col_of_byte(&rv.text, b) as f32 * m.cell_w;
                    if nl {
                        x1 += m.cell_w; // 改行を含む印
                    }
                    p.fill(x0, y, x1 - x0, lh, bg);
                }
            }
            let v = visible_part(&rv.text, self.x_cols, self.view_cols_cached(m));
            p.text(origin + v.col0 as f32 * m.cell_w, y, &v.text, ed.fg);
        }
        // 行番号欄は本文の左端のはみ出しを隠して描く。
        if !self.line_numbers {
            return self.draw_notices(p, gutter, pad);
        }
        p.fill(0.0, 0.0, gutter, self.height as f32, ed.line_number_bg);
        for (i, rv) in self.rows.iter().enumerate() {
            let y = i as f32 * lh;
            let label = match (rv.line, rv.row.starts_line) {
                (Some(n), true) => (n + 1).to_string(),
                (None, true) => "…".to_owned(), // 索引がまだ
                _ => String::new(),
            };
            let w = label.chars().count() as f32 * m.cell_w;
            p.text(
                gutter - pad - m.cell_w * 0.5 - w,
                y,
                &label,
                ed.line_number_fg,
            );
        }
        self.draw_notices(p, gutter, pad);
    }

    /// エラーや「ファイルがありません」の表示。
    fn draw_notices(&self, p: &mut dyn Painter, gutter: f32, pad: f32) {
        if let Some(msg) = &self.error {
            p.text(gutter + pad, 0.0, msg, self.theme.ui.status_warn);
        } else if self.source.is_none() {
            p.text(
                gutter + pad,
                0.0,
                "（ファイルがありません）",
                self.theme.editor.line_number_fg,
            );
        }
    }

    fn view_cols_cached(&self, m: &Metrics) -> u32 {
        ((self.width as f32 - self.gutter_w(m)) / m.cell_w)
            .ceil()
            .max(1.0) as u32
            + 1
    }
}

/// 見つからなければ反対の端から（折り返して）もう一度探す。
fn search_with_wrap(
    src: &BigFile,
    pattern: &SearchPattern,
    from: u64,
    direction: Direction,
    ctx: &TaskContext,
) -> Result<(Option<TextRange>, bool)> {
    let first = src.find(pattern, from, direction, ctx)?;
    if first.value.hit.is_some() || !first.completion.is_complete() {
        return Ok((first.value.hit, false));
    }
    let restart = match direction {
        Direction::Forward => 0,
        Direction::Backward => src.len(),
    };
    if from == restart {
        return Ok((None, false));
    }
    let second = src.find(pattern, restart, direction, ctx)?;
    Ok((second.value.hit, second.value.hit.is_some()))
}

// ------------------------------------------------------------------ ウィンドウ

fn with_state<R>(hwnd: HWND, f: impl FnOnce(&mut Inner) -> R) -> Option<R> {
    // SAFETY: GWLP_USERDATA には WM_CREATE で作った Shell へのポインタを入れ、
    // WM_NCDESTROY で外して解放する。UI スレッドだけが触る。
    let shell = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Shell).as_ref()? };
    let result = {
        let mut inner = shell.inner.try_borrow_mut().ok()?; // 入れ子の呼び出しは無視
        f(&mut inner)
    };
    // 借用を解放してから通知する（コールバックから API を呼べるように）。
    let (events, callback) = {
        let mut inner = shell.inner.try_borrow_mut().ok()?;
        (std::mem::take(&mut inner.events), inner.callback.clone())
    };
    if let Some(cb) = callback {
        for e in &events {
            cb(e);
        }
    }
    Some(result)
}

fn key_down(vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
    // SAFETY: 状態の読み取りだけ。
    unsafe { GetAsyncKeyState(vk.0 as i32) < 0 }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let shell = Box::new(Shell {
                inner: RefCell::new(Inner::new(hwnd)),
            });
            // SAFETY: WM_NCDESTROY で解放する。
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(shell) as isize) };
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // SAFETY: WM_CREATE で入れたポインタを外して解放する。
            unsafe {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Shell;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if !ptr.is_null() {
                    if let Some(t) = (*ptr).inner.borrow().find_cancel.as_ref() {
                        t.cancel();
                    }
                    drop(Box::from_raw(ptr));
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_GETDLGCODE => LRESULT(0x0001 | 0x0080), // DLGC_WANTARROWS | DLGC_WANTCHARS
        WM_PAINT => {
            if with_state(hwnd, |s| s.paint()).is_none() {
                // SAFETY: 既定の処理。
                return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
            }
            LRESULT(0)
        }
        WM_SIZE => {
            with_state(hwnd, |s| {
                s.rows_dirty = true;
                s.invalidate();
            });
            LRESULT(0)
        }
        WM_SETFOCUS => {
            with_state(hwnd, |s| {
                s.focused = true;
                s.emit(BigViewEvent::Focused);
                s.invalidate();
            });
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            with_state(hwnd, |s| {
                s.focused = false;
                s.invalidate();
            });
            LRESULT(0)
        }
        WM_VSCROLL => {
            with_state(hwnd, |s| on_vscroll(s, wp.0 as u32 & 0xffff));
            LRESULT(0)
        }
        WM_HSCROLL => {
            with_state(hwnd, |s| on_hscroll(s, wp.0 as u32 & 0xffff));
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xffff) as u16 as i16 as i32;
            let notches = delta as f32 / WHEEL_DELTA as f32;
            let lines = (notches * WHEEL_LINES as f32).round() as isize;
            with_state(hwnd, |s| {
                if key_down(VK_SHIFT) {
                    s.scroll_x(-(lines as i64) * 4);
                } else {
                    s.scroll_rows(-lines);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            // SAFETY: フォーカスとマウスの捕捉。
            unsafe {
                let _ = SetFocus(Some(hwnd));
                SetCapture(hwnd);
            }
            let (x, y) = mouse_pos(lp);
            with_state(hwnd, |s| {
                if let Some(off) = s.offset_at(x, y) {
                    if !key_down(VK_SHIFT) {
                        s.anchor = off;
                    }
                    s.caret = off;
                    s.hit = None;
                    s.dragging = true;
                    s.emit_status();
                    s.invalidate();
                }
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = mouse_pos(lp);
            with_state(hwnd, |s| {
                if !s.dragging {
                    return;
                }
                let lh = s.metrics().line_h;
                if y < 0 {
                    s.scroll_rows(-1);
                } else if y >= s.height && lh > 0 {
                    s.scroll_rows(1);
                }
                if let Some(off) = s.offset_at(x, y) {
                    if off != s.caret {
                        s.caret = off;
                        s.emit_status();
                        s.invalidate();
                    }
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            with_state(hwnd, |s| s.dragging = false);
            // SAFETY: 捕捉の解除。
            unsafe {
                let _ = ReleaseCapture();
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = key_down(VK_CONTROL);
            let vk = wp.0 as u16;
            let handled = with_state(hwnd, |s| on_key(s, vk, ctrl)).unwrap_or(false);
            if handled {
                LRESULT(0)
            } else {
                // SAFETY: 既定の処理。
                unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
            }
        }
        WM_BIGVIEW_FOUND => {
            with_state(hwnd, |s| s.on_found());
            LRESULT(0)
        }
        // SAFETY: 既定の処理。
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn mouse_pos(lp: LPARAM) -> (i32, i32) {
    (
        (lp.0 & 0xffff) as u16 as i16 as i32,
        ((lp.0 >> 16) & 0xffff) as u16 as i16 as i32,
    )
}

fn on_vscroll(s: &mut Inner, code: u32) {
    let fit = s.fit() as isize;
    match code {
        c if c == SB_LINEUP.0 as u32 => s.scroll_rows(-1),
        c if c == SB_LINEDOWN.0 as u32 => s.scroll_rows(1),
        c if c == SB_PAGEUP.0 as u32 => s.scroll_rows(-(fit - 1).max(1)),
        c if c == SB_PAGEDOWN.0 as u32 => s.scroll_rows((fit - 1).max(1)),
        c if c == SB_TOP.0 as u32 => s.set_top(0),
        c if c == SB_BOTTOM.0 as u32 => s.scroll_to_end(),
        c if c == SB_THUMBTRACK.0 as u32 || c == SB_THUMBPOSITION.0 as u32 => {
            let mut info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_TRACKPOS,
                ..Default::default()
            };
            // SAFETY: 出力先はローカル変数。
            let ok = unsafe { GetScrollInfo(s.hwnd, SB_VERT, &mut info) }.is_ok();
            if ok {
                let len = s.source.as_ref().map_or(0, |x| x.len());
                s.set_top(offset_from_scroll(info.nTrackPos, len));
            }
        }
        c if c == SB_ENDSCROLL.0 as u32 => {}
        _ => {}
    }
}

fn on_hscroll(s: &mut Inner, code: u32) {
    let page = s.view_cols().saturating_sub(1).max(1) as i64;
    match code {
        c if c == SB_LINEUP.0 as u32 => s.scroll_x(-4),
        c if c == SB_LINEDOWN.0 as u32 => s.scroll_x(4),
        c if c == SB_PAGEUP.0 as u32 => s.scroll_x(-page),
        c if c == SB_PAGEDOWN.0 as u32 => s.scroll_x(page),
        c if c == SB_THUMBTRACK.0 as u32 || c == SB_THUMBPOSITION.0 as u32 => {
            let mut info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_TRACKPOS,
                ..Default::default()
            };
            // SAFETY: 出力先はローカル変数。
            if unsafe { GetScrollInfo(s.hwnd, SB_HORZ, &mut info) }.is_ok() {
                let delta = info.nTrackPos as i64 - s.x_cols as i64;
                s.scroll_x(delta);
            }
        }
        _ => {}
    }
}

fn on_key(s: &mut Inner, vk: u16, ctrl: bool) -> bool {
    let fit = s.fit() as isize;
    match vk {
        v if v == b'C' as u16 && ctrl => s.copy_with_message(),
        v if v == b'A' as u16 && ctrl => s.select_all(),
        v if v == b'G' as u16 && ctrl => s.emit(BigViewEvent::GotoLineRequested),
        v if v == VK_UP.0 => s.scroll_rows(-1),
        v if v == VK_DOWN.0 => s.scroll_rows(1),
        v if v == VK_PRIOR.0 => s.scroll_rows(-(fit - 1).max(1)),
        v if v == VK_NEXT.0 => s.scroll_rows((fit - 1).max(1)),
        v if v == VK_LEFT.0 => s.scroll_x(-4),
        v if v == VK_RIGHT.0 => s.scroll_x(4),
        v if v == VK_HOME.0 => {
            if ctrl {
                s.set_top(0);
            }
            s.scroll_x(i64::MIN / 2);
        }
        v if v == VK_END.0 => {
            if ctrl {
                s.scroll_to_end();
            }
        }
        _ => return false,
    }
    true
}

fn register_class(instance: HINSTANCE) -> Result<()> {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    let ok = *REGISTERED.get_or_init(|| {
        // SAFETY: クラス名とウィンドウプロシージャは静的。
        unsafe {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_DBLCLKS,
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_IBEAM).unwrap_or_default(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc) != 0
        }
    });
    if ok {
        Ok(())
    } else {
        Err(Error::unavailable(
            "巨大ファイルのビュー",
            "ウィンドウクラスを登録できませんでした",
        ))
    }
}

// ------------------------------------------------------------------ 公開 API

impl BigView {
    /// 子ウィンドウとして作る。`id` は `WM_COMMAND`／`GWLP_ID` 用。
    pub fn create(parent: HWND, instance: HINSTANCE, id: isize, visible: bool) -> Result<BigView> {
        register_class(instance)?;
        let mut style = WS_CHILD | WS_CLIPSIBLINGS | WS_VSCROLL | WS_HSCROLL;
        if visible {
            style |= WS_VISIBLE;
        }
        // SAFETY: 親は有効なウィンドウ。
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOPARENTNOTIFY,
                CLASS,
                PCWSTR::null(),
                style,
                0,
                0,
                100,
                100,
                Some(parent),
                Some(HMENU(id as *mut _)),
                Some(instance),
                None,
            )
        }
        .map_err(|e| Error::unavailable("巨大ファイルのビュー", e.to_string()))?;
        Ok(BigView { hwnd })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn destroy(self) {
        // SAFETY: 自分のウィンドウ。
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    pub fn show(&self, visible: bool) {
        // SAFETY: 自分のウィンドウ。
        unsafe {
            let _ = ShowWindow(self.hwnd, if visible { SW_SHOW } else { SW_HIDE });
        }
    }

    pub fn move_to(&self, r: RECT) {
        // SAFETY: 自分のウィンドウ。
        unsafe {
            let _ = MoveWindow(
                self.hwnd,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                true,
            );
        }
    }

    pub fn focus(&self) {
        // SAFETY: 自分のウィンドウ。
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
    }

    /// 通知を受けるコールバック（置き換え）。
    pub fn set_callback(&self, callback: Callback) {
        with_state(self.hwnd, |s| s.callback = Some(callback));
    }

    /// 表示するファイルを替える（`None` で空にする）。表示位置・選択・検索結果は初期化。
    pub fn set_source(&self, source: Option<Arc<BigFile>>) {
        with_state(self.hwnd, |s| {
            s.cancel_find();
            s.source = source;
            s.top = 0;
            s.x_cols = 0;
            s.anchor = 0;
            s.caret = 0;
            s.hit = None;
            s.max_cols = 0;
            s.error = None;
            s.last_status = None;
            s.refresh();
        });
    }

    /// 行番号の欄を出す・隠す（欄の幅が変わるので表示を作り直す）。
    pub fn set_line_numbers(&self, show: bool) {
        with_state(self.hwnd, |s| {
            if s.line_numbers != show {
                s.line_numbers = show;
                s.refresh();
            }
        });
    }

    pub fn set_theme(&self, theme: &Theme) {
        with_state(self.hwnd, |s| {
            s.theme = theme.clone();
            s.invalidate();
        });
    }

    /// DPI が変わったとき（`WM_DPICHANGED`）。書体を作り直す。
    pub fn set_dpi(&self, dpi: u32) {
        with_state(self.hwnd, |s| {
            let dpi = if dpi == 0 { BASE_DPI } else { dpi };
            if s.dpi != dpi {
                s.dpi = dpi;
                s.surface = None;
                s.surface_tried = false;
                s.refresh();
            }
        });
    }

    /// Direct2D を使わず GDI で描く（テストと、Direct2D が不調な環境向け）。
    pub fn set_force_gdi(&self, force: bool) {
        with_state(self.hwnd, |s| {
            s.force_gdi = force;
            s.surface = None;
            s.surface_tried = false;
            s.refresh();
        });
    }

    /// 行番号の欄の幅（ピクセル。行番号を隠していれば左の余白だけ）。試験用。
    pub fn gutter_width(&self) -> i32 {
        with_state(self.hwnd, |s| {
            let m = s.metrics_cached();
            s.gutter_w(&m) as i32
        })
        .unwrap_or(0)
    }

    /// 使っている描画方式（`"Direct2D"`、`"GDI"`）。
    pub fn renderer(&self) -> &'static str {
        with_state(self.hwnd, |s| {
            s.ensure_surface();
            s.renderer()
        })
        .unwrap_or("none")
    }

    /// 1 行ぶんの高さ（px）。
    pub fn line_height(&self) -> i32 {
        with_state(self.hwnd, |s| s.metrics().line_h).unwrap_or(16)
    }

    /// 索引の進みやファイルの変化を反映する（タイマーで呼ぶ）。
    pub fn refresh(&self) {
        with_state(self.hwnd, |s| s.refresh());
    }

    /// 行番号（1 始まり）へ移る。
    pub fn goto_line(&self, line: u64) -> GotoResult {
        with_state(self.hwnd, |s| s.goto_line(line)).unwrap_or(GotoResult::IndexPending)
    }

    /// バイト位置へ移る（選択は変えない）。
    pub fn goto_offset(&self, offset: u64) {
        with_state(self.hwnd, |s| s.goto_offset(offset, false));
    }

    pub fn scroll_to_start(&self) {
        with_state(self.hwnd, |s| s.set_top(0));
    }

    /// 末尾へ（Ctrl+End）。
    pub fn scroll_to_end(&self) {
        with_state(self.hwnd, |s| s.scroll_to_end());
    }

    pub fn scroll_rows(&self, delta: isize) {
        with_state(self.hwnd, |s| s.scroll_rows(delta));
    }

    /// 先頭行のバイト位置。
    pub fn top_offset(&self) -> u64 {
        with_state(self.hwnd, |s| s.top).unwrap_or(0)
    }

    /// 今見えている行（先頭から）の本文と行番号（1 始まり。索引がまだなら `None`）。
    pub fn visible_lines(&self) -> Vec<(Option<u64>, String)> {
        with_state(self.hwnd, |s| {
            if s.rows_dirty {
                s.reload();
            }
            s.rows
                .iter()
                .map(|r| (r.line.map(|l| l + 1), r.text.clone()))
                .collect()
        })
        .unwrap_or_default()
    }

    pub fn select_all(&self) {
        with_state(self.hwnd, |s| s.select_all());
    }

    /// 選択範囲（バイト位置）。
    pub fn selection(&self) -> Option<TextRange> {
        with_state(self.hwnd, |s| s.selection()).flatten()
    }

    pub fn set_selection(&self, range: TextRange) {
        with_state(self.hwnd, |s| {
            s.anchor = range.start;
            s.caret = range.end;
            s.emit_status();
            s.invalidate();
        });
    }

    /// 選択範囲の文字列（上限 [`DEFAULT_COPY_MAX_BYTES`]）。
    pub fn selected_text(&self) -> Result<String> {
        with_state(self.hwnd, |s| s.selected_text()).unwrap_or_else(|| Ok(String::new()))
    }

    /// 選択範囲をクリップボードへ。コピーしたバイト数を返す。
    pub fn copy_selection(&self) -> Result<usize> {
        with_state(self.hwnd, |s| s.copy()).unwrap_or(Ok(0))
    }

    /// 検索ヒット（強調している範囲）。
    pub fn hit(&self) -> Option<TextRange> {
        with_state(self.hwnd, |s| s.hit).flatten()
    }

    /// 次（前）の一致を探す。ワーカーで探し、結果は [`BigViewEvent::Found`] で届く。
    /// 現在のヒット（なければ選択、なければ先頭行）から探し、端に着いたら反対側から探す。
    /// 進行中の検索は中止する。
    pub fn find(&self, pattern: Arc<SearchPattern>, direction: Direction) -> bool {
        with_state(self.hwnd, |s| s.find(pattern, direction)).unwrap_or(false)
    }

    pub fn cancel_find(&self) {
        with_state(self.hwnd, |s| s.cancel_find());
    }

    pub fn status(&self) -> Option<BigViewStatus> {
        with_state(self.hwnd, |s| s.status()).flatten()
    }

    pub fn state(&self) -> BigViewState {
        with_state(self.hwnd, |s| BigViewState {
            top: s.top,
            x_cols: s.x_cols,
            anchor: s.anchor,
            caret: s.caret,
        })
        .unwrap_or_default()
    }

    /// タブの切り替えで戻す。
    pub fn restore_state(&self, st: BigViewState) {
        with_state(self.hwnd, |s| {
            s.anchor = st.anchor;
            s.caret = st.caret;
            s.x_cols = st.x_cols;
            s.set_top(st.top);
        });
    }
}
