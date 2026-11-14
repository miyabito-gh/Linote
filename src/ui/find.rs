//! 検索・置換欄と Grep の結果の画面（1 枚の小窓）。中核は [`crate::search`]。
//! 初めて使うときに作る（[`FindPanel::create`] は何も読み込まない）。
//!
//! 担当: タスク K。
//!
//! # 使い方（`ui::app` 側）
//!
//! - コマンド `search.find`／`search.replace`／`search.grep`（キーは統括が割り当てる）で、
//!   初めてなら [`FindPanel::create`]、以降は [`FindPanel::show`]（選択文字列があれば
//!   `prefill` に渡す）。小窓はメインウィンドウの所有ウィンドウ。
//! - 文書内の検索・置換は小窓の中ではやらない。利用者の操作は [`FindEvent`] で届くので、
//!   `ui::app` が現在の文書に対して [`crate::search::Searcher`] を呼ぶ。
//!   - 通常の文書: 本文を `TextSource`／`EditTarget` として包み（`ui::editor` の
//!     Scintilla 呼び出し）、`Searcher::find`（`wrap = true`）→ 選択、
//!     `replacement_for` → `replace`、`replace_all`（Undo は 1 単位）。
//!     長い文書は `WorkerPool` で行い、結果は文書 ID と版（`DocStamp`）を照合してから反映する。
//!   - 巨大モード: `bigfile` の `BigFile::find`（`SearchPattern`）を呼ぶ。起動前に
//!     [`FindPanel::set_replace_enabled`]`(false, 案内)` を呼び、置換を無効にして
//!     制約（行単位の検索など）を状態欄に案内する。
//!   - 結果の通知は [`FindPanel::set_status`]（無効なパターンはこの小窓が自分で表示する）。
//! - Grep は小窓が作業スレッドで行い、結果を一覧に逐次足す。一覧の選択（Enter、ダブル
//!   クリック）で [`FindEvent::OpenHit`] が届くので、`ui::app` がファイルを開いて該当行へ
//!   移る。起点のフォルダーは [`FindPanel::set_grep_folder`]。
//! - 小窓が閉じたら [`FindEvent::Closed`]。`ui::app` は本文にフォーカスを戻す。
//! - 配色は [`FindPanel::set_theme`]、DPI は [`FindPanel::set_dpi`]。
//!
//! キー操作: 検索欄で Enter＝次、Shift+Enter＝前。置換欄で Enter＝置換。フォルダー欄で
//! Enter＝Grep。一覧で Enter＝開く。Esc＝閉じる。

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, InvalidateRect, SetBkColor, SetTextColor, HBRUSH, HDC,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetKeyState, SetFocus, VK_ESCAPE, VK_RETURN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible,
    LoadCursorW, MoveWindow, PostMessageW, RegisterClassExW, SendMessageW, SetWindowLongPtrW,
    SetWindowPos, SetWindowTextW, ShowWindow, BM_GETCHECK, BN_CLICKED, BS_AUTOCHECKBOX,
    BS_PUSHBUTTON, CREATESTRUCTW, ES_AUTOHSCROLL, GWLP_USERDATA, GWLP_WNDPROC, HMENU, IDC_ARROW,
    LBN_DBLCLK, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT,
    LB_SETCURSEL, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOW, WM_ACTIVATE, WM_CHAR, WM_CLOSE,
    WM_COMMAND, WM_CREATE, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_ERASEBKGND,
    WM_KEYDOWN, WM_NCDESTROY, WM_SETFONT, WM_SIZE, WNDCLASSEXW, WNDPROC, WS_BORDER, WS_CAPTION,
    WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE, WS_EX_TOOLWINDOW, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_THICKFRAME, WS_VISIBLE, WS_VSCROLL,
};

use crate::core::{
    ui_channel, CancelToken, Completion, Error, Result, TaskContext, UiReceiver, UiSender, UiWaker,
    WorkerPool,
};
use crate::platform::system::{scale, set_dark_title_bar, to_wide};
use crate::search::{grep, GrepFile, GrepHit, GrepOptions, GrepSummary, SearchOptions};
use crate::ui::theme::Theme;
use crate::ui::window::{UiFont, WM_APP_FIRST_FREE};

