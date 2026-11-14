//! メモの小窓（一覧、名前・本文での検索、選択して開く）。中核は [`crate::memo`]。
//! 一覧は初めて開くときに読み込む。閉じたら本文にフォーカスを戻す。
//!
//! 担当: タスク E。
//!
//! # 使い方（`ui::app` 側）
//!
//! - コマンド `memo.open_panel`（キーは統括が割り当てる）の実装で、初めてなら
//!   [`MemoPanel::create`]（ここでは何も読まない）、以降は [`MemoPanel::toggle`] か
//!   [`MemoPanel::show`] を呼ぶ。小窓はメインウィンドウの所有ウィンドウなので、
//!   メインウィンドウと一緒に破棄される。
//! - 一覧の読み込み・検索・メモの読み込み・新規作成は、すべて [`WorkerPool`] で行う。
//!   結果は小窓が自分で UI スレッドに受け取り、検索の世代を照合してから反映する。
//! - 利用者の操作は `on_event` に [`PanelEvent`] として届く。`on_event` は小窓の
//!   ウィンドウプロシージャの中から呼ばれるので、重い処理や小窓自身の破棄はせず、
//!   必要なら `PostMessageW` でメインウィンドウに委ねる。
//! - 小窓が閉じたら（Esc、×、メモを開いた直後）[`PanelEvent::Closed`] が届く。
//!   `ui::app` は本文（`focus_active`）にフォーカスを戻す。
//! - 配色は [`MemoPanel::set_theme`]（テーマ切り替えのたびに呼ぶ）、DPI が変わったら
//!   [`MemoPanel::set_dpi`]。
//!
//! キー操作: 検索欄で ↑↓ で一覧を選び、Enter で開く。Esc で閉じる。一覧のダブルクリックでも開く。

use std::rc::Rc;
use std::sync::Arc;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, SetBkColor, SetTextColor, HBRUSH, HDC,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, VK_DELETE, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect,
    GetWindowLongPtrW, GetWindowRect, IsWindowVisible, LoadCursorW, MoveWindow, PostMessageW,
    RegisterClassExW, SetWindowLongPtrW, SetWindowPos, ShowWindow, BN_CLICKED, BS_PUSHBUTTON,
    CREATESTRUCTW, ES_AUTOHSCROLL, GWLP_USERDATA, GWLP_WNDPROC, HMENU, IDC_ARROW, LBN_DBLCLK,
    LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL,
    MINMAXINFO, SWP_NOMOVE, SWP_NOZORDER, SW_HIDE, SW_SHOW, WM_ACTIVATE, WM_CHAR, WM_CLOSE,
    WM_COMMAND, WM_CREATE, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_ERASEBKGND, WM_GETMINMAXINFO,
    WM_KEYDOWN, WM_NCDESTROY, WM_SETFONT, WM_SIZE, WNDCLASSEXW, WNDPROC, WS_BORDER, WS_CAPTION,
    WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE, WS_EX_TOOLWINDOW, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_THICKFRAME, WS_VISIBLE, WS_VSCROLL,
};

use crate::core::{
    ui_channel, CancelToken, Completion, Error, MemoId, Result, TaskContext, UiReceiver, UiSender,
    UiWaker, WorkerPool,
};
use crate::memo::{Memo, MemoHit, MemoStore};
use crate::platform::system::{scale, set_dark_title_bar, to_wide};
use crate::ui::theme::Theme;
use crate::ui::window::{UiFont, WM_APP_FIRST_FREE};
use windows::Win32::Graphics::Gdi::{
    DrawFocusRect, DrawTextW, SelectObject, SetBkMode, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX,
    DT_RIGHT, DT_SINGLELINE, DT_VCENTER, TRANSPARENT,
};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_FOCUS, ODS_SELECTED};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{SetWindowTextW, WM_DRAWITEM};

const CLASS: PCWSTR = w!("LinoteMemoPanel");
/// 作業スレッドからの結果が届いたことを知らせる（`WM_APP + 16`）。
const WM_PANEL_RESULT: u32 = WM_APP_FIRST_FREE;

const ID_SEARCH: usize = 101;
const ID_LIST: usize = 102;
const ID_NEW: usize = 103;
const ID_DELETE: usize = 104;

const EN_CHANGE: u32 = 0x0300;
/// 検索結果の一覧に載せる最大件数。
const MAX_RESULTS: u64 = 500;

