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
use windows::Win32::UI::Controls::ShowScrollBar;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetKeyState, SetFocus, VK_ESCAPE, VK_RETURN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowExW, GetClientRect,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible,
    LoadCursorW, MoveWindow, PostMessageW, RegisterClassExW, SendMessageW, SetWindowLongPtrW,
    SetWindowPos, SetWindowTextW, ShowWindow, BM_GETCHECK, BM_SETCHECK, BN_CLICKED,
    BS_AUTOCHECKBOX, BS_PUSHBUTTON, CREATESTRUCTW, ES_AUTOHSCROLL, GWLP_USERDATA, GWLP_WNDPROC,
    HMENU, IDC_ARROW, LBN_DBLCLK, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, LB_ADDSTRING, LB_GETCURSEL,
    LB_RESETCONTENT, LB_SETCURSEL, MINMAXINFO, SB_VERT, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_HIDE, SW_SHOW, WM_ACTIVATE, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLOREDIT,
    WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN,
    WM_NCDESTROY, WM_SETFONT, WM_SIZE, WNDCLASSEXW, WNDPROC, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN,
    WS_EX_CLIENTEDGE, WS_EX_TOOLWINDOW, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_THICKFRAME,
    WS_VISIBLE, WS_VSCROLL,
};

use crate::appdata::settings::GrepSettings;
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
/// 窓の高さを用途と内容に合わせ直す（State を借りていない所で行うため、投函して後で）。
const WM_PANEL_FIT: u32 = WM_APP_FIRST_FREE + 1;
/// 窓の縁と項目の間の余白（96 DPI の px）。
const MARGIN: i32 = 12;

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
const ID_INSEL: usize = 215;
const ID_EXTENSIONS: usize = 216;
const ID_EXCLUDE_DIRS: usize = 217;
const ID_INCLUDE_HIDDEN: usize = 218;
const ID_MODE_FIND: usize = 219;
const ID_MODE_REPLACE: usize = 220;
const ID_MODE_GREP: usize = 221;

const BS_AUTORADIOBUTTON: u32 = 0x0009;
const BS_PUSHLIKE: u32 = 0x1000;
const BS_DEFPUSHBUTTON: u32 = 0x0001;
const BM_SETSTYLE: u32 = 0x00F4;
const SS_CENTERIMAGE: u32 = 0x0200;
const EM_SETCUEBANNER: u32 = 0x1501;
const CB_SETCUEBANNER: u32 = 0x1703;
const CB_SETITEMHEIGHT: u32 = 0x0153;

/// 小窓の用途。用途に関係する項目とボタンだけを出す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindMode {
    /// 文書内の検索。
    Find,
    /// 文書内の置換。
    Replace,
    /// フォルダーの中のファイルを検索。
    Grep,
}