const CLASS: PCWSTR = w!("LinoteFindPanel");
/// 作業スレッドからの結果が届いたことを知らせる（`WM_APP + 16`。メモの小窓と同じ番号で、
/// 別ウィンドウ向けなので衝突しない）。
const WM_PANEL_RESULT: u32 = WM_APP_FIRST_FREE;

const ID_QUERY: usize = 201;
const ID_REPLACE: usize = 202;
const ID_FOLDER: usize = 203;
const ID_CASE: usize = 204;
const ID_WORD: usize = 205;
const ID_REGEX: usize = 206;
const ID_NEXT: usize = 207;
const ID_PREV: usize = 208;
const ID_REPLACE_ONE: usize = 209;
const ID_REPLACE_ALL: usize = 210;
const ID_GREP: usize = 211;
const ID_STOP: usize = 212;
const ID_STATUS: usize = 213;
const ID_LIST: usize = 214;

const EN_CHANGE: u32 = 0x0300;
const BST_CHECKED: usize = 1;

/// 小窓から `ui::app` への通知。`on_event` は小窓のウィンドウプロシージャの中から
/// 呼ばれるので、重い処理や小窓自身の破棄はしない。
#[derive(Debug)]
pub enum FindEvent {
    /// 次を検索（端で回り込む）。
    Next(SearchOptions),
    /// 前を検索（端で回り込む）。
    Prev(SearchOptions),
    /// 選択が一致なら置換して次へ。
    Replace {
        options: SearchOptions,
        replacement: String,
    },
    /// すべて置換（Undo は 1 単位）。
    ReplaceAll {
        options: SearchOptions,
        replacement: String,
    },
    /// Grep の結果を開く。`line`・`column` は 1 始まり。
    OpenHit {
        path: PathBuf,
        line: u64,
        column: u64,
    },
    /// 小窓が閉じた。本文にフォーカスを戻す。
    Closed,
}

/// 一覧に出す 1 行（`root` からの相対パス、行番号、抜粋）。
pub fn hit_label(root: &Path, hit: &GrepHit) -> String {
    let rel = hit.path.strip_prefix(root).unwrap_or(&hit.path);
    let text: String = hit
        .text
        .trim()
        .chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .collect();
    format!("{}:{}:  {}", rel.display(), hit.line, text)
}

/// Grep の終わり方を状態欄の文にする。
pub fn grep_status(summary: &GrepSummary, completion: Completion) -> String {
    let tail = match completion {
        Completion::Complete => String::new(),
        Completion::ItemLimit => "（件数の上限で打ち切りました）".to_owned(),
        Completion::ByteLimit => "（容量の上限で打ち切りました）".to_owned(),
        Completion::Cancelled => "（中止しました）".to_owned(),
    };
    format!(
        "{} 件（{} ファイルを検索、{} 個を除外）{}",
        summary.hits, summary.files_scanned, summary.files_skipped, tail
    )
}

enum Msg {
    Files {
        generation: u64,
        files: Vec<GrepFile>,
    },
    Done {
        generation: u64,
        result: Result<(GrepSummary, Completion)>,
    },
}

struct State {
    hwnd: HWND,
    query: HWND,
    replace: HWND,
    folder: HWND,
    case: HWND,
    word: HWND,
    regex: HWND,
    buttons: [HWND; 6],
    status: HWND,
    list: HWND,
    font: UiFont,
    dpi: u32,
    bg_brush: HBRUSH,
    list_brush: HBRUSH,
    theme: Theme,
    status_is_error: bool,
    replace_enabled: bool,
    hits: Vec<GrepHit>,
    grep_root: PathBuf,
    generation: u64,
    cancel: Option<CancelToken>,
    tx: UiSender<Msg>,
    rx: UiReceiver<Msg>,
    on_event: Rc<dyn Fn(FindEvent)>,
    edit_proc: WNDPROC,
    list_proc: WNDPROC,
}