/// 小窓から `ui::app` への通知。
#[derive(Debug)]
pub enum PanelEvent {
    /// このメモをタブで開く（読み込み済み。新規作成した直後のメモも同じ）。
    Open(Memo),
    /// 選択中のメモの削除が求められた（確認と削除は `ui::app` が行い、終わったら
    /// [`MemoPanel::refresh`] で一覧を読み直す）。
    Delete { id: MemoId, name: String },
    /// 小窓が閉じた。本文にフォーカスを戻す。
    Closed,
    /// 一覧・読み込み・作成の失敗（状態表示などで利用者に知らせる）。
    Failed(Error),
}

/// 作業スレッドから UI スレッドへ。
enum Msg {
    Search {
        generation: u64,
        result: Result<(Vec<MemoHit>, Completion)>,
    },
    Loaded(Result<Memo>),
}

struct State {
    hwnd: HWND,
    edit: HWND,
    list: HWND,
    button: HWND,
    delete_button: HWND,
    /// 一覧が空のときに、一覧の代わりに出す案内。
    empty: HWND,
    store: MemoStore,
    font: UiFont,
    dpi: u32,
    bg_brush: HBRUSH,
    list_brush: HBRUSH,
    theme: Theme,
    hits: Vec<MemoHit>,
    generation: u64,
    cancel: Option<CancelToken>,
    tx: UiSender<Msg>,
    rx: UiReceiver<Msg>,
    on_event: Rc<dyn Fn(PanelEvent)>,
    edit_proc: WNDPROC,
    list_proc: WNDPROC,
}

/// メモの小窓への取っ手。ウィンドウはメインウィンドウの所有ウィンドウで、
/// 所有者と一緒に破棄される（この値は軽く、複製してよい）。
#[derive(Debug, Clone, Copy)]
pub struct MemoPanel {
    hwnd: HWND,
}

struct PostWaker(isize);

impl UiWaker for PostWaker {
    fn wake(&self) {
        // SAFETY: ウィンドウが既にないときは失敗するだけ。
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.0 as *mut _)),
                WM_PANEL_RESULT,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

struct CreateParams {
    store: MemoStore,
    theme: Theme,
    dpi: u32,
    on_event: Rc<dyn Fn(PanelEvent)>,
}

