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
    SWP_NOZORDER, SW_HIDE, SW_SHOW, WM_ACTIVATE, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_ERASEBKGND, WM_KEYDOWN, WM_NCDESTROY, WM_SETFONT,
    WM_SIZE, WNDCLASSEXW, WNDPROC, WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN,
    WS_EX_CLIENTEDGE, WS_EX_TOOLWINDOW, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_THICKFRAME,
    WS_VISIBLE, WS_VSCROLL,
};

use crate::core::{
    ui_channel, CancelToken, Completion, Error, MemoId, Result, TaskContext, UiReceiver, UiSender,
    UiWaker, WorkerPool,
};
use crate::memo::{Memo, MemoHit, MemoStore};
use crate::platform::system::{scale, set_dark_title_bar, to_wide};
use crate::ui::theme::Theme;
use crate::ui::window::{UiFont, WM_APP_FIRST_FREE};

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

    fn apply_font(&self) {
        for h in [self.edit, self.list, self.button, self.delete_button] {
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

    fn layout(&self) {
        let mut rc = RECT::default();
        // SAFETY: 自分のウィンドウ。
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let m = scale(8, self.dpi);
        let edit_h = scale(28, self.dpi);
        let button_h = scale(30, self.dpi);
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        let inner = (w - 2 * m).max(0);
        let list_h = (h - edit_h - button_h - 4 * m).max(0);
        // SAFETY: 自分の子ウィンドウ。
        unsafe {
            let _ = MoveWindow(self.edit, m, m, inner, edit_h, true);
            let _ = MoveWindow(self.list, m, 2 * m + edit_h, inner, list_h, true);
            let half = ((inner - m) / 2).max(0);
            let y = 3 * m + edit_h + list_h;
            let _ = MoveWindow(self.button, m, y, half, button_h, true);
            let _ = MoveWindow(
                self.delete_button,
                m + half + m,
                y,
                (inner - half - m).max(0),
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
                WS_TABSTOP.0 | WS_VSCROLL.0 | (LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32,
                WS_EX_CLIENTEDGE.0,
                ID_LIST,
            );
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
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let brush = with_state(hwnd, |s| {
                let hdc = HDC(wparam.0 as *mut _);
                // SAFETY: wparam は子ウィンドウの DC。
                unsafe {
                    SetTextColor(hdc, COLORREF(s.theme.editor.fg.colorref()));
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