/// 検索の小窓への取っ手（軽く、複製してよい）。
#[derive(Debug, Clone, Copy)]
pub struct FindPanel {
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
    theme: Theme,
    dpi: u32,
    on_event: Rc<dyn Fn(FindEvent)>,
}

impl FindPanel {
    /// 小窓を作る（表示はしない）。
    pub fn create(
        owner: HWND,
        theme: &Theme,
        dpi: u32,
        on_event: Rc<dyn Fn(FindEvent)>,
    ) -> Result<FindPanel> {
        register_class()?;
        let raw = Box::into_raw(Box::new(CreateParams {
            theme: theme.clone(),
            dpi,
            on_event,
        }));
        // SAFETY: `raw` は WM_CREATE で Box に戻す。
        let created = unsafe {
            let instance = GetModuleHandleW(None).unwrap_or_default();
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS,
                w!("検索・置換・Grep"),
                WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN,
                0,
                0,
                scale(560, dpi),
                scale(520, dpi),
                Some(owner),
                None,
                Some(instance.into()),
                Some(raw as *const _),
            )
        };
        match created {
            Ok(hwnd) => {
                let panel = FindPanel { hwnd };
                panel.center_on(owner);
                Ok(panel)
            }
            Err(e) => Err(Error::unavailable(
                "検索の小窓",
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
                    SWP_NOSIZE | SWP_NOZORDER,
                );
            }
        }
    }

    pub fn is_visible(&self) -> bool {
        // SAFETY: ウィンドウハンドルの問い合わせだけ。
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    /// 表示して検索欄にフォーカスを置く。`prefill` があれば検索欄に入れて全選択する
    /// （改行を含む選択は渡さない）。
    pub fn show(&self, prefill: Option<&str>) {
        // SAFETY: 自分のウィンドウを表示するだけ。
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
        }
        with_state(self.hwnd, |s| {
            if let Some(text) = prefill.filter(|t| !t.is_empty() && !t.contains(['\n', '\r'])) {
                set_text(s.query, text);
            }
            // SAFETY: 自分の子ウィンドウ。EM_SETSEL(0, -1) で全選択。
            unsafe {
                let _ = SetFocus(Some(s.query));
                SendMessageW(s.query, 0x00B1, Some(WPARAM(0)), Some(LPARAM(-1)));
            }
            s.validate();
        });
    }

    pub fn toggle(&self) {
        if self.is_visible() {
            self.hide();
        } else {
            self.show(None);
        }
    }

    /// 閉じる（Grep は中止し、[`FindEvent::Closed`] が届く）。
    pub fn hide(&self) {
        // SAFETY: WM_CLOSE を自分に送る。
        unsafe {
            let _ = SendMessageW(self.hwnd, WM_CLOSE, None, None);
        }
    }

    /// 状態欄に文を出す。`error` なら警告色。
    pub fn set_status(&self, text: &str, error: bool) {
        with_state(self.hwnd, |s| s.show_status(text, error));
    }

    /// 置換を使えるかどうか。巨大モードでは `false` と案内文（状態欄に出す）。
    pub fn set_replace_enabled(&self, enabled: bool, note: Option<&str>) {
        with_state(self.hwnd, |s| {
            s.replace_enabled = enabled;
            // SAFETY: 自分の子ウィンドウ。
            unsafe {
                for h in [s.replace, s.buttons[2], s.buttons[3]] {
                    let _ = EnableWindow(h, enabled);
                }
            }
            if let Some(note) = note {
                s.show_status(note, false);
            }
        });
    }

    /// Grep の起点のフォルダーの初期値（空のときだけ入れる）。
    pub fn set_grep_folder(&self, folder: &Path) {
        with_state(self.hwnd, |s| {
            if text_of(s.folder).is_empty() {
                set_text(s.folder, &folder.to_string_lossy());
            }
        });
    }

    /// 現在の検索条件（無効なパターンの検査はしない）。
    pub fn options(&self) -> Option<SearchOptions> {
        with_state(self.hwnd, |s| s.options())
    }

    pub fn set_theme(&self, theme: &Theme) {
        with_state(self.hwnd, |s| {
            s.theme = theme.clone();
            s.rebuild_brushes();
            set_dark_title_bar(s.hwnd, s.theme.dark);
            s.repaint();
        });
    }

    pub fn set_dpi(&self, dpi: u32) {
        with_state(self.hwnd, |s| {
            s.dpi = dpi;
            s.font = UiFont::for_dpi(dpi);
            s.apply_font();
            s.layout();
        });
    }

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
            "検索の小窓",
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
    use windows::Win32::UI::WindowsAndMessaging::{WINDOW_EX_STYLE, WINDOW_STYLE};
    // SAFETY: 親は作成中のウィンドウ。
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(ex),
            class,
            text,
            WINDOW_STYLE(style) | WS_CHILD | WS_VISIBLE,
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