impl MemoPanel {
    /// 小窓を作る（表示はしない。メモは読まない）。
    pub fn create(
        owner: HWND,
        store: MemoStore,
        theme: &Theme,
        dpi: u32,
        on_event: Rc<dyn Fn(PanelEvent)>,
    ) -> Result<MemoPanel> {
        register_class()?;
        let params = Box::new(CreateParams {
            store,
            theme: theme.clone(),
            dpi,
            on_event,
        });
        let raw = Box::into_raw(params);
        // SAFETY: `raw` は WM_CREATE で Box に戻す（作成に失敗したときは下で戻す）。
        let created = unsafe {
            let instance = GetModuleHandleW(None).unwrap_or_default();
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS,
                w!("メモ"),
                WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN,
                0,
                0,
                scale(420, dpi),
                scale(480, dpi),
                Some(owner),
                None,
                Some(instance.into()),
                Some(raw as *const _),
            )
        };
        match created {
            Ok(hwnd) => {
                let panel = MemoPanel { hwnd };
                panel.ensure_minimum_size();
                panel.center_on(owner);
                Ok(panel)
            }
            // WM_CREATE が届かずに失敗した場合は params を回収できず漏れるが、
            // ウィンドウを作れない異常時の小さな漏れなので許容する。
            Err(e) => Err(Error::unavailable(
                "メモの小窓",
                format!("作れませんでした: {e}"),
            )),
        }
    }

    fn ensure_minimum_size(&self) {
        if let Some((min_w, min_h)) = with_state(self.hwnd, |s| s.minimum_outer_size()) {
            let mut rect = RECT::default();
            // SAFETY: Compute the size before resizing, so WM_SIZE can borrow State independently.
            unsafe {
                let _ = GetWindowRect(self.hwnd, &mut rect);
                if rect.right - rect.left < min_w || rect.bottom - rect.top < min_h {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        0,
                        0,
                        (rect.right - rect.left).max(min_w),
                        (rect.bottom - rect.top).max(min_h),
                        SWP_NOMOVE | SWP_NOZORDER,
                    );
                }
            }
        }
    }

    fn center_on(&self, owner: HWND) {
        // SAFETY: 有効なウィンドウの矩形を読んで移動するだけ。
        unsafe {
            let mut o = RECT::default();
            let mut r = RECT::default();
            if GetWindowRect(owner, &mut o).is_ok() && GetWindowRect(self.hwnd, &mut r).is_ok() {
                let x = o.left + ((o.right - o.left) - (r.right - r.left)) / 2;
                let y = o.top + ((o.bottom - o.top) - (r.bottom - r.top)) / 3;
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    x.max(0),
                    y.max(0),
                    0,
                    0,
                    windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE | SWP_NOZORDER,
                );
            }
        }
    }

    pub fn is_visible(&self) -> bool {
        // SAFETY: ウィンドウハンドルの問い合わせだけ。
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    /// 表示して一覧を読み直し、検索欄にフォーカスを置く。
    pub fn show(&self) {
        // SAFETY: 自分のウィンドウを表示するだけ。
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
        }
        with_state(self.hwnd, |s| {
            s.start_search();
            // SAFETY: 自分の子ウィンドウ。
            unsafe {
                let _ = SetFocus(Some(s.edit));
            }
        });
    }

    /// 一覧を読み直す（削除の後など）。
    pub fn refresh(&self) {
        with_state(self.hwnd, |s| s.start_search());
    }

    /// 開いていれば閉じ、閉じていれば開く。
    pub fn toggle(&self) {
        if self.is_visible() {
            self.hide();
        } else {
            self.show();
        }
    }

    /// 閉じる（[`PanelEvent::Closed`] が届く）。
    pub fn hide(&self) {
        // SAFETY: WM_CLOSE を自分に送る（処理は下の wndproc）。
        unsafe {
            let _ = SendMessageW(self.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    /// 配色を替える。
    pub fn set_theme(&self, theme: &Theme) {
        with_state(self.hwnd, |s| {
            s.theme = theme.clone();
            s.rebuild_brushes();
            set_dark_title_bar(s.hwnd, s.theme.dark);
            s.repaint();
        });
    }

    /// DPI が変わったときに書体と大きさを直す。
    pub fn set_dpi(&self, dpi: u32) {
        with_state(self.hwnd, |s| {
            s.dpi = dpi;
            s.font = UiFont::for_dpi(dpi);
            s.apply_font();
            s.layout();
        });
        self.ensure_minimum_size();
    }

    /// 破棄する（所有者の破棄でも自動的に破棄される）。
    pub fn destroy(self) {
        // SAFETY: 自分のウィンドウを破棄する。
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn register_class() -> Result<()> {
    static REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ok = *REGISTERED.get_or_init(|| {
        // SAFETY: クラスの情報は呼び出しの間有効。
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(panel_proc),
                hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class) != 0
        }
    });
    if ok {
        Ok(())
    } else {
        Err(Error::unavailable(
            "メモの小窓",
            "ウィンドウクラスを登録できませんでした",
        ))
    }
}

fn with_state<R>(hwnd: HWND, f: impl FnOnce(&mut State) -> R) -> Option<R> {
    // SAFETY: GWLP_USERDATA には WM_CREATE で作った State へのポインタを入れ、
    // WM_NCDESTROY で外して解放する。UI スレッドだけが触る。
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        ptr.as_mut().map(f)
    }
}

fn child(parent: HWND, class: PCWSTR, text: PCWSTR, style: u32, ex: u32, id: usize) -> HWND {
    // SAFETY: 親は作成中のウィンドウ。
    unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(ex),
            class,
            text,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(style) | WS_CHILD | WS_VISIBLE,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(id as *mut _)),
            GetModuleHandleW(None).ok().map(Into::into),
            None,
        )
        .unwrap_or_default()
    }
}

impl State {
    fn rebuild_brushes(&mut self) {
        // SAFETY: 自分が作ったブラシを消して作り直す。
        unsafe {
            if !self.bg_brush.is_invalid() {
                let _ = DeleteObject(self.bg_brush.into());
            }
            if !self.list_brush.is_invalid() {
                let _ = DeleteObject(self.list_brush.into());
            }
            self.bg_brush = CreateSolidBrush(COLORREF(self.theme.ui.bar_bg.colorref()));
            self.list_brush = CreateSolidBrush(COLORREF(self.theme.editor.bg.colorref()));
        }
    }

    fn repaint(&self) {
        use windows::Win32::Graphics::Gdi::InvalidateRect;
        // SAFETY: 自分のウィンドウと子ウィンドウ。
        unsafe {
            for h in [self.hwnd, self.edit, self.list, self.button] {
                let _ = InvalidateRect(Some(h), None, true);
            }
        }
    }

    /// 一覧の 1 行の高さ（名前と日時を 1 行に描く）。
    fn item_height(&self) -> i32 {
        self.font.measure_text("あAg", None).cy + scale(10, self.dpi)
    }