/// コンボボックスのメッセージと通知（`CBS_DROPDOWN`: 入力もできる一覧）。
const CBS_DROPDOWN: u32 = 0x0002;
const CBN_SELCHANGE: u32 = 1;
const CBN_EDITCHANGE: u32 = 5;
const CB_ADDSTRING: u32 = 0x0143;
const CB_RESETCONTENT: u32 = 0x014B;
const CB_GETCURSEL: u32 = 0x0147;
const CB_GETLBTEXT: u32 = 0x0148;
const CB_GETLBTEXTLEN: u32 = 0x0149;
const CB_SETEDITSEL: u32 = 0x0142;
/// 閉じているときの高さに足す、開いた一覧の高さ（96 DPI の px）。
const DROPDOWN_EXTRA: i32 = 220;
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
    /// 「選択範囲内」のチェックが切り替わった（`true` ならオンにした時点の選択範囲が対象）。
    Scope(bool),
    /// 検索・置換・Grep を実行した。履歴に残す語（実行しなかった欄は `None`）。
    Ran {
        find: String,
        replace: Option<String>,
        grep_dir: Option<String>,
    },
    /// Grep を実行したときの絞り込み（設定へ保存する）。
    GrepSettings(GrepSettings),
    /// 検索欄の条件が変わった（入力・オプションの切り替え）。強調表示の更新に使う。
    /// 空や無効なパターンでも届く。
    Changed(SearchOptions),
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
    in_selection: HWND,
    extensions: HWND,
    exclude_dirs: HWND,
    include_hidden: HWND,
    /// 各欄の名前（検索、置換後、フォルダー、対象の拡張子、除外フォルダー）。
    labels: [HWND; 5],
    /// 用途の切り替え（検索｜置換｜Grep）。
    mode_buttons: [HWND; 3],
    mode: FindMode,
    /// Grep の用途で使っていた窓の高さ（ほかの用途から戻したときに使う）。
    grep_height: i32,
    /// コンボボックスと Grep 条件の入力欄（キー操作を受ける）。
    edits: [HWND; 6],
    /// 履歴の差し替え中は入力の通知を無視する。
    quiet: bool,
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
    edit_procs: [WNDPROC; 6],
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
                scale(610, dpi),
                Some(owner),
                None,
                Some(instance.into()),
                Some(raw as *const _),
            )
        };
        match created {
            Ok(hwnd) => {
                let panel = FindPanel { hwnd };
                panel.fit_to_mode();
                panel.center_on(owner);
                Ok(panel)
            }
            Err(e) => Err(Error::unavailable(
                "検索の小窓",
                format!("作れませんでした: {e}"),
            )),
        }
    }

    /// 用途に合わせて窓の高さを決める（検索・置換は内容の高さ、Grep は結果の一覧の分も）。
    fn fit_to_mode(&self) {
        let Some((min_w, min_h, grep, remembered)) = with_state(self.hwnd, |s| {
            let (w, h) = s.minimum_outer_size();
            (w, h, s.mode == FindMode::Grep, s.grep_height)
        }) else {
            return;
        };
        let mut rect = RECT::default();
        // SAFETY: 大きさを読んでから変える（WM_SIZE が State を借りられるよう、借用の外で）。
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
            let width = (rect.right - rect.left).max(min_w);
            let height = if grep { remembered.max(min_h) } else { min_h };
            let _ = SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                width,
                height,
                SWP_NOMOVE | SWP_NOZORDER,
            );
        }
    }

    /// 用途を切り替える（項目・ボタン・窓の高さが変わる）。
    pub fn set_mode(&self, mode: FindMode) {
        let changed = with_state(self.hwnd, |s| s.switch_mode(mode)).unwrap_or(false);
        if changed {
            self.fit_to_mode();
        }
    }

    /// いまの用途。
    pub fn mode(&self) -> FindMode {
        with_state(self.hwnd, |s| s.mode).unwrap_or(FindMode::Find)
    }

    /// 用途を選んで表示する。
    pub fn show_mode(&self, mode: FindMode, prefill: Option<&str>) {
        self.set_mode(mode);
        self.show(prefill);
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
                SendMessageW(
                    s.query,
                    CB_SETEDITSEL,
                    Some(WPARAM(0)),
                    Some(LPARAM(-1 << 16)),
                );
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

    /// 3 つの欄の履歴（新しい順）を差し替える。入力中の文字列は保つ。
    pub fn set_history(&self, find: &[String], replace: &[String], grep_dir: &[String]) {
        with_state(self.hwnd, |s| {
            s.quiet = true;
            for (h, list) in [(s.query, find), (s.replace, replace), (s.folder, grep_dir)] {
                let text = text_of(h);
                // SAFETY: 自分のコンボボックス。文字列は呼び出しの間有効。
                unsafe {
                    SendMessageW(h, CB_RESETCONTENT, None, None);
                    for item in list {
                        let wide = to_wide(item);
                        SendMessageW(h, CB_ADDSTRING, None, Some(LPARAM(wide.as_ptr() as isize)));
                    }
                }
                set_text(h, &text);
            }
            s.quiet = false;
        });
    }

    /// 「選択範囲内」のチェックを付け外しする（通知は出ない）。
    pub fn set_in_selection(&self, on: bool) {
        with_state(self.hwnd, |s| {
            // SAFETY: 自分のチェックボックス。
            unsafe {
                SendMessageW(s.in_selection, BM_SETCHECK, Some(WPARAM(on as usize)), None);
            }
        });
    }

    /// 「選択範囲内」がオンか。
    pub fn in_selection(&self) -> bool {
        with_state(self.hwnd, |s| is_checked(s.in_selection)).unwrap_or(false)
    }

    /// 検索欄の文字列を差し替える（条件が変わった通知が届く）。
    pub fn set_query(&self, text: &str) {
        with_state(self.hwnd, |s| {
            // コンボボックスは文字列の差し替えを通知しないことがあるので、通知は自分で出す。
            s.quiet = true;
            set_text(s.query, text);
            s.quiet = false;
            s.validate();
            s.emit(FindEvent::Changed(s.options()));
        });
    }

    /// 保存済みの Grep の絞り込みを反映する（履歴の更新では入力中の条件を変えない）。
    pub fn set_grep_settings(&self, settings: &GrepSettings) {
        with_state(self.hwnd, |s| {
            set_text(s.extensions, &settings.extensions);
            set_text(s.exclude_dirs, &settings.exclude_dirs);
            // SAFETY: 自分のチェックボックス。
            unsafe {
                SendMessageW(
                    s.include_hidden,
                    BM_SETCHECK,
                    Some(WPARAM(settings.include_hidden as usize)),
                    None,
                );
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
            s.fit_combo_heights();
            s.layout();
        });
        self.fit_to_mode();
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

/// コンボボックスの一覧で選ばれている項目の文字列。
fn selected_text(combo: HWND) -> Option<String> {
    // SAFETY: 自分のコンボボックス。長さ + 1 の領域を渡す。
    unsafe {
        let i = SendMessageW(combo, CB_GETCURSEL, None, None).0;
        if i < 0 {
            return None;
        }
        let len = SendMessageW(combo, CB_GETLBTEXTLEN, Some(WPARAM(i as usize)), None).0;
        if len < 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = SendMessageW(
            combo,
            CB_GETLBTEXT,
            Some(WPARAM(i as usize)),
            Some(LPARAM(buf.as_mut_ptr() as isize)),
        )
        .0;
        (n >= 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
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
            self.in_selection,
            self.extensions,
            self.exclude_dirs,
            self.include_hidden,
            self.status,
            self.list,
        ];
        v.extend(self.buttons);
        v.extend(self.labels);
        v.extend(self.mode_buttons);
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

    fn row_height(&self) -> i32 {
        self.font.measure_text("あAg", None).cy + scale(10, self.dpi)
    }

    /// コンボボックスの入力部分を、ほかの入力欄と同じ高さにする。
    fn fit_combo_heights(&self) {
        let h = (self.row_height() - scale(6, self.dpi)).max(1);
        for c in [self.query, self.replace, self.folder] {
            // SAFETY: 自分のコンボボックス。wparam = -1 は入力部分。
            unsafe {
                SendMessageW(
                    c,
                    CB_SETITEMHEIGHT,
                    Some(WPARAM(usize::MAX)),
                    Some(LPARAM(h as isize)),
                );
            }
        }
    }

    /// この用途で出す項目か。
    fn visible_in_mode(&self, h: HWND) -> bool {
        let grep = self.mode == FindMode::Grep;
        let replace = self.mode == FindMode::Replace;
        let [_, label_replace, label_folder, label_ext, label_exclude] = self.labels;
        let [next, prev, replace_one, replace_all, grep_button, stop] = self.buttons;
        if h == self.replace || h == label_replace || h == replace_one || h == replace_all {
            replace
        } else if [
            self.folder,
            self.extensions,
            self.exclude_dirs,
            self.include_hidden,
            label_folder,
            label_ext,
            label_exclude,
            grep_button,
            stop,
            self.list,
        ]
        .contains(&h)
        {
            grep
        } else if h == prev {
            self.mode == FindMode::Find
        } else if h == self.in_selection || h == next {
            !grep
        } else {
            true
        }
    }

    /// 用途を切り替える。変わったら `true`。
    fn switch_mode(&mut self, mode: FindMode) -> bool {
        if self.mode == mode && self.mode_checked() == Some(mode) {
            return false;
        }
        if self.mode == FindMode::Grep {
            let mut r = RECT::default();
            // SAFETY: 自分のウィンドウ。
            unsafe {
                let _ = GetWindowRect(self.hwnd, &mut r);
            }
            self.grep_height = r.bottom - r.top;
        }
        self.mode = mode;
        for (i, b) in self.mode_buttons.into_iter().enumerate() {
            // SAFETY: 自分のラジオボタン。
            unsafe {
                SendMessageW(
                    b,
                    BM_SETCHECK,
                    Some(WPARAM((i == mode as usize) as usize)),
                    None,
                );
            }
        }
        let [next, _, replace_one, _, grep_button, _] = self.buttons;
        let default = match mode {
            FindMode::Find => next,
            FindMode::Replace => replace_one,
            FindMode::Grep => grep_button,
        };
        for b in self.buttons {
            let style = if b == default {
                BS_DEFPUSHBUTTON
            } else {
                BS_PUSHBUTTON as u32
            };
            // SAFETY: 自分のボタン。見た目（既定の枠）だけを変える。
            unsafe {
                SendMessageW(
                    b,
                    BM_SETSTYLE,
                    Some(WPARAM(style as usize)),
                    Some(LPARAM(1)),
                );
            }
        }
        for h in self.controls() {
            if h == self.status {
                continue;
            }
            let show = self.visible_in_mode(h);
            // SAFETY: 自分の子ウィンドウ。
            unsafe {
                let _ = ShowWindow(h, if show { SW_SHOW } else { SW_HIDE });
            }
        }
        self.update_stop();
        self.layout();
        true
    }

    fn mode_checked(&self) -> Option<FindMode> {
        let modes = [FindMode::Find, FindMode::Replace, FindMode::Grep];
        self.mode_buttons
            .iter()
            .position(|&b| is_checked(b))
            .map(|i| modes[i])
    }

    /// 中止は Grep の実行中だけ押せる。
    fn update_stop(&self) {
        // SAFETY: 自分のボタン。
        unsafe {
            let _ = EnableWindow(self.buttons[5], self.cancel.is_some());
        }
    }

    fn minimum_client_size(&self) -> (i32, i32) {
        let px = |n| scale(n, self.dpi);
        let m = px(MARGIN);
        let (form_bottom, _) = self.form_layout(px(420));
        let width = self.minimum_form_width() + 2 * m;
        let height = if self.mode == FindMode::Grep {
            form_bottom + px(8) + self.row_height() * 5 + m
        } else {
            form_bottom + m
        };
        (width, height)
    }

    fn minimum_form_width(&self) -> i32 {
        let px = |n| scale(n, self.dpi);
        let label = self.label_width();
        let buttons: i32 = self
            .buttons
            .iter()
            .filter(|h| self.visible_in_mode(**h))
            .map(|h| self.button_width(*h) + px(8))
            .sum();
        let options: i32 = [self.case, self.word, self.regex]
            .iter()
            .map(|h| self.check_width(*h) + px(16))
            .sum();
        let modes: i32 = self
            .mode_buttons
            .iter()
            .map(|h| self.button_width(*h))
            .sum();
        (label + options).max(buttons).max(modes).max(px(360))
    }

    fn label_width(&self) -> i32 {
        self.labels
            .iter()
            .map(|h| self.font.measure_text(&text_of(*h), None).cx)
            .max()
            .unwrap_or(0)
            + scale(12, self.dpi)
    }

    fn button_width(&self, h: HWND) -> i32 {
        (self.font.measure_text(&text_of(h), None).cx + scale(28, self.dpi))
            .max(scale(88, self.dpi))
    }

    fn check_width(&self, h: HWND) -> i32 {
        self.font.measure_text(&text_of(h), None).cx + scale(24, self.dpi)
    }

    fn minimum_outer_size(&self) -> (i32, i32) {
        let (w, h) = self.minimum_client_size();
        let (mut outer, mut client) = (RECT::default(), RECT::default());
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut outer);
            let _ = GetClientRect(self.hwnd, &mut client);
        }
        (
            w + outer.right - outer.left - client.right + client.left,
            h + outer.bottom - outer.top - client.bottom + client.top,
        )
    }

    /// 上から順に項目を並べる。`apply` が偽なら位置を計算するだけ。
    /// 戻り値は（項目の下端、ボタンの行の上端）。
    fn place_form(&self, inner: i32, apply: bool) -> (i32, i32) {
        let d = self.dpi;
        let px = |n| scale(n, d);
        let (m, row, gap) = (px(MARGIN), self.row_height(), px(8));
        let mv = |hwnd: HWND, x: i32, y: i32, w: i32, h: i32| {
            if apply {
                // SAFETY: 自分の子ウィンドウ。
                unsafe {
                    let _ = MoveWindow(hwnd, x, y, w.max(0), h, true);
                }
            }
        };
        let mut y = m;
        // 用途の切り替え（左寄せの区切りボタン）。
        let mut x = m;
        for b in self.mode_buttons {
            let w = self.button_width(b);
            mv(b, x, y, w, row);
            x += w;
        }
        y += row + px(12);
        // 名前つきの欄。
        let label_w = self.label_width();
        let field_w = inner - label_w;
        let fields = [
            (self.labels[0], self.query, true),
            (self.labels[1], self.replace, true),
            (self.labels[2], self.folder, true),
            (self.labels[3], self.extensions, false),
            (self.labels[4], self.exclude_dirs, false),
        ];
        for (label, input, combo) in fields {
            if !self.visible_in_mode(input) {
                continue;
            }
            mv(label, m, y, label_w, row);
            // コンボボックスの高さには開いた一覧の分も含める（閉じた見た目は変わらない）。
            let h = if combo { row + px(DROPDOWN_EXTRA) } else { row };
            mv(input, m + label_w, y, field_w, h);
            y += row + gap;
        }
        // オプション（欄の位置に揃えて横に並べる）。
        let mut x = m + label_w;
        for c in [self.case, self.word, self.regex] {
            let w = self.check_width(c);
            mv(c, x, y, w, row);
            x += w + px(16);
        }
        y += row;
        let second = if self.mode == FindMode::Grep {
            self.include_hidden
        } else {
            self.in_selection
        };
        mv(second, m + label_w, y, self.check_width(second), row);
        y += row + gap;
        // 状態文（空なら場所を取らない）。
        let status_text = text_of(self.status);
        if status_text.is_empty() {
            mv(self.status, m, y, inner, 0);
        } else {
            let line_h = self.font.measure_text("あAg", None).cy.max(1);
            let text_h = self
                .font
                .measure_text(&status_text, Some((inner - px(24)).max(1)))
                .cy;
            let status_h = text_h.clamp(line_h, line_h * 3) + px(4);
            mv(self.status, m, y, inner, status_h);
            if apply {
                // 収まらないときだけスクロールバーを出す。
                // SAFETY: 自分の子ウィンドウ。
                unsafe {
                    let _ = ShowScrollBar(self.status, SB_VERT, text_h > line_h * 3);
                }
            }
            y += status_h + gap;
        }
        // ボタン（右寄せ。既定のボタンを先頭に）。
        let buttons: Vec<HWND> = match self.mode {
            FindMode::Find => vec![self.buttons[0], self.buttons[1]],
            FindMode::Replace => vec![self.buttons[2], self.buttons[3], self.buttons[0]],
            FindMode::Grep => vec![self.buttons[4], self.buttons[5]],
        };
        let total: i32 = buttons.iter().map(|b| self.button_width(*b)).sum::<i32>()
            + gap * (buttons.len() as i32 - 1);
        let mut x = m + inner - total;
        let button_top = y;
        for b in buttons {
            let w = self.button_width(b);
            mv(b, x, y, w, row + px(2));
            x += w + gap;
        }
        y += row + px(2);
        (y, button_top)
    }

    /// 内容に合わせて窓の高さを直す（後で。Grep 以外）。
    fn fit_height(&self) {
        // SAFETY: 自分のウィンドウに投函するだけ。
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_PANEL_FIT, WPARAM(0), LPARAM(0));
        }
    }

    fn form_layout(&self, inner: i32) -> (i32, i32) {
        self.place_form(inner, false)
    }

    fn layout(&self) {
        let mut rc = RECT::default();
        // SAFETY: 自分のウィンドウ。
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let m = scale(MARGIN, self.dpi);
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        let inner = (w - 2 * m).max(0);
        let (bottom, _) = self.place_form(inner, true);
        let y = bottom + scale(8, self.dpi);
        // SAFETY: 自分の子ウィンドウ。
        unsafe {
            let _ = MoveWindow(self.list, m, y, inner, (h - y - m).max(0), true);
        }
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
        if self.mode == FindMode::Grep {
            self.layout();
            // 動いた項目の跡を残さない（子ウィンドウも描き直す）。
            // SAFETY: 自分のウィンドウ。
            unsafe {
                use windows::Win32::Graphics::Gdi::{
                    RedrawWindow, RDW_ALLCHILDREN, RDW_ERASE, RDW_INVALIDATE,
                };
                let _ = RedrawWindow(
                    Some(self.hwnd),
                    None,
                    None,
                    RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
                );
            }
        } else {
            self.fit_height();
        }
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
        self.emit(FindEvent::Ran {
            find: o.query.clone(),
            replace: None,
            grep_dir: None,
        });
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
        self.emit(FindEvent::Ran {
            find: options.query.clone(),
            replace: Some(replacement.clone()),
            grep_dir: None,
        });
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
        self.update_stop();
        self.hits.clear();
        self.grep_root = root.clone();
        // SAFETY: 自分のリストボックス。
        unsafe {
            SendMessageW(self.list, LB_RESETCONTENT, None, None);
        }
        self.show_status("検索しています…", false);
        let settings = GrepSettings {
            extensions: text_of(self.extensions),
            exclude_dirs: text_of(self.exclude_dirs),
            include_hidden: is_checked(self.include_hidden),
        };
        let mut options = GrepOptions::new(root.clone(), self.options());
        options.extensions = crate::search::parse_extensions(&settings.extensions);
        options.exclude_dirs = crate::search::parse_exclude_dirs(&settings.exclude_dirs);
        options.include_hidden = settings.include_hidden;
        self.emit(FindEvent::GrepSettings(settings));
        self.emit(FindEvent::Ran {
            find: self.options().query,
            replace: None,
            grep_dir: Some(root.to_string_lossy().into_owned()),
        });
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
        self.update_stop();
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
            // 履歴の一覧つきの入力欄（`CBS_DROPDOWN`）。
            let edit_style = WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWN | ES_AUTOHSCROLL as u32;
            let edit = |id| child(hwnd, w!("COMBOBOX"), w!(""), edit_style, 0, id);
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
            let in_selection = check(w!("選択範囲内だけ"), ID_INSEL);
            let extensions = child(
                hwnd,
                w!("EDIT"),
                w!(""),
                WS_TABSTOP.0 | ES_AUTOHSCROLL as u32,
                WS_EX_CLIENTEDGE.0,
                ID_EXTENSIONS,
            );
            let exclude_dirs = child(
                hwnd,
                w!("EDIT"),
                w!(""),
                WS_TABSTOP.0 | ES_AUTOHSCROLL as u32,
                WS_EX_CLIENTEDGE.0,
                ID_EXCLUDE_DIRS,
            );
            let include_hidden = check(w!("隠しファイルも対象"), ID_INCLUDE_HIDDEN);
            let label = |text: PCWSTR| child(hwnd, w!("STATIC"), text, SS_CENTERIMAGE, 0, 0);
            let labels = [
                label(w!("検索する文字列")),
                label(w!("置換後の文字列")),
                label(w!("フォルダー")),
                label(w!("対象の拡張子")),
                label(w!("除外フォルダー")),
            ];
            let mode_button = |text: PCWSTR, id, first: bool| {
                let group = if first {
                    0x0002_0000 /* WS_GROUP */
                } else {
                    0
                };
                child(
                    hwnd,
                    w!("BUTTON"),
                    text,
                    WS_TABSTOP.0 | BS_AUTORADIOBUTTON | BS_PUSHLIKE | group,
                    0,
                    id,
                )
            };
            let mode_buttons = [
                mode_button(w!("検索"), ID_MODE_FIND, true),
                mode_button(w!("置換"), ID_MODE_REPLACE, false),
                mode_button(w!("Grep"), ID_MODE_GREP, false),
            ];
            // 入力欄の中の薄い案内（空のときだけ見える）。
            for (h, text, combo) in [
                (
                    query,
                    "検索する語（Enter で次へ、Shift+Enter で前へ）",
                    true,
                ),
                (replace, "置き換える語（空なら削除）", true),
                (folder, "検索するフォルダー", true),
                (extensions, "例: *.rs;*.txt（空ならすべて）", false),
                (exclude_dirs, "; で区切る", false),
            ] {
                let wide = to_wide(text);
                let msg = if combo {
                    CB_SETCUEBANNER
                } else {
                    EM_SETCUEBANNER
                };
                // SAFETY: 自分の子ウィンドウ。文字列は呼び出しの間有効（コピーされる）。
                unsafe {
                    SendMessageW(
                        h,
                        msg,
                        Some(WPARAM(1)),
                        Some(LPARAM(wide.as_ptr() as isize)),
                    );
                }
            }
            let buttons = [
                button(w!("次を検索"), ID_NEXT),
                button(w!("前を検索"), ID_PREV),
                button(w!("置換"), ID_REPLACE_ONE),
                button(w!("すべて置換"), ID_REPLACE_ALL),
                button(w!("検索開始"), ID_GREP),
                button(w!("中止"), ID_STOP),
            ];
            // Read-only multi-line status keeps long regex diagnostics accessible by scrolling.
            let status = child(
                hwnd,
                w!("EDIT"),
                w!(""),
                0x0004 /* ES_MULTILINE */ | 0x0800 /* ES_READONLY */
                    | 0x0040 /* ES_AUTOVSCROLL */ | WS_VSCROLL.0,
                0,
                ID_STATUS,
            );
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
                in_selection,
                extensions,
                exclude_dirs,
                include_hidden,
                labels,
                mode_buttons,
                mode: FindMode::Find,
                grep_height: scale(600, dpi),
                edits: [HWND::default(); 6],
                quiet: false,
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
                edit_procs: [None; 6],
                list_proc: None,
            });
            state.rebuild_brushes();
            state.apply_font();
            // SAFETY: 元の処理を控えて、キー操作を受ける処理に差し替える。
            unsafe {
                for (i, combo) in [query, replace, folder, extensions, exclude_dirs, status]
                    .into_iter()
                    .enumerate()
                {
                    // コンボボックスの中の入力欄がキー操作を受ける。
                    let h = FindWindowExW(Some(combo), None, w!("EDIT"), None).unwrap_or(combo);
                    state.edits[i] = h;
                    let old =
                        SetWindowLongPtrW(h, GWLP_WNDPROC, key_proc as *const () as usize as isize);
                    state.edit_procs[i] = std::mem::transmute::<isize, WNDPROC>(old);
                }
                let old =
                    SetWindowLongPtrW(list, GWLP_WNDPROC, key_proc as *const () as usize as isize);
                state.list_proc = std::mem::transmute::<isize, WNDPROC>(old);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
            }
            with_state(hwnd, |s| {
                set_dark_title_bar(hwnd, s.theme.dark);
                s.fit_combo_heights();
                s.switch_mode(FindMode::Find);
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
                (ID_QUERY | ID_REPLACE | ID_FOLDER, CBN_SELCHANGE) => {
                    // 一覧で選んだ語を入力欄に入れる（通知の時点では入力欄はまだ古い）。
                    let combo = match id {
                        ID_QUERY => s.query,
                        ID_REPLACE => s.replace,
                        _ => s.folder,
                    };
                    if let Some(text) = selected_text(combo) {
                        set_text(combo, &text);
                    }
                }
                (ID_QUERY, CBN_EDITCHANGE) | (ID_CASE | ID_WORD | ID_REGEX, BN_CLICKED)
                    if !s.quiet =>
                {
                    s.validate();
                    s.emit(FindEvent::Changed(s.options()));
                }
                (ID_MODE_FIND | ID_MODE_REPLACE | ID_MODE_GREP, BN_CLICKED) => {
                    let mode = match id {
                        ID_MODE_FIND => FindMode::Find,
                        ID_MODE_REPLACE => FindMode::Replace,
                        _ => FindMode::Grep,
                    };
                    if s.switch_mode(mode) {
                        s.fit_height();
                    }
                }
                (ID_INSEL, BN_CLICKED) => s.emit(FindEvent::Scope(is_checked(s.in_selection))),
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
            with_state(hwnd, |s| {
                s.drain();
                s.update_stop();
            });
            LRESULT(0)
        }
        WM_PANEL_FIT => {
            FindPanel { hwnd }.fit_to_mode();
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
    // SAFETY: 親は小窓（入力欄は、コンボボックスの中にあるので 1 つ上）。
    let parent = unsafe {
        use windows::Win32::UI::WindowsAndMessaging::GetParent;
        let p = GetParent(hwnd).unwrap_or_default();
        if with_state(p, |_| ()).is_some() {
            p
        } else {
            GetParent(p).unwrap_or_default()
        }
    };
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
                if hwnd == s.edits[0] && s.mode == FindMode::Grep {
                    s.start_grep();
                } else if hwnd == s.edits[0] {
                    s.find(!shift);
                } else if hwnd == s.edits[1] {
                    s.replace(false);
                } else if s.edits[2..5].contains(&hwnd) {
                    s.start_grep();
                } else if hwnd == s.list {
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
            s.edits
                .iter()
                .position(|&h| h == hwnd)
                .and_then(|i| s.edit_procs[i])
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