fn text_of(h: HWND) -> String {
    // SAFETY: 長さ + 1 の領域を渡す。
    unsafe {
        let len = GetWindowTextLengthW(h).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(h, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }
}

fn set_text(h: HWND, text: &str) {
    let wide = to_wide(text);
    // SAFETY: NUL 終端の文字列は呼び出しの間有効。
    unsafe {
        let _ = SetWindowTextW(h, PCWSTR(wide.as_ptr()));
    }
}

fn is_checked(h: HWND) -> bool {
    // SAFETY: 自分のチェックボックス。
    unsafe { SendMessageW(h, BM_GETCHECK, None, None).0 as usize == BST_CHECKED }
}

impl State {
    fn rebuild_brushes(&mut self) {
        // SAFETY: 自分が作ったブラシを消して作り直す。
        unsafe {
            for b in [self.bg_brush, self.list_brush] {
                if !b.is_invalid() {
                    let _ = DeleteObject(b.into());
                }
            }
            self.bg_brush = CreateSolidBrush(COLORREF(self.theme.ui.bar_bg.colorref()));
            self.list_brush = CreateSolidBrush(COLORREF(self.theme.editor.bg.colorref()));
        }
    }

    fn controls(&self) -> Vec<HWND> {
        let mut v = vec![
            self.query,
            self.replace,
            self.folder,
            self.case,
            self.word,
            self.regex,
            self.status,
            self.list,
        ];
        v.extend(self.buttons);
        v
    }

    fn repaint(&self) {
        // SAFETY: 自分のウィンドウと子ウィンドウ。
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, true);
            for h in self.controls() {
                let _ = InvalidateRect(Some(h), None, true);
            }
        }
    }

    fn apply_font(&self) {
        for h in self.controls() {
            // SAFETY: 書体は State が保持する間有効。
            unsafe {
                SendMessageW(
                    h,
                    WM_SETFONT,
                    Some(WPARAM(self.font.handle().0 as usize)),
                    Some(LPARAM(1)),
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
        let d = self.dpi;
        let (m, row, gap) = (scale(8, d), scale(26, d), scale(6, d));
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        let inner = (w - 2 * m).max(0);
        let mut y = m;
        let mv = |hwnd: HWND, x: i32, y: i32, w: i32, h: i32| {
            // SAFETY: 自分の子ウィンドウ。
            unsafe {
                let _ = MoveWindow(hwnd, x, y, w.max(0), h, true);
            }
        };
        for e in [self.query, self.replace, self.folder] {
            mv(e, m, y, inner, row);
            y += row + gap;
        }
        let cw = inner / 3;
        for (i, c) in [self.case, self.word, self.regex].into_iter().enumerate() {
            mv(c, m + cw * i as i32, y, cw, row);
        }
        y += row + gap;
        let bw = (inner - 5 * gap) / 6;
        for (i, b) in self.buttons.into_iter().enumerate() {
            mv(b, m + (bw + gap) * i as i32, y, bw, row + gap);
        }
        y += row + 2 * gap;
        mv(self.status, m, y, inner, scale(20, d));
        y += scale(20, d) + gap;
        mv(self.list, m, y, inner, (h - y - m).max(0));
    }

    fn options(&self) -> SearchOptions {
        SearchOptions {
            query: text_of(self.query),
            regex: is_checked(self.regex),
            case_sensitive: is_checked(self.case),
            whole_word: is_checked(self.word),
        }
    }

    fn show_status(&mut self, text: &str, error: bool) {
        self.status_is_error = error;
        set_text(self.status, text);
        // SAFETY: 自分の子ウィンドウ（色が変わるので描き直す）。
        unsafe {
            let _ = InvalidateRect(Some(self.status), None, true);
        }
    }

    /// 検索欄のパターンを検査し、無効なら状態欄にエラーを出す。有効（または空）なら消す。
    fn validate(&mut self) -> bool {
        let options = self.options();
        if options.query.is_empty() {
            self.show_status("", false);
            return false;
        }
        match options.compile() {
            Ok(_) => {
                self.show_status("", false);
                true
            }
            Err(e) => {
                self.show_status(&format!("無効なパターン: {}", reason_of(&e)), true);
                false
            }
        }
    }

    fn emit(&self, event: FindEvent) {
        (self.on_event)(event);
    }

    fn find(&mut self, forward: bool) {
        if !self.validate() {
            return;
        }
        let o = self.options();
        self.emit(if forward {
            FindEvent::Next(o)
        } else {
            FindEvent::Prev(o)
        });
    }

    fn replace(&mut self, all: bool) {
        if !self.replace_enabled || !self.validate() {
            return;
        }
        let options = self.options();
        let replacement = text_of(self.replace);
        self.emit(if all {
            FindEvent::ReplaceAll {
                options,
                replacement,
            }
        } else {
            FindEvent::Replace {
                options,
                replacement,
            }
        });
    }

    /// Grep を作業スレッドで始める（前の Grep は中止）。結果は世代で照合する。
    fn start_grep(&mut self) {
        self.stop_grep();
        if !self.validate() {
            if self.options().query.is_empty() {
                self.show_status("検索する文字列を入力してください", true);
            }
            return;
        }
        let root = PathBuf::from(text_of(self.folder).trim());
        if !root.is_dir() {
            self.show_status("フォルダーが見つかりません", true);
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        let cancel = CancelToken::new();
        self.cancel = Some(cancel.clone());
        self.hits.clear();
        self.grep_root = root.clone();
        // SAFETY: 自分のリストボックス。
        unsafe {
            SendMessageW(self.list, LB_RESETCONTENT, None, None);
        }
        self.show_status("検索しています…", false);
        let options = GrepOptions::new(root, self.options());
        let tx = self.tx.clone();
        WorkerPool::global().spawn(move || {
            let ctx = TaskContext {
                cancel,
                ..TaskContext::detached()
            };
            let result = grep(&options, &ctx, &mut |file| {
                tx.send(Msg::Files {
                    generation,
                    files: vec![file],
                });
            })
            .map(|o| (o.value, o.completion));
            tx.send(Msg::Done { generation, result });
        });
    }

    fn stop_grep(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.cancel();
        }
    }

    fn open_selected(&self) {
        // SAFETY: 自分のリストボックス。
        let i = unsafe { SendMessageW(self.list, LB_GETCURSEL, None, None) }.0;
        if let Some(hit) = usize::try_from(i).ok().and_then(|i| self.hits.get(i)) {
            self.emit(FindEvent::OpenHit {
                path: hit.path.clone(),
                line: hit.line,
                column: hit.column,
            });
        }
    }

    fn drain(&mut self) {
        let msgs: Vec<Msg> = self.rx.drain().collect();
        for msg in msgs {
            match msg {
                Msg::Files { generation, files } if generation == self.generation => {
                    for file in files {
                        for hit in file.hits {
                            let wide = to_wide(&hit_label(&self.grep_root, &hit));
                            // SAFETY: 呼び出しの間有効な文字列を渡す。
                            unsafe {
                                let n = SendMessageW(
                                    self.list,
                                    LB_ADDSTRING,
                                    None,
                                    Some(LPARAM(wide.as_ptr() as isize)),
                                );
                                if n.0 == 0 {
                                    SendMessageW(self.list, LB_SETCURSEL, Some(WPARAM(0)), None);
                                }
                            }
                            self.hits.push(hit);
                        }
                    }
                    let n = self.hits.len();
                    self.show_status(&format!("検索しています… {n} 件"), false);
                }
                Msg::Done { generation, result } if generation == self.generation => {
                    self.cancel = None;
                    match result {
                        Ok((summary, completion)) => {
                            self.show_status(&grep_status(&summary, completion), false)
                        }
                        Err(e) if e.is_cancelled() => self.show_status("中止しました", false),
                        Err(e) => self.show_status(&reason_of(&e), true),
                    }
                }
                _ => {}
            }
        }
    }
}

fn reason_of(e: &Error) -> String {
    match e {
        Error::InvalidArgument { reason, .. } => reason.clone(),
        other => other.to_string(),
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.stop_grep();
        // SAFETY: 自分が作ったブラシを 1 回だけ消す。
        unsafe {
            for b in [self.bg_brush, self.list_brush] {
                if !b.is_invalid() {
                    let _ = DeleteObject(b.into());
                }
            }
        }
    }
}

extern "system" fn panel_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            // SAFETY: lpCreateParams は create() が渡した Box。
            let params = unsafe {
                let cs = &*(lparam.0 as *const CREATESTRUCTW);
                Box::from_raw(cs.lpCreateParams as *mut CreateParams)
            };
            let CreateParams {
                theme,
                dpi,
                on_event,
            } = *params;
            let edit_style = WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32;
            let edit = |id| child(hwnd, w!("EDIT"), w!(""), edit_style, WS_EX_CLIENTEDGE.0, id);
            let check = |text: PCWSTR, id| {
                child(
                    hwnd,
                    w!("BUTTON"),
                    text,
                    WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32,
                    0,
                    id,
                )
            };
            let button = |text: PCWSTR, id| {
                child(
                    hwnd,
                    w!("BUTTON"),
                    text,
                    WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
                    0,
                    id,
                )
            };
            let (query, replace, folder) = (edit(ID_QUERY), edit(ID_REPLACE), edit(ID_FOLDER));
            let (case, word, regex) = (
                check(w!("大文字小文字を区別"), ID_CASE),
                check(w!("単語単位"), ID_WORD),
                check(w!("正規表現"), ID_REGEX),
            );
            let buttons = [
                button(w!("次へ"), ID_NEXT),
                button(w!("前へ"), ID_PREV),
                button(w!("置換"), ID_REPLACE_ONE),
                button(w!("すべて置換"), ID_REPLACE_ALL),
                button(w!("Grep"), ID_GREP),
                button(w!("中止"), ID_STOP),
            ];
            let status = child(hwnd, w!("STATIC"), w!(""), 0, 0, ID_STATUS);
            let list = child(
                hwnd,
                w!("LISTBOX"),
                w!(""),
                WS_TABSTOP.0 | WS_VSCROLL.0 | (LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32,
                WS_EX_CLIENTEDGE.0,
                ID_LIST,
            );
            let (tx, rx) = ui_channel(Arc::new(PostWaker(hwnd.0 as isize)));
            let mut state = Box::new(State {
                hwnd,
                query,
                replace,
                folder,
                case,
                word,
                regex,
                buttons,
                status,
                list,
                font: UiFont::for_dpi(dpi),
                dpi,
                bg_brush: HBRUSH::default(),
                list_brush: HBRUSH::default(),
                theme,
                status_is_error: false,
                replace_enabled: true,
                hits: Vec::new(),
                grep_root: PathBuf::new(),
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
                for h in [query, replace, folder] {
                    let old =
                        SetWindowLongPtrW(h, GWLP_WNDPROC, key_proc as *const () as usize as isize);
                    state.edit_proc = std::mem::transmute::<isize, WNDPROC>(old);
                }
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
            if let Some(brush) = with_state(hwnd, |s| s.bg_brush) {
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
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORSTATIC => {
            let ctl = HWND(lparam.0 as *mut _);
            let brush = with_state(hwnd, |s| {
                let hdc = HDC(wparam.0 as *mut _);
                let flat = msg == WM_CTLCOLORSTATIC || ctl == s.case;
                let (fg, bg, brush) = if flat {
                    let fg = if s.status_is_error {
                        s.theme.ui.status_warn
                    } else {
                        s.theme.editor.fg
                    };
                    (fg, s.theme.ui.bar_bg, s.bg_brush)
                } else {
                    (s.theme.editor.fg, s.theme.editor.bg, s.list_brush)
                };
                // SAFETY: wparam は子ウィンドウの DC。
                unsafe {
                    SetTextColor(hdc, COLORREF(fg.colorref()));
                    SetBkColor(hdc, COLORREF(bg.colorref()));
                }
                brush
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
                (ID_QUERY, EN_CHANGE) | (ID_CASE | ID_WORD | ID_REGEX, BN_CLICKED) => {
                    s.validate();
                }
                (ID_NEXT, BN_CLICKED) => s.find(true),
                (ID_PREV, BN_CLICKED) => s.find(false),
                (ID_REPLACE_ONE, BN_CLICKED) => s.replace(false),
                (ID_REPLACE_ALL, BN_CLICKED) => s.replace(true),
                (ID_GREP, BN_CLICKED) => s.start_grep(),
                (ID_STOP, BN_CLICKED) => s.stop_grep(),
                (ID_LIST, LBN_DBLCLK) => s.open_selected(),
                _ => {}
            });
            LRESULT(0)
        }
        WM_PANEL_RESULT => {
            with_state(hwnd, |s| s.drain());
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if wparam.0 & 0xFFFF != 0 {
                with_state(hwnd, |s| {
                    // SAFETY: 自分の子ウィンドウ。
                    unsafe {
                        let _ = SetFocus(Some(s.query));
                    }
                });
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // 破棄せず隠す。
            // SAFETY: 自分のウィンドウ。
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            with_state(hwnd, |s| {
                s.stop_grep();
                s.emit(FindEvent::Closed);
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

/// 入力欄と一覧のキー操作（Enter、Esc）。ほかは元の処理に渡す。
extern "system" fn key_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: 親は小窓。
    let parent =
        unsafe { windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd).unwrap_or_default() };
    let key = wparam.0 as u16;
    let handled = match msg {
        WM_KEYDOWN if key == VK_ESCAPE.0 => {
            // SAFETY: 小窓に WM_CLOSE を送る。
            unsafe {
                SendMessageW(parent, WM_CLOSE, None, None);
            }
            true
        }
        WM_KEYDOWN if key == VK_RETURN.0 => {
            // SAFETY: キー状態の問い合わせだけ。
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            with_state(parent, |s| {
                if hwnd == s.query {
                    s.find(!shift);
                } else if hwnd == s.replace {
                    s.replace(false);
                } else if hwnd == s.folder {
                    s.start_grep();
                } else {
                    s.open_selected();
                }
            });
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
        if hwnd == s.list {
            s.list_proc
        } else {
            s.edit_proc
        }
    })
    .flatten();
    // SAFETY: 差し替え前の処理にそのまま渡す。
    unsafe { CallWindowProcW(orig, hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str, line: u64, text: &str) -> GrepHit {
        GrepHit {
            path: PathBuf::from(path),
            line,
            column: 1,
            text: text.into(),
            ranges: vec![],
            encoding: "utf-8",
        }
    }

    #[test]
    fn label_is_relative_and_single_line() {
        let h = hit("/r/sub/a.txt", 3, "\tfoo\tbar ");
        assert_eq!(hit_label(Path::new("/r"), &h), "sub/a.txt:3:  foo bar");
        assert_eq!(hit_label(Path::new("/x"), &h), "/r/sub/a.txt:3:  foo bar");
    }

    #[test]
    fn status_mentions_truncation() {
        let s = GrepSummary {
            files_scanned: 5,
            files_skipped: 1,
            hits: 7,
        };
        assert!(grep_status(&s, Completion::Complete).starts_with("7 件"));
        assert!(grep_status(&s, Completion::ItemLimit).contains("上限"));
        assert!(grep_status(&s, Completion::Cancelled).contains("中止"));
    }
}