    fn apply_font(&self) {
        // SAFETY: 自分のリストボックス（自前で描く固定高さの行）。
        unsafe {
            SendMessageW(
                self.list,
                LB_SETITEMHEIGHT,
                WPARAM(0),
                LPARAM(self.item_height() as isize),
            );
        }
        for h in [
            self.edit,
            self.list,
            self.button,
            self.delete_button,
            self.empty,
        ] {
            // SAFETY: 書体は State が保持する間有効。
            unsafe {
                SendMessageW(
                    h,
                    WM_SETFONT,
                    WPARAM(self.font.handle().0 as usize),
                    LPARAM(1),
                );
            }
        }
    }

    fn minimum_outer_size(&self) -> (i32, i32) {
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};
        let button_width = [self.button, self.delete_button]
            .into_iter()
            .map(|h| {
                let mut text = vec![0u16; unsafe { GetWindowTextLengthW(h) }.max(0) as usize + 1];
                let len = unsafe { GetWindowTextW(h, &mut text) }.max(0) as usize;
                self.font
                    .measure_text(&String::from_utf16_lossy(&text[..len]), None)
                    .cx
                    + scale(24, self.dpi)
            })
            .max()
            .unwrap_or(0);
        let (mut outer, mut client) = (RECT::default(), RECT::default());
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut outer);
            let _ = GetClientRect(self.hwnd, &mut client);
        }
        (
            button_width * 2 + scale(24, self.dpi) + outer.right - outer.left - client.right
                + client.left,
            self.font.measure_text("あAg", None).cy * 2 + scale(150, self.dpi) + outer.bottom
                - outer.top
                - client.bottom
                + client.top,
        )
    }

    fn layout(&self) {
        let mut rc = RECT::default();
        // SAFETY: 自分のウィンドウ。
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let px = |n| scale(n, self.dpi);
        let (m, gap) = (px(12), px(8));
        let row = self.font.measure_text("あAg", None).cy + px(10);
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        let inner = (w - 2 * m).max(0);
        let button_w =
            |b: HWND| (self.font.measure_text(&window_text(b), None).cx + px(28)).max(px(88));
        let list_y = m + row + gap;
        let button_h = row + px(2);
        let list_h = (h - list_y - gap - button_h - m).max(0);
        let y = list_y + list_h + gap;
        // SAFETY: 自分の子ウィンドウ。
        unsafe {
            let _ = MoveWindow(self.edit, m, m, inner, row, true);
            let _ = MoveWindow(self.list, m, list_y, inner, list_h, true);
            let _ = MoveWindow(self.empty, m, list_y + px(8), inner, list_h - px(8), true);
            // 主な操作（新規メモ）は右、削除は左に離して置く。
            let new_w = button_w(self.button);
            let _ = MoveWindow(self.button, m + inner - new_w, y, new_w, button_h, true);
            let _ = MoveWindow(
                self.delete_button,
                m,
                y,
                button_w(self.delete_button),
                button_h,
                true,
            );
        }
    }

    fn query(&self) -> String {
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};
        // SAFETY: 長さ + 1 の領域を渡す。
        unsafe {
            let len = GetWindowTextLengthW(self.edit).max(0) as usize;
            let mut buf = vec![0u16; len + 1];
            let n = GetWindowTextW(self.edit, &mut buf).max(0) as usize;
            String::from_utf16_lossy(&buf[..n])
        }
    }

    /// 検索を作業スレッドで始める（前の検索は中止する）。結果は世代で照合する。
    fn start_search(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.cancel();
        }
        self.generation += 1;
        let generation = self.generation;
        let cancel = CancelToken::new();
        self.cancel = Some(cancel.clone());
        let store = self.store.clone();
        let query = self.query();
        let tx = self.tx.clone();
        WorkerPool::global().spawn(move || {
            let ctx = TaskContext::detached().with_limits(crate::core::Limits::items(MAX_RESULTS));
            let ctx = TaskContext { cancel, ..ctx };
            let result = store.search(&query, &ctx).map(|o| (o.value, o.completion));
            tx.send(Msg::Search { generation, result });
        });
    }

    fn selected(&self) -> Option<&MemoHit> {
        // SAFETY: 自分のリストボックス。
        let i = unsafe { SendMessageW(self.list, LB_GETCURSEL, WPARAM(0), LPARAM(0)) }.0;
        usize::try_from(i).ok().and_then(|i| self.hits.get(i))
    }

    fn move_selection(&self, delta: isize) {
        let n = self.hits.len() as isize;
        if n == 0 {
            return;
        }
        // SAFETY: 自分のリストボックス。
        unsafe {
            let cur = SendMessageW(self.list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
            let next = (cur + delta).clamp(0, n - 1);
            SendMessageW(self.list, LB_SETCURSEL, WPARAM(next as usize), LPARAM(0));
        }
    }

    fn show_hits(&mut self, hits: Vec<MemoHit>) {
        // SAFETY: 自分のリストボックスに、呼び出しの間有効な文字列を渡す。
        unsafe {
            SendMessageW(self.list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
            for h in &hits {
                let label = match (&h.snippet, h.name_match) {
                    (Some(s), false) => format!("{}  —  {}", h.name, s),
                    _ => h.name.clone(),
                };
                let wide = to_wide(&label);
                SendMessageW(
                    self.list,
                    LB_ADDSTRING,
                    WPARAM(0),
                    LPARAM(wide.as_ptr() as isize),
                );
            }
            if !hits.is_empty() {
                SendMessageW(self.list, LB_SETCURSEL, WPARAM(0), LPARAM(0));
            }
        }
        self.hits = hits;
        self.update_empty();
    }

    /// 一覧が空なら、一覧の代わりに案内を出す。削除は選択があるときだけ。
    fn update_empty(&self) {
        let empty = self.hits.is_empty();
        let text = if !empty {
            ""
        } else if self.query().trim().is_empty() {
            "メモはまだありません。\n「新規メモ」で作成できます。"
        } else {
            "一致するメモはありません。\n「新規メモ」で、この名前のメモを作成できます。"
        };
        let wide = to_wide(text);
        // SAFETY: 自分の子ウィンドウ。
        unsafe {
            let _ = SetWindowTextW(self.empty, PCWSTR(wide.as_ptr()));
            let _ = ShowWindow(self.empty, if empty { SW_SHOW } else { SW_HIDE });
            let _ = ShowWindow(self.list, if empty { SW_HIDE } else { SW_SHOW });
            let _ = EnableWindow(self.delete_button, !empty);
        }
    }

    /// 一覧の 1 行を描く（名前と抜粋を左、更新日時を右に薄く）。
    fn draw_item(&self, d: &DRAWITEMSTRUCT) {
        let Some(hit) = self.hits.get(d.itemID as usize) else {
            return;
        };
        let selected = d.itemState.0 & ODS_SELECTED.0 != 0;
        let t = &self.theme;
        let bg = if selected {
            t.editor.selection_bg
        } else {
            t.editor.bg
        };
        let px = |n| scale(n, self.dpi);
        let mut rc = d.rcItem;
        // SAFETY: WM_DRAWITEM で渡された DC と矩形。
        unsafe {
            let brush = CreateSolidBrush(COLORREF(bg.colorref()));
            FillRect(d.hDC, &rc, brush);
            let _ = DeleteObject(brush.into());
            let old_font = SelectObject(d.hDC, self.font.handle().into());
            SetBkMode(d.hDC, TRANSPARENT);
            let date = to_wide(&format_updated(hit.updated_ms));
            let date_w = self
                .font
                .measure_text(&format_updated(hit.updated_ms), None)
                .cx;
            rc.left += px(6);
            rc.right -= px(6);
            let mut date_rc = rc;
            SetTextColor(d.hDC, COLORREF(t.ui.status_dim.colorref()));
            let mut date_text = date[..date.len() - 1].to_vec();
            DrawTextW(
                d.hDC,
                &mut date_text,
                &mut date_rc,
                DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            let mut name_rc = rc;
            name_rc.right = (rc.right - date_w - px(12)).max(rc.left);
            let label = match (&hit.snippet, hit.name_match) {
                (Some(snippet), false) => format!("{}  —  {}", hit.name, snippet),
                _ => hit.name.clone(),
            };
            let mut text: Vec<u16> = label.encode_utf16().collect();
            SetTextColor(d.hDC, COLORREF(t.editor.fg.colorref()));
            DrawTextW(
                d.hDC,
                &mut text,
                &mut name_rc,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
            SelectObject(d.hDC, old_font);
            if d.itemState.0 & ODS_FOCUS.0 != 0 {
                let _ = DrawFocusRect(d.hDC, &d.rcItem);
            }
        }
    }

    fn emit(&self, event: PanelEvent) {
        (self.on_event)(event);
    }

    /// 選択中のメモを作業スレッドで読み、届いたら [`PanelEvent::Open`]。
    fn open_selected(&self) {
        let Some(id) = self.selected().map(|h| h.id.clone()) else {
            return;
        };
        self.load(id);
    }

    /// 選択中のメモの削除を求める。
    fn delete_selected(&self) {
        let Some((id, name)) = self.selected().map(|h| (h.id.clone(), h.name.clone())) else {
            return;
        };
        self.emit(PanelEvent::Delete { id, name });
    }

    fn load(&self, id: MemoId) {
        let store = self.store.clone();
        let tx = self.tx.clone();
        WorkerPool::global().spawn(move || {
            tx.send(Msg::Loaded(store.load(&id)));
        });
    }

    /// 新しいメモを作って開く。検索欄に文字があればそれを名前にする。
    fn create_new(&self) {
        let query = self.query();
        let name = if query.trim().is_empty() {
            "無題のメモ".to_owned()
        } else {
            query.trim().to_owned()
        };
        let store = self.store.clone();
        let tx = self.tx.clone();
        WorkerPool::global().spawn(move || {
            tx.send(Msg::Loaded(store.create(&name, "")));
        });
    }

    fn drain(&mut self) {
        let msgs: Vec<Msg> = self.rx.drain().collect();
        for msg in msgs {
            match msg {
                Msg::Search { generation, result } => {
                    if generation != self.generation {
                        continue;
                    }
                    match result {
                        Ok((hits, _completion)) => self.show_hits(hits),
                        Err(e) if e.is_cancelled() => {}
                        Err(e) => self.emit(PanelEvent::Failed(e)),
                    }
                }
                Msg::Loaded(Ok(memo)) => {
                    self.emit(PanelEvent::Open(memo));
                    // SAFETY: 自分のウィンドウを隠して Closed を通知する。
                    unsafe {
                        let _ = ShowWindow(self.hwnd, SW_HIDE);
                    }
                    self.emit(PanelEvent::Closed);
                }
                Msg::Loaded(Err(e)) => self.emit(PanelEvent::Failed(e)),
            }
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        // SAFETY: 自分が作ったブラシを 1 回だけ消す。
        unsafe {
            if !self.bg_brush.is_invalid() {
                let _ = DeleteObject(self.bg_brush.into());
            }
            if !self.list_brush.is_invalid() {
                let _ = DeleteObject(self.list_brush.into());
            }
        }
    }
}

extern "system" fn panel_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            // SAFETY: lparam は CREATESTRUCTW、lpCreateParams は create() が渡した Box。
            let params = unsafe {
                let cs = &*(lparam.0 as *const CREATESTRUCTW);
                Box::from_raw(cs.lpCreateParams as *mut CreateParams)
            };
            let CreateParams {
                store,
                theme,
                dpi,
                on_event,
            } = *params;
            let edit = child(
                hwnd,
                w!("EDIT"),
                w!(""),
                WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32,
                WS_EX_CLIENTEDGE.0,
                ID_SEARCH,
            );
            let list = child(
                hwnd,
                w!("LISTBOX"),
                w!(""),
                WS_TABSTOP.0
                    | WS_VSCROLL.0
                    | (LBS_NOTIFY | LBS_NOINTEGRALHEIGHT | LBS_OWNERDRAWFIXED | LBS_HASSTRINGS)
                        as u32,
                WS_EX_CLIENTEDGE.0,
                ID_LIST,
            );
            let empty = child(hwnd, w!("STATIC"), w!(""), SS_CENTER, 0, 0);
            // 検索欄の薄い案内（空のときだけ見える）。
            let cue = to_wide("メモを検索（名前と本文）");
            // SAFETY: 自分の入力欄。文字列はコピーされる。
            unsafe {
                SendMessageW(
                    edit,
                    EM_SETCUEBANNER,
                    WPARAM(1),
                    LPARAM(cue.as_ptr() as isize),
                );
            }
            let button = child(
                hwnd,
                w!("BUTTON"),
                w!("新規メモ"),
                WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
                0,
                ID_NEW,
            );
            let delete_button = child(
                hwnd,
                w!("BUTTON"),
                w!("削除"),
                WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
                0,
                ID_DELETE,
            );
            let (tx, rx) = ui_channel(Arc::new(PostWaker(hwnd.0 as isize)));
            let mut state = Box::new(State {
                hwnd,
                edit,
                list,
                button,
                delete_button,
                empty,
                store,
                font: UiFont::for_dpi(dpi),
                dpi,
                bg_brush: HBRUSH::default(),
                list_brush: HBRUSH::default(),
                theme,
                hits: Vec::new(),
                generation: 0,
                cancel: None,
                tx,
                rx,
                on_event,
                edit_proc: None,
                list_proc: None,
            });
            state.rebuild_brushes();
            state.apply_font();
            // SAFETY: 元の処理を控えて、キー操作を受ける処理に差し替える。
            unsafe {
                let old =
                    SetWindowLongPtrW(edit, GWLP_WNDPROC, key_proc as *const () as usize as isize);
                state.edit_proc = std::mem::transmute::<isize, WNDPROC>(old);
                let old =
                    SetWindowLongPtrW(list, GWLP_WNDPROC, key_proc as *const () as usize as isize);
                state.list_proc = std::mem::transmute::<isize, WNDPROC>(old);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
            }
            with_state(hwnd, |s| {
                set_dark_title_bar(hwnd, s.theme.dark);
                s.layout();
            });
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            if let Some((w, h)) = with_state(hwnd, |s| s.minimum_outer_size()) {
                if lparam.0 != 0 {
                    let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                    info.ptMinTrackSize.x = w;
                    info.ptMinTrackSize.y = h;
                }
            }
            LRESULT(0)
        }
        WM_SIZE => {
            with_state(hwnd, |s| s.layout());
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            let brush = with_state(hwnd, |s| s.bg_brush);
            if let Some(brush) = brush {
                let mut rc = RECT::default();
                // SAFETY: wparam は描画先の DC。
                unsafe {
                    let _ = GetClientRect(hwnd, &mut rc);
                    FillRect(HDC(wparam.0 as *mut _), &rc, brush);
                }
                return LRESULT(1);
            }
            // SAFETY: 既定の処理。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DRAWITEM => {
            if lparam.0 != 0 {
                // SAFETY: WM_DRAWITEM の lparam は DRAWITEMSTRUCT。
                let d = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
                with_state(hwnd, |s| s.draw_item(d));
            }
            LRESULT(1)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORSTATIC_ => {
            let brush = with_state(hwnd, |s| {
                let hdc = HDC(wparam.0 as *mut _);
                let fg = if msg == WM_CTLCOLORSTATIC_ {
                    s.theme.ui.status_dim
                } else {
                    s.theme.editor.fg
                };
                // SAFETY: wparam は子ウィンドウの DC。
                unsafe {
                    SetTextColor(hdc, COLORREF(fg.colorref()));
                    SetBkColor(hdc, COLORREF(s.theme.editor.bg.colorref()));
                }
                s.list_brush
            });
            match brush {
                Some(b) => LRESULT(b.0 as isize),
                // SAFETY: 既定の処理。
                None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            }
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xFFFF;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            with_state(hwnd, |s| match (id, code) {
                (ID_SEARCH, EN_CHANGE) => s.start_search(),
                (ID_LIST, LBN_DBLCLK) => s.open_selected(),
                (ID_NEW, BN_CLICKED) => s.create_new(),
                (ID_DELETE, BN_CLICKED) => s.delete_selected(),
                _ => {}
            });
            LRESULT(0)
        }
        WM_PANEL_RESULT => {
            with_state(hwnd, |s| s.drain());
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // 前面に戻ってきたときに検索欄へ。
            if wparam.0 & 0xFFFF != 0 {
                with_state(hwnd, |s| {
                    // SAFETY: 自分の子ウィンドウ。
                    unsafe {
                        let _ = SetFocus(Some(s.edit));
                    }
                });
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // 破棄せず隠す（次に開くときに一覧を読み直す）。
            // SAFETY: 自分のウィンドウ。
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            with_state(hwnd, |s| {
                if let Some(c) = s.cancel.take() {
                    c.cancel();
                }
                s.emit(PanelEvent::Closed);
            });
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // SAFETY: WM_CREATE で入れたポインタを外して解放する。
            unsafe {
                let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut State;
                if !ptr.is_null() {
                    drop(Box::from_raw(ptr));
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        // SAFETY: 既定の処理。
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 検索欄と一覧のキー操作（↑↓、Enter、Esc）。ほかは元の処理に渡す。
extern "system" fn key_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: 親は小窓。
    let parent =
        unsafe { windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd).unwrap_or_default() };
    let key = wparam.0 as u16;
    let handled = match msg {
        WM_KEYDOWN if key == VK_ESCAPE.0 => {
            // SAFETY: 小窓に WM_CLOSE を送る。
            unsafe {
                SendMessageW(parent, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            true
        }
        WM_KEYDOWN if key == VK_RETURN.0 => {
            with_state(parent, |s| s.open_selected());
            true
        }
        WM_KEYDOWN if (key == VK_DOWN.0 || key == VK_UP.0) && is_edit(parent, hwnd) => {
            let delta = if key == VK_DOWN.0 { 1 } else { -1 };
            with_state(parent, |s| s.move_selection(delta));
            true
        }
        WM_KEYDOWN if key == VK_DELETE.0 && !is_edit(parent, hwnd) => {
            with_state(parent, |s| s.delete_selected());
            true
        }
        // Enter・Esc の文字がビープ音にならないようにする。
        WM_CHAR if key == 0x0D || key == 0x1B => true,
        _ => false,
    };
    if handled {
        return LRESULT(0);
    }
    let orig = with_state(parent, |s| {
        if hwnd == s.edit {
            s.edit_proc
        } else {
            s.list_proc
        }
    })
    .flatten();
    // SAFETY: 差し替え前の処理にそのまま渡す。
    unsafe { CallWindowProcW(orig, hwnd, msg, wparam, lparam) }
}

fn is_edit(parent: HWND, hwnd: HWND) -> bool {
    with_state(parent, |s| s.edit == hwnd).unwrap_or(false)
}

/// `SendMessageW` の引数を、このファイルで使いやすい形（`Option` なし）にした包み。
#[allow(non_snake_case)]
unsafe fn SendMessageW(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    windows::Win32::UI::WindowsAndMessaging::SendMessageW(h, msg, Some(w), Some(l))
}

const EM_SETCUEBANNER: u32 = 0x1501;
const WM_CTLCOLORSTATIC_: u32 = 0x0138;
const LBS_OWNERDRAWFIXED: i32 = 0x0010;
const LBS_HASSTRINGS: i32 = 0x0040;
const LB_SETITEMHEIGHT: u32 = 0x01A0;
const SS_CENTER: u32 = 0x0001;

fn window_text(h: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};
    // SAFETY: 長さ + 1 の領域を渡す。
    unsafe {
        let len = GetWindowTextLengthW(h).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(h, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }
}

/// 更新日時の表示（今日なら時刻、今年なら月日、それより前は年月日）。
fn format_updated(updated_ms: u64) -> String {
    let Some(t) = local_parts(updated_ms) else {
        return String::new();
    };
    let now = local_parts(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    );
    format_relative(t, now)
}

/// （年, 月, 日, 時, 分）。
type DateParts = (u16, u16, u16, u16, u16);

fn format_relative(t: DateParts, now: Option<DateParts>) -> String {
    let (y, mo, d, h, mi) = t;
    match now {
        Some((ny, nmo, nd, _, _)) if (ny, nmo, nd) == (y, mo, d) => format!("{h}:{mi:02}"),
        Some((ny, _, _, _, _)) if ny == y => format!("{mo}/{d}"),
        _ => format!("{y}/{mo}/{d}"),
    }
}

/// UNIX 時刻（ミリ秒）を地域の日時にする。
fn local_parts(ms: u64) -> Option<DateParts> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    // 1601-01-01 からの 100 ナノ秒単位。
    const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
    let ticks = ms.checked_mul(10_000)?.checked_add(EPOCH_DIFF)?;
    let ft = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut local = FILETIME::default();
    // SAFETY: 出力先は呼び出しの間有効。
    unsafe { FileTimeToLocalFileTime(&ft, &mut local).ok()? };
    let local_ticks = (u64::from(local.dwHighDateTime) << 32) | u64::from(local.dwLowDateTime);
    let secs = local_ticks.checked_sub(EPOCH_DIFF)? / 10_000_000;
    Some(civil(secs))
}

/// UNIX 秒（地域時刻に直したもの）を年月日時分にする。
fn civil(secs: u64) -> DateParts {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // H. Hinnant の civil_from_days。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        y as u16,
        m as u16,
        d as u16,
        (rem / 3_600) as u16,
        (rem % 3_600 / 60) as u16,
    )
}

#[cfg(test)]
mod date_tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0));
        assert_eq!(civil(1_700_000_000), (2023, 11, 14, 22, 13));
        assert_eq!(civil(951_782_400), (2000, 2, 29, 0, 0));
    }

    #[test]
    fn relative_format() {
        let t = (2026, 11, 14, 9, 5);
        assert_eq!(format_relative(t, Some((2026, 11, 14, 23, 0))), "9:05");
        assert_eq!(format_relative(t, Some((2026, 12, 1, 0, 0))), "11/14");
        assert_eq!(format_relative(t, Some((2027, 1, 1, 0, 0))), "2026/11/14");
        assert_eq!(format_relative(t, None), "2026/11/14");
    }
}
