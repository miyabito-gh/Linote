//! アプリの状態とメッセージループ。
//!
//! - 開いている文書の一覧（[`crate::document::DocumentState`] と表示部品の組）を持つ。
//! - [`crate::command::CommandHost`] を実装し、[`crate::command::Registry`] を保持する。
//!   `begin_edit`〜`end_edit` の間に届いた Scintilla の変更通知は、その実行者の変更として
//!   [`crate::document::DocumentState::record_edit`] に渡す。
//! - [`crate::core::UiWaker`] を `PostMessageW`（アプリ定義のメッセージ 1 種類）で実装し、
//!   そのメッセージを受けたら [`crate::core::UiReceiver::drain`] で結果を取り出す。
//!   取り出した結果は文書 ID と版を照合してから反映する。
//! - 1 プロセスにメインウィンドウは 1 つ。2 つ目のウィンドウは別プロセスになる。
//!
//! ## 再入と変更通知の扱い
//!
//! アプリの状態は UI スレッドのスレッドローカルに置き、ウィンドウプロシージャは
//! [`with_app`] で借りて処理する。Scintilla への `SendMessage` の途中で、Scintilla から
//! `WM_NOTIFY`（`SCN_MODIFIED` など）が同期的に届く（再入）。そこではアプリを借りず、
//! 通知を「どの Scintilla 文書の通知か」と一緒に待ち行列に積むだけにする。積んだ通知は
//! [`App::sync`] がまとめて文書の版に反映する。`sync` は、`with_app` の終わり、
//! `end_edit`、文書の切り替えの前などで呼ばれる。
//!
//! 再入中に届いたほかのメッセージ（モーダルなダイアログの間の `WM_PAINT` など）は既定の
//! 処理に回し、描画は外側の処理が終わってから描き直す。**ダイアログを開くなど、
//! メッセージループを回す処理をアプリを借りたまま行う場合（I1）は、この点に注意する。**
//!
//! 担当: タスク B（統合は I1）。

mod bigdoc;
mod closing;
mod commands;
mod editcmds;
mod fileio;
mod fileops;
mod highlight;
mod memos;
mod preview;
mod recover;
mod search;
pub mod selftest;
mod viewopts;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetDC, InvalidateRect, ReleaseDC, ScreenToClient, SelectObject,
    UpdateWindow, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos, GetMessageW, IsWindow, KillTimer,
    LoadCursorW, PostMessageW, PostQuitMessage, SendMessageW, SetCursor, SetTimer, SetWindowPos,
    SetWindowTextW, ShowWindow, TranslateMessage, HMENU, HTCLIENT, IDC_SIZEWE, MSG, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_HIDE, SW_SHOW, SW_SHOWDEFAULT, WM_ACTIVATE, WM_CAPTURECHANGED, WM_CLOSE,
    WM_COMMAND, WM_DESTROY, WM_DPICHANGED, WM_DROPFILES, WM_ERASEBKGND, WM_HOTKEY,
    WM_INITMENUPOPUP, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONUP, WM_MOUSEMOVE,
    WM_NOTIFY, WM_PAINT, WM_SETCURSOR, WM_SETFOCUS, WM_SETTINGCHANGE, WM_SIZE, WM_TIMER,
};

use crate::appdata::{AppPaths, Settings, ThemeMode};
use crate::bigfile::LargeModeThresholds;
use crate::command::{
    Call, CommandDef, CommandHost, CommandId, CommandOutput, Invocation, ParamDef, ParamType,
    Registry, TargetKind, Value,
};
use crate::core::{
    ui_channel, Actor, CancelToken, DocStamp, DocumentId, Error, Result, TargetRef, UiReceiver,
    UiSender, Version,
};
use crate::document::{Capabilities, Capability, DocumentState};
use crate::file::FileStamp;
use crate::memo::MemoTab;
use crate::platform::hotkey::GlobalHotkey;
use crate::platform::system;
use crate::recovery::RecoverySlot;
use crate::search::SearchOptions;
use crate::ui::dialogs::{Prompts, Win32Prompts};
use crate::ui::editor::{self, DocLoader, Editor, EditorEvent, Language, SciDoc, ViewState};
use crate::ui::find::FindPanel;
use crate::ui::memo_panel::MemoPanel;
use crate::ui::menu::{KeyMap, MenuModel};
use crate::ui::statusbar::{MessageKind, StatusBar, StatusInfo, StatusSegment};
use crate::ui::tabs::{TabBar, TabHit, TabItem};
use crate::ui::theme::Theme;
use crate::ui::window::{self, Layout, UiFont, WM_APP_REPAINT, WM_APP_WAKE};

/// 状態表示を消すタイマー。B が使うタイマー ID は 1〜99。ほかのタスクは 100 以降を使う。
const TIMER_STATUS: usize = 1;
/// 状態表示を出しておく時間（ミリ秒）。
const STATUS_MESSAGE_MS: u32 = 5000;
/// 復元データ・メモの書き出しの時刻を見るタイマー（統合 I1 が使う。100 以降）。
const TIMER_AUTOSAVE: usize = 110;
/// 書き出しの時刻を見る間隔（ミリ秒）。
const AUTOSAVE_TICK_MS: u32 = 500;

// ------------------------------------------------------------------ メッセージ

/// ワーカーから UI スレッドへ送るメッセージ（[`crate::core::ui_channel`] の `M`）。
///
/// 文書に関する結果は、`Run` の中で [`DocStamp`] を照合してから反映すること
/// （[`App::is_current`]）。統合（I1）で種類を足してよい。
pub enum UiMessage {
    /// 短い状態表示。
    Status(String),
    /// UI スレッドで実行する処理。
    Run(Box<dyn FnOnce(&mut App) + Send>),
}

// ------------------------------------------------------------------ 文書

/// 文書の表示部品。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocView {
    /// 編集欄（Scintilla）で表示する。本文はこのドキュメントにある。
    Scintilla(SciDoc),
    /// ほかの部品のウィンドウで表示する（巨大ファイルの表示など。タスク L）。
    /// 配置と表示・非表示は `App` が行い、破棄は持ち主が
    /// [`Hooks::document_closed`] で行う。
    External(HWND),
}

/// 開いている文書 1 つ。
#[derive(Debug)]
pub struct OpenDoc {
    pub state: DocumentState,
    pub title: String,
    pub language: Language,
    view: DocView,
    view_state: ViewState,
    /// Scintilla の保存位置が `state` の保存済みの版と対応しているか。
    /// 対応していなければ `SCN_SAVEPOINTREACHED` を無視する（保存していない内容を
    /// 未変更と見なさないため）。
    savepoint_valid: bool,
    /// 読み込みで不正なバイトを U+FFFD に置き換えた（上書き保存の前に確認する）。
    lossy: bool,
    /// 復元データの枠（初めて書くときに開く。復元した文書は引き継ぐ）。
    slot: Option<Arc<RecoverySlot>>,
    /// メモを開いている文書の、保存領域との対応。
    memo: Option<MemoTab>,
    /// 書き出していない編集（最初の編集, 最後の編集）。復元データ・メモの自動保存が使う。
    pending: Option<(Instant, Instant)>,
    /// 復元データ・メモの書き出しが進行中。
    write_in_flight: bool,
    /// 保存（ファイルへの書き出し）が進行中。
    saving: bool,
}

impl OpenDoc {
    fn new(
        state: DocumentState,
        title: String,
        language: Language,
        view: DocView,
        savepoint_valid: bool,
    ) -> OpenDoc {
        OpenDoc {
            state,
            title,
            language,
            view,
            view_state: ViewState::default(),
            savepoint_valid,
            lossy: false,
            slot: None,
            memo: None,
            pending: None,
            write_in_flight: false,
            saving: false,
        }
    }

    pub fn id(&self) -> DocumentId {
        self.state.id()
    }

    pub fn view(&self) -> DocView {
        self.view
    }
}

/// メインウィンドウのメッセージを最初に受け取るフック（[`Hooks::message`]）。
pub type MessageHook = fn(&mut App, u32, WPARAM, LPARAM) -> Option<LRESULT>;

/// 起動時に一度だけ呼ぶ準備（[`RunOptions::setup`]）。
pub type SetupFn = Box<dyn FnOnce(&mut App)>;

/// 統合側（I1 など）が差し込む処理。どれも省略できる（既定の動作がある）。
///
/// 関数ポインターにしてあるのは、`&mut App` を渡して呼べるようにするため。
#[derive(Clone, Copy)]
pub struct Hooks {
    /// メインウィンドウのメッセージを最初に受け取る。`Some` を返すと既定の処理をしない
    /// （メニューの `WM_COMMAND` など）。
    pub message: Option<MessageHook>,
    /// メッセージループで `TranslateMessage` の前に呼ぶ。`true` を返すと、そのメッセージは
    /// 処理済み（ショートカットキーなど）。
    pub pre_translate: Option<fn(&mut App, &MSG) -> bool>,
    /// 未保存の文書を閉じてよいか（確認ダイアログ。既定は保存／保存しない／取り消しの確認）。
    pub confirm_close: fn(&mut App, DocumentId) -> bool,
    /// 未保存の文書があるときに終了してよいか（既定は文書ごとの確認）。
    pub confirm_exit: fn(&mut App) -> bool,
    /// ステータスバーの項目がクリックされた。
    pub status_click: Option<fn(&mut App, StatusSegment)>,
    /// 明暗テーマが変わった（プレビューに配色を渡すなど）。
    pub theme_changed: Option<fn(&mut App)>,
    /// 文書の本文が変わった（自動保存・復元・プレビューのタイマーを始めるなど）。
    pub document_changed: Option<fn(&mut App, DocumentId)>,
    /// アクティブな文書が変わった。
    pub active_changed: Option<fn(&mut App, Option<DocumentId>)>,
    /// 文書を閉じた（一覧から外した後）。外部の表示部品の破棄などを行う。
    pub document_closed: Option<fn(&mut App, DocumentId, DocView)>,
    /// ウィンドウが前面に来た／離れた（外部変更の確認など）。
    pub window_activated: Option<fn(&mut App, bool)>,
}

impl Default for Hooks {
    fn default() -> Self {
        Hooks {
            message: None,
            pre_translate: None,
            confirm_close: fileio::confirm_close,
            confirm_exit: fileio::confirm_exit,
            status_click: None,
            theme_changed: None,
            document_changed: None,
            active_changed: None,
            document_closed: None,
            window_activated: None,
        }
    }
}

/// [`run`] の指定。
#[derive(Default)]
pub struct RunOptions {
    /// 起動時に開くファイル。空なら新しい空の文書を 1 つ開く。
    pub files: Vec<PathBuf>,
    /// 明暗の指定。`None` なら設定ファイルの値（なければ Windows に合わせる）。
    pub theme: Option<ThemeMode>,
    /// データ（設定・メモ・復元データ）の保存場所。`None` なら環境から決める。
    pub paths: Option<AppPaths>,
    /// ウィンドウを作った後、表示する前に呼ぶ（コマンドの登録、フック、メニューの設定）。
    pub setup: Option<SetupFn>,
}

struct EditSession {
    doc: DocumentId,
    actor: Actor,
    single_undo: bool,
}

/// 右ペイン（プレビュー）。
#[derive(Debug, Clone, Copy)]
struct RightPane {
    hwnd: HWND,
    ratio: f32,
}

/// 下部の差し込み欄（検索欄など）。
#[derive(Debug, Clone, Copy)]
struct BottomPanel {
    hwnd: HWND,
    height_dip: i32,
}

/// アプリの状態。UI スレッドだけが持つ。
pub struct App {
    hwnd: HWND,
    instance: HINSTANCE,
    editor: Editor,
    /// 文書を作る・表示していない文書を編集するための隠れた編集欄（初めて要るときに作る）。
    aux: Option<Editor>,
    /// `aux` が表示中の文書（`None` は不明または空）。
    aux_doc: Option<SciDoc>,
    docs: Vec<OpenDoc>,
    active: Option<usize>,
    tabs: TabBar,
    status: StatusBar,
    theme_mode: ThemeMode,
    theme: Theme,
    dpi: u32,
    font: UiFont,
    layout: Layout,
    right: Option<RightPane>,
    bottom: Option<BottomPanel>,
    edits: Vec<EditSession>,
    registry: Rc<Registry<App>>,
    sender: UiSender<UiMessage>,
    receiver: UiReceiver<UiMessage>,
    untitled_counter: u32,
    tracking_mouse: bool,
    dragging_splitter: bool,
    exit_without_confirmation: bool,
    /// データの保存場所（決められなければ `None`。メモ・復元・設定の保存は使えない）。
    paths: Option<AppPaths>,
    settings: Settings,
    settings_state: fileio::SettingsState,
    prompts: Rc<dyn Prompts>,
    keymap: KeyMap,
    menu: Option<MenuModel>,
    hotkey: Option<GlobalHotkey>,
    memo_panel: Option<MemoPanel>,
    /// 検索・置換・Grep の小窓（初めて使うときに作る）。
    find_panel: Option<FindPanel>,
    /// 最後に使った検索条件（F3 用）。
    last_search: Option<SearchOptions>,
    /// 検索の一致の強調表示（U3）。
    highlight: highlight::Highlight,
    /// 実行中の検索（ワーカー）の中止。
    search_cancel: Option<CancelToken>,
    /// これ以上の大きさの文書は、検索・置換をワーカーで行う。
    worker_search_bytes: usize,
    /// Grep の結果を開いたあとの移動先（開き終えるまで待たせる）。
    pending_jumps: HashMap<PathBuf, (u64, u64)>,
    /// 巨大ファイルのタブの部品。
    bigviews: HashMap<DocumentId, bigdoc::BigDoc>,
    /// 巨大モードにする条件。
    large_thresholds: LargeModeThresholds,
    big_timer: bool,
    /// Markdown プレビュー（初めて開くときに作る）。
    preview: Option<preview::PreviewState>,
    /// 終了時に「保存しない」と選んだ文書（確認を繰り返さない。破棄は終了の間際）。
    exit_discard: HashMap<DocumentId, Version>,
    /// 読み込み中のパス（二重に開かない）。
    opening: HashSet<PathBuf>,
    autosave_timer: bool,
    clipboard_counter: u32,
    /// 起動時に分かった問題（表示する前にまとめて知らせる）。
    startup_notes: Vec<String>,
    /// 統合側が差し込む処理。
    pub hooks: Hooks,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// 積まれた Scintilla の通知（Scintilla の文書, 通知）。
    static EVENTS: RefCell<Vec<(isize, EditorEvent)>> = const { RefCell::new(Vec::new()) };
    /// このスレッドの編集欄のウィンドウ（通知の送り元の確認用）。
    static EDITORS: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
    static REPAINT_PENDING: Cell<bool> = const { Cell::new(false) };
    /// アプリを借りられない間に届いた「ワーカーの結果が届いた」知らせ（借用が終わったら
    /// もう一度投函する。モーダルなダイアログの間に取りこぼさないため）。
    static WAKE_PENDING: Cell<bool> = const { Cell::new(false) };
}

/// アプリの状態を借りて処理する。アプリがない、または既に借りられている（再入）なら
/// `None`。処理の後で [`App::sync`] を呼ぶ。
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    let result = APP.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        let app = guard.as_mut()?;
        let r = f(app);
        app.sync();
        Some((r, app.hwnd))
    });
    let (r, hwnd) = result?;
    if REPAINT_PENDING.with(|p| p.replace(false)) {
        // SAFETY: 自分のウィンドウを描き直す。
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
    if WAKE_PENDING.with(|p| p.replace(false)) {
        // SAFETY: 自分のウィンドウへの投函。
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_WAKE, WPARAM(0), LPARAM(0));
        }
    }
    Some(r)
}

fn is_our_editor(hwnd: HWND) -> bool {
    EDITORS.with(|e| e.borrow().contains(&(hwnd.0 as isize)))
}

fn register_editor_hwnd(hwnd: HWND) {
    EDITORS.with(|e| e.borrow_mut().push(hwnd.0 as isize));
}

fn unregister_editor_hwnd(hwnd: HWND) {
    EDITORS.with(|e| e.borrow_mut().retain(|h| *h != hwnd.0 as isize));
}

/// メインウィンドウのメッセージの入口（`window.rs` のウィンドウプロシージャから呼ぶ）。
pub(crate) fn dispatch(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_NOTIFY => {
            // SAFETY: WM_NOTIFY の lParam。
            if let Some((from, event)) = unsafe { editor::parse_notification(lparam) } {
                if is_our_editor(from) {
                    // 通知の途中で Scintilla にメッセージを送ってよい（文書を尋ねるだけ）。
                    // SAFETY: 自分の編集欄への同期メッセージ。
                    let doc = unsafe {
                        SendMessageW(
                            from,
                            editor::sci::SCI_GETDOCPOINTER,
                            Some(WPARAM(0)),
                            Some(LPARAM(0)),
                        )
                        .0
                    };
                    EVENTS.with(|e| e.borrow_mut().push((doc, event)));
                    // 再入でなければ、ここで反映する（通常のキー入力）。
                    with_app(|_| ());
                    return Some(LRESULT(0));
                }
            }
            None
        }
        WM_CLOSE => {
            if with_app(|app| app.can_exit()) == Some(true) {
                // SAFETY: 自分のウィンドウを閉じる（アプリを借りていない状態で）。
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            Some(LRESULT(0))
        }
        WM_DESTROY => {
            with_app(|app| app.teardown());
            // SAFETY: 引数なし。
            unsafe { PostQuitMessage(0) };
            Some(LRESULT(0))
        }
        _ => match with_app(|app| app.handle(msg, wparam, lparam)) {
            Some(result) => result,
            None => {
                if msg == WM_APP_WAKE {
                    WAKE_PENDING.with(|p| p.set(true));
                }
                if msg == WM_PAINT {
                    REPAINT_PENDING.with(|p| p.set(true));
                    // 外側の処理がアプリを返さないまま終わった場合に備えて、後で描き直す。
                    // SAFETY: 自分のウィンドウへの投函。
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), WM_APP_REPAINT, WPARAM(0), LPARAM(0));
                    }
                }
                None
            }
        },
    }
}

/// Linote の画面を起動し、終了するまでメッセージループを回す。終了コードを返す。
///
/// `main.rs` から呼ぶ。
pub fn run(options: RunOptions) -> i32 {
    system::enable_per_monitor_dpi();
    match start(options) {
        Ok(()) => message_loop(),
        Err(e) => {
            eprintln!("linote: {e}");
            1
        }
    }
}

/// ウィンドウを作ってアプリの状態を用意し、表示する（メッセージループは回さない）。
pub fn start(options: RunOptions) -> Result<()> {
    // SAFETY: 自分のモジュールのハンドル。
    let instance: HINSTANCE = unsafe { GetModuleHandleW(PCWSTR::null()) }
        .map_err(|e| Error::unavailable("画面", format!("モジュールのハンドルを取れません: {e}")))?
        .into();
    window::register_main_class(instance)?;
    editor::register_classes(instance)?;
    let hwnd = window::create_main_window(instance)?;
    let paths = options.paths.clone().or_else(|| AppPaths::from_env().ok());
    let (settings, settings_state) = fileio::load_settings(paths.as_ref());
    let theme_mode = options.theme.unwrap_or(settings.theme);
    let app = match App::new(hwnd, instance, theme_mode, paths, settings, settings_state) {
        Ok(app) => app,
        Err(e) => {
            // SAFETY: 作ったばかりのウィンドウ。アプリはまだないので既定の処理になる。
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err(e);
        }
    };
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    let RunOptions { files, setup, .. } = options;
    with_app(|app| {
        app.relayout();
        app.install_menu();
        if files.is_empty() {
            if let Err(e) = app.new_untitled() {
                app.set_status(&e.to_string(), MessageKind::Warning);
            }
        }
        for path in &files {
            app.open_path(path);
        }
        if let Some(setup) = setup {
            setup(app);
        }
        app.startup_notices();
    })
    .ok_or_else(|| Error::Internal("アプリの状態を用意できませんでした".into()))?;
    // SAFETY: 自分のウィンドウを表示する。
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWDEFAULT);
        let _ = UpdateWindow(hwnd);
    }
    with_app(|app| {
        app.focus_active();
        // 表示した後で、ワーカーに復元データを探させ、グローバルショートカットを登録する。
        app.init_hotkey();
        app.start_recovery_scan();
    });
    Ok(())
}

/// メインウィンドウが閉じるまでメッセージを処理する。
pub fn message_loop() -> i32 {
    let mut msg = MSG::default();
    loop {
        // SAFETY: MSG は呼び出しの間有効。
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 == 0 || r.0 == -1 {
            break;
        }
        let handled = with_app(|app| {
            app.translate_shortcut(&msg)
                || match app.hooks.pre_translate {
                    Some(f) => f(app, &msg),
                    None => false,
                }
        })
        .unwrap_or(false);
        if handled {
            continue;
        }
        // SAFETY: GetMessageW で受け取ったメッセージ。
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    APP.with(|cell| cell.borrow_mut().take());
    EDITORS.with(|e| e.borrow_mut().clear());
    EVENTS.with(|e| e.borrow_mut().clear());
    msg.wParam.0 as i32
}

/// このスレッドのアプリがあるか。
pub fn is_running() -> bool {
    APP.with(|cell| cell.try_borrow().map(|a| a.is_some()).unwrap_or(true))
}

impl App {
    fn new(
        hwnd: HWND,
        instance: HINSTANCE,
        theme_mode: ThemeMode,
        paths: Option<AppPaths>,
        settings: Settings,
        settings_state: fileio::SettingsState,
    ) -> Result<App> {
        let editor = Editor::create(hwnd, instance, window::ID_EDITOR, true)?;
        register_editor_hwnd(editor.hwnd());
        let dpi = system::dpi_for_window(hwnd);
        let theme = Theme::resolve(theme_mode, system::apps_use_light_theme());
        system::set_dark_title_bar(hwnd, theme.dark);
        let waker_hwnd = hwnd.0 as isize;
        let (sender, receiver) = ui_channel::<UiMessage>(Arc::new(move || {
            // SAFETY: 自分のウィンドウへの投函（どのスレッドからでもよい）。ウィンドウが
            // 既にないときは失敗するだけ。
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(waker_hwnd as *mut _)),
                    WM_APP_WAKE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }));
        let mut registry = Registry::new();
        register(&mut registry)?;
        commands::register(&mut registry)?;
        let app = App {
            hwnd,
            instance,
            editor,
            aux: None,
            aux_doc: None,
            docs: Vec::new(),
            active: None,
            tabs: TabBar::new(),
            status: StatusBar::new(),
            theme_mode,
            theme,
            dpi,
            font: UiFont::for_dpi(dpi),
            layout: Layout::default(),
            right: None,
            bottom: None,
            edits: Vec::new(),
            registry: Rc::new(registry),
            sender,
            receiver,
            untitled_counter: 0,
            tracking_mouse: false,
            dragging_splitter: false,
            exit_without_confirmation: false,
            paths,
            settings,
            settings_state,
            prompts: Rc::new(Win32Prompts),
            keymap: KeyMap::default(),
            menu: None,
            hotkey: None,
            memo_panel: None,
            find_panel: None,
            last_search: None,
            highlight: highlight::Highlight::default(),
            search_cancel: None,
            worker_search_bytes: search::WORKER_SEARCH_BYTES,
            pending_jumps: HashMap::new(),
            bigviews: HashMap::new(),
            large_thresholds: LargeModeThresholds::DEFAULT,
            big_timer: false,
            preview: None,
            exit_discard: HashMap::new(),
            opening: HashSet::new(),
            autosave_timer: false,
            clipboard_counter: 0,
            startup_notes: Vec::new(),
            hooks: Hooks::default(),
        };
        app.editor.apply_view(&app.settings.view);
        app.editor.apply_theme(&app.theme, Language::PlainText);
        // ファイルのドラッグ＆ドロップ（タブバー・状態表示・本文）。本文は Scintilla の
        // ドロップ対象が親へ WM_DROPFILES として転送する（third_party/VENDORED.md を参照）。
        // SAFETY: 自分のウィンドウ。
        unsafe { windows::Win32::UI::Shell::DragAcceptFiles(hwnd, true) };
        Ok(app)
    }

    // ---------------------------------------------------------------- 取得

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn instance(&self) -> HINSTANCE {
        self.instance
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn theme_mode(&self) -> ThemeMode {
        self.theme_mode
    }

    pub fn dpi(&self) -> u32 {
        self.dpi
    }

    /// 表示用の編集欄（アクティブな文書を表示している）。
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// ワーカーから結果を送るための送信口（複製して渡す）。
    pub fn sender(&self) -> UiSender<UiMessage> {
        self.sender.clone()
    }

    pub fn registry(&self) -> Rc<Registry<App>> {
        Rc::clone(&self.registry)
    }

    /// 登録表を差し替える（統合時に、すべてのモジュールの `register` を済ませたもの）。
    /// B のコマンド（[`register`]）も含めること。
    pub fn set_registry(&mut self, registry: Registry<App>) {
        self.registry = Rc::new(registry);
    }

    pub fn documents(&self) -> &[OpenDoc] {
        &self.docs
    }

    pub fn document(&self, id: DocumentId) -> Option<&OpenDoc> {
        self.docs.iter().find(|d| d.id() == id)
    }

    fn index_of(&self, id: DocumentId) -> Result<usize> {
        self.docs
            .iter()
            .position(|d| d.id() == id)
            .ok_or_else(|| Error::not_found(format!("文書 {id}")))
    }

    pub fn state(&self, id: DocumentId) -> Option<&DocumentState> {
        self.document(id).map(|d| &d.state)
    }

    /// 文書の状態を変える（保存の記録、外部変更の記録など）。変えた後はタブと
    /// ステータスバーを更新する。本文の変更は Scintilla を通すこと（版は通知で進む）。
    pub fn update_state<R>(
        &mut self,
        id: DocumentId,
        f: impl FnOnce(&mut DocumentState) -> R,
    ) -> Result<R> {
        let i = self.index_of(id)?;
        let r = f(&mut self.docs[i].state);
        self.refresh_tabs();
        self.refresh_status();
        Ok(r)
    }

    pub fn active_id(&self) -> Option<DocumentId> {
        self.active.map(|i| self.docs[i].id())
    }

    /// 結果がまだ有効か（文書が開いていて、版が同じ）。
    pub fn is_current(&self, stamp: DocStamp) -> bool {
        stamp.is_current(self.state(stamp.document).map(|s| s.version()))
    }

    // ---------------------------------------------------------------- 通知の反映

    /// 積まれた Scintilla の通知を文書の状態に反映する。
    pub fn sync(&mut self) {
        let events = EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()));
        if events.is_empty() {
            return;
        }
        let mut tabs_changed = false;
        let mut status_changed = false;
        let mut margin = false;
        let mut brace = false;
        let mut zoom = false;
        let mut clear_highlight = false;
        let mut changed_docs: Vec<DocumentId> = Vec::new();
        for (doc, event) in events {
            let Some(i) = self
                .docs
                .iter()
                .position(|d| matches!(d.view, DocView::Scintilla(s) if s.as_raw() == doc))
            else {
                continue;
            };
            let is_active = self.active == Some(i);
            let id = self.docs[i].id();
            match event {
                EditorEvent::Modified { lines_added, .. } => {
                    let actor = self
                        .edits
                        .iter()
                        .rev()
                        .find(|s| s.doc == id)
                        .map(|s| s.actor)
                        .unwrap_or(Actor::User);
                    let state = &mut self.docs[i].state;
                    let before = (state.is_dirty(), state.has_ai_edits());
                    state.record_edit(actor);
                    tabs_changed |= before != (state.is_dirty(), state.has_ai_edits());
                    status_changed |= is_active;
                    margin |= is_active && lines_added != 0;
                    clear_highlight |= self.highlight_doc() == Some(id);
                    if !changed_docs.contains(&id) {
                        changed_docs.push(id);
                    }
                }
                EditorEvent::SavePointReached => {
                    let d = &mut self.docs[i];
                    if d.savepoint_valid && d.state.is_dirty() {
                        d.state.record_unmodified();
                        tabs_changed = true;
                    }
                }
                EditorEvent::UpdateUi { selection, .. } => {
                    status_changed |= is_active;
                    brace |= is_active && selection;
                }
                EditorEvent::CharAdded { ch } => {
                    if is_active
                        && self.settings.view.auto_indent
                        && self.editor.is_newline_char(ch)
                    {
                        self.editor.auto_indent_after_newline();
                    }
                }
                EditorEvent::Zoom => zoom |= is_active,
                EditorEvent::SavePointLeft | EditorEvent::FocusIn | EditorEvent::FocusOut => {}
            }
        }
        if margin {
            self.editor.update_line_number_margin();
        }
        if clear_highlight {
            self.clear_highlight_and_count();
        }
        if brace {
            self.editor.update_brace_highlight();
        }
        if zoom {
            self.on_zoom_changed();
        }
        if tabs_changed {
            self.refresh_tabs();
        }
        if status_changed {
            self.refresh_status();
        }
        for &id in &changed_docs {
            self.note_change(id);
            self.preview_note_change(id);
        }
        if let Some(hook) = self.hooks.document_changed {
            for id in changed_docs {
                hook(self, id);
            }
        }
    }

    // ---------------------------------------------------------------- 文書を開く・作る

    /// 隠れた編集欄（文書の作成用・表示していない文書の編集用）。
    fn aux(&mut self) -> Result<&Editor> {
        if self.aux.is_none() {
            let aux = Editor::create(self.hwnd, self.instance, window::ID_FACTORY, false)?;
            register_editor_hwnd(aux.hwnd());
            self.aux = Some(aux);
            self.aux_doc = None;
        }
        Ok(self.aux.as_ref().expect("作成済み"))
    }

    /// 本文を流し込むための読み込み口を作る。ワーカーへ渡して [`DocLoader::add`] し、
    /// UI スレッドで [`DocLoader::finish`] してから [`add_loaded`](Self::add_loaded) する。
    pub fn create_loader(&mut self, bytes_hint: u64) -> Result<DocLoader> {
        let loader = self.aux()?.create_loader(bytes_hint);
        // SCI_CREATELOADER は隠れた編集欄の表示状態を壊すので、次に使うときに付け直す。
        self.aux_doc = None;
        loader
    }

    /// 新しい空の文書を開き、アクティブにする。
    pub fn new_untitled(&mut self) -> Result<DocumentId> {
        self.untitled_counter += 1;
        let title = if self.untitled_counter == 1 {
            "無題".to_owned()
        } else {
            format!("無題 {}", self.untitled_counter)
        };
        self.open_text(
            DocumentState::untitled(),
            "",
            title,
            Language::PlainText,
            true,
        )
    }

    /// 本文を持つ文書を開く（クリップボード、メモ、復元、読み込んだファイル）。
    ///
    /// `state` の版・保存済みの版はそのまま使う（読み込みでは版は進まない）。
    /// `state` が未保存（[`DocumentState::is_dirty`]）なら、Undo で読み込み時点に戻っても
    /// 未保存のまま扱う。
    pub fn open_text(
        &mut self,
        state: DocumentState,
        text: &str,
        title: String,
        language: Language,
        activate: bool,
    ) -> Result<DocumentId> {
        let mut loader = self.create_loader(text.len() as u64)?;
        loader.add(text.as_bytes())?;
        let doc = loader.finish()?;
        self.add_loaded(state, doc, title, language, activate)
    }

    /// [`DocLoader::finish`] で作ったドキュメントを文書として加える（参照は `App` が引き取る）。
    pub fn add_loaded(
        &mut self,
        state: DocumentState,
        doc: SciDoc,
        title: String,
        language: Language,
        activate: bool,
    ) -> Result<DocumentId> {
        self.sync();
        let read_only = !state.capabilities().contains(Capability::Edit);
        let aux = match self.aux() {
            Ok(aux) => aux,
            Err(e) => {
                self.editor.release_document(doc);
                return Err(e);
            }
        };
        aux.set_document(doc);
        aux.finish_loaded_document();
        aux.set_eol(state.format.eol);
        aux.set_read_only(read_only);
        let language = language.effective(aux.length());
        aux.set_language(language);
        self.aux_doc = Some(doc);
        // 読み込みで生じた通知（保存位置など）は、文書を一覧に加える前に捨てる。
        self.sync();
        let savepoint_valid = !state.is_dirty();
        let id = state.id();
        self.docs.push(OpenDoc::new(
            state,
            title,
            language,
            DocView::Scintilla(doc),
            savepoint_valid,
        ));
        if activate || self.active.is_none() {
            self.activate(id)?;
        } else {
            self.refresh_tabs();
        }
        Ok(id)
    }

    /// ほかの部品が表示する文書（巨大ファイルなど）を加える。`hwnd` はメインウィンドウの
    /// 子ウィンドウで、配置と表示・非表示は `App` が行う。
    pub fn add_external(
        &mut self,
        state: DocumentState,
        hwnd: HWND,
        title: String,
        activate: bool,
    ) -> Result<DocumentId> {
        // SAFETY: 渡されたウィンドウを隠すだけ。
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        let id = state.id();
        self.docs.push(OpenDoc::new(
            state,
            title,
            Language::PlainText,
            DocView::External(hwnd),
            false,
        ));
        if activate || self.active.is_none() {
            self.activate(id)?;
        } else {
            self.refresh_tabs();
        }
        Ok(id)
    }

    /// 保存が終わったことを記録する（[`DocumentState::record_saved`]）。保存した版が
    /// 現在の版なら Scintilla の保存位置も合わせる。
    pub fn record_saved(
        &mut self,
        id: DocumentId,
        version: Version,
        path: Option<PathBuf>,
        disk: Option<FileStamp>,
    ) -> Result<()> {
        self.sync();
        let i = self.index_of(id)?;
        let new_path = path.clone();
        self.docs[i].state.record_saved(version, path, disk);
        let current = self.docs[i].state.version() == version;
        if let Some(p) = &new_path {
            self.docs[i].title = file_title(p);
            let language = Language::from_path(p);
            if language != self.docs[i].language {
                self.docs[i].language = language;
                self.with_text(id, |ed| ed.set_language(language))?;
                if self.active == Some(i) {
                    self.editor.apply_theme(&self.theme, language);
                }
            }
        }
        if current {
            self.with_text(id, |ed| ed.set_save_point())?;
            self.docs[i].savepoint_valid = true;
        } else {
            // 保存した内容は Scintilla の履歴のどこにも対応しない。
            self.docs[i].savepoint_valid = false;
        }
        self.sync();
        self.refresh_tabs();
        self.refresh_status();
        self.update_window_title();
        Ok(())
    }

    /// 本文の写しと、その時点の印（保存・復元・変換をワーカーに渡すため）。
    pub fn text_snapshot(&mut self, id: DocumentId) -> Result<(DocStamp, String)> {
        self.sync();
        let stamp = self
            .state(id)
            .map(|s| s.stamp())
            .ok_or_else(|| Error::not_found(format!("文書 {id}")))?;
        let text = self.with_text(id, |ed| ed.text())?;
        Ok((stamp, text))
    }

    /// 文書の本文を表示している（または一時的に付けた）編集欄で処理する。
    /// アクティブでない文書は隠れた編集欄で扱うので、選択などの表示状態は保たれない。
    /// 処理で生じた変更通知は、終わった後に反映する。
    pub fn with_text<R>(&mut self, id: DocumentId, f: impl FnOnce(&mut Editor) -> R) -> Result<R> {
        let i = self.index_of(id)?;
        let DocView::Scintilla(doc) = self.docs[i].view else {
            return Err(Error::unsupported("この文書は編集欄で表示していません"));
        };
        let r = if self.active == Some(i) {
            f(&mut self.editor)
        } else {
            if self.aux_doc != Some(doc) {
                self.sync();
                self.aux()?.set_document(doc);
                self.aux_doc = Some(doc);
            }
            f(self.aux.as_mut().expect("作成済み"))
        };
        self.sync();
        Ok(r)
    }

    // ---------------------------------------------------------------- 切り替え・閉じる

    /// 文書をアクティブにする（タブの切り替え）。
    pub fn activate(&mut self, id: DocumentId) -> Result<()> {
        let i = self.index_of(id)?;
        if self.active == Some(i) {
            return Ok(());
        }
        self.sync();
        if self.highlight_doc().is_some_and(|d| d != id) {
            self.clear_highlight();
        }
        if let Some(cur) = self.active {
            match self.docs[cur].view {
                DocView::Scintilla(_) => self.docs[cur].view_state = self.editor.view_state(),
                DocView::External(h) => {
                    let leaving = self.docs[cur].id();
                    self.big_leave(leaving);
                    // SAFETY: 外部の表示部品を隠すだけ。
                    unsafe {
                        let _ = ShowWindow(h, SW_HIDE);
                    }
                }
            }
        }
        let d = &self.docs[i];
        match d.view {
            DocView::Scintilla(doc) => {
                self.editor.set_document(doc);
                self.editor.set_eol(d.state.format.eol);
                self.editor
                    .set_read_only(!d.state.capabilities().contains(Capability::Edit));
                self.editor.apply_theme(&self.theme, d.language);
                self.editor.restore_view_state(&d.view_state);
                // SAFETY: 自分の編集欄を表示する。
                unsafe {
                    let _ = ShowWindow(self.editor.hwnd(), SW_SHOW);
                }
            }
            DocView::External(h) => {
                let r = self.layout.editor;
                // SAFETY: 編集欄を隠し、外部の表示部品を同じ位置に出す。
                unsafe {
                    let _ = ShowWindow(self.editor.hwnd(), SW_HIDE);
                    let _ = SetWindowPos(
                        h,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    let _ = ShowWindow(h, SW_SHOW);
                }
                self.big_enter(id);
            }
        }
        self.active = Some(i);
        // 切り替えで届いた通知（表示の更新）を反映する。
        self.sync();
        self.refresh_tabs();
        self.refresh_status();
        self.update_window_title();
        self.focus_active();
        // 表示した文書のファイルが、ほかで変更されていないか確かめる。
        self.check_external_change_of(id);
        self.update_find_context();
        self.preview_active_changed(id);
        if let Some(hook) = self.hooks.active_changed {
            hook(self, Some(id));
        }
        Ok(())
    }

    /// 前後のタブへ（`delta` は +1／-1。端では反対側へ回る）。
    pub fn cycle_tab(&mut self, delta: isize) -> Result<()> {
        let n = self.docs.len();
        if n == 0 {
            return Ok(());
        }
        let cur = self.active.unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(n as isize) as usize;
        self.activate(self.docs[next].id())
    }

    /// 文書を閉じる。未保存なら `force` でない限り [`Hooks::confirm_close`] で確かめる。
    /// 閉じたら `true`。最後の文書を閉じたら新しい空の文書を開く。
    pub fn close_document(&mut self, id: DocumentId, force: bool) -> Result<bool> {
        self.sync();
        let i = self.index_of(id)?;
        if !force && self.docs[i].state.is_dirty() {
            let confirm = self.hooks.confirm_close;
            if !confirm(self, id) {
                return Ok(false);
            }
        }
        let i = self.index_of(id)?;
        if self.active == Some(i) {
            // 隣（右、なければ左）を先に表示し、編集欄が閉じる文書を手放すようにする。
            if self.docs.len() == 1 {
                self.new_untitled()?;
            } else {
                let next = if i + 1 < self.docs.len() {
                    i + 1
                } else {
                    i - 1
                };
                self.activate(self.docs[next].id())?;
            }
        }
        let i = self.index_of(id)?;
        let doc = self.docs.remove(i);
        self.exit_discard.remove(&id);
        // 保存済み・保存しないを選んだ文書の復元データは、ここで破棄する。
        if let Some(slot) = doc.slot {
            recover::discard_slot(slot);
        }
        if let Some(a) = self.active {
            if a > i {
                self.active = Some(a - 1);
            }
        }
        self.edits.retain(|s| s.doc != id);
        self.close_big(id);
        self.preview_document_closed(id);
        self.pending_jumps
            .retain(|p, _| doc.state.path() != Some(p.as_path()));
        if let DocView::Scintilla(sci) = doc.view {
            if let Some(aux) = &self.aux {
                if self.aux_doc == Some(sci) || aux.document() == sci {
                    aux.detach_document();
                    self.aux_doc = None;
                }
            }
            self.editor.release_document(sci);
        }
        // 閉じた文書の通知（あれば）を捨てる。
        self.sync();
        self.refresh_tabs();
        if let Some(hook) = self.hooks.document_closed {
            hook(self, id, doc.view);
        }
        Ok(true)
    }

    /// タブの題名を変える（メモの名前の変更など）。
    pub fn set_title(&mut self, id: DocumentId, title: String) -> Result<()> {
        let i = self.index_of(id)?;
        self.docs[i].title = title;
        self.refresh_tabs();
        self.update_window_title();
        Ok(())
    }

    fn focus_active(&self) {
        match self.active.map(|i| self.docs[i].view) {
            Some(DocView::Scintilla(_)) | None => self.editor.focus(),
            // SAFETY: 外部の表示部品にフォーカスを移す。
            Some(DocView::External(h)) => unsafe {
                let _ = SetFocus(Some(h));
            },
        }
    }

    fn update_window_title(&self) {
        let title = match self.active.map(|i| &self.docs[i]) {
            Some(d) => format!("{} - Linote", d.title),
            None => "Linote".to_owned(),
        };
        let wide = system::to_wide(&title);
        // SAFETY: NUL 終端の文字列。
        unsafe {
            let _ = SetWindowTextW(self.hwnd, PCWSTR(wide.as_ptr()));
        }
    }

    // ---------------------------------------------------------------- 表示の更新

    /// タブの一覧を文書の状態から作り直す。
    pub fn refresh_tabs(&mut self) {
        let items: Vec<TabItem> = self
            .docs
            .iter()
            .map(|d| TabItem {
                id: d.id(),
                title: d.title.clone(),
                dirty: d.state.is_dirty(),
                ai_edits: d.state.has_ai_edits(),
                read_only: !d.state.capabilities().contains(Capability::Edit),
            })
            .collect();
        if self.tabs.set_items(items, self.active) {
            self.relayout_tabs();
            self.invalidate(self.layout.tabs);
        }
    }

    fn relayout_tabs(&mut self) {
        let bar = self.layout.tabs;
        let dpi = self.dpi;
        let hwnd = self.hwnd;
        let font = self.font.handle();
        // SAFETY: ウィンドウの DC を借りて返す。
        unsafe {
            let dc = GetDC(Some(hwnd));
            let old = SelectObject(dc, font.into());
            let canvas = window::Canvas { hdc: dc, dpi };
            self.tabs.relayout(bar, dpi, &|s| canvas.measure(s));
            SelectObject(dc, old);
            ReleaseDC(Some(hwnd), dc);
        }
    }

    /// ステータスバーを、アクティブな文書とカーソルの位置から作り直す。
    pub fn refresh_status(&mut self) {
        let info = match self.active.map(|i| &self.docs[i]) {
            None => StatusInfo::default(),
            Some(d) => {
                let big = self.big_status_text(d.id());
                let (line, column, selected_chars) = match d.view {
                    DocView::Scintilla(_) => {
                        let sel = self.editor.selection();
                        let line = self.editor.line_from_position(sel.caret);
                        let column = self.editor.column_chars(sel.caret);
                        let r = sel.range();
                        let selected = if r.is_empty() || self.editor.is_rectangular_selection() {
                            0
                        } else {
                            count_chars(&self.editor, r.start, r.end)
                        };
                        (line + 1, column + 1, selected)
                    }
                    DocView::External(_) => (1, 1, 0),
                };
                StatusInfo {
                    line,
                    column,
                    selected_chars,
                    position_text: big.as_ref().map(|b| b.0.clone()),
                    language: match &big {
                        Some(b) => b.1.clone(),
                        None => d.language.label().to_owned(),
                    },
                    read_only: !d.state.capabilities().contains(Capability::Edit),
                    external_change: d.state.has_external_change(),
                    ai_edits: d.state.has_ai_edits(),
                    ..Default::default()
                }
                .with_format(&d.state.format)
            }
        };
        if self.status.set_info(info) {
            self.invalidate(self.layout.status);
        }
    }

    /// 短い状態表示を出す（数秒で消える）。
    pub fn set_status(&mut self, message: &str, kind: MessageKind) {
        self.status.set_message(Some((message.to_owned(), kind)));
        self.invalidate(self.layout.status);
        // SAFETY: 自分のウィンドウのタイマー。
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_STATUS, STATUS_MESSAGE_MS, None);
        }
    }

    pub fn status_message(&self) -> Option<&str> {
        self.status.message()
    }

    fn invalidate(&self, r: RECT) {
        // SAFETY: 自分のウィンドウの一部を描き直す。
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), Some(&r), false);
        }
    }

    // ---------------------------------------------------------------- 配置・テーマ

    /// 配置を計算し直して子ウィンドウを動かす。
    pub fn relayout(&mut self) {
        let mut client = RECT::default();
        // SAFETY: 出力先は RECT 1 つ。
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut client);
        }
        self.layout = window::compute_layout(
            client,
            self.dpi,
            self.right.map(|r| r.ratio),
            self.bottom.map(|b| b.height_dip),
        );
        let l = self.layout;
        let place = |h: HWND, r: RECT| {
            // SAFETY: 子ウィンドウを動かすだけ。
            unsafe {
                let _ = SetWindowPos(
                    h,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        };
        place(self.editor.hwnd(), l.editor);
        // 桁指定の折り返しは、編集欄の幅に合わせて右余白を直す。
        self.editor.update_wrap_margin();
        for d in &self.docs {
            if let DocView::External(h) = d.view {
                place(h, l.editor);
            }
        }
        if let (Some(p), Some(r)) = (self.right, l.right) {
            place(p.hwnd, r);
        }
        if let (Some(b), Some(r)) = (self.bottom, l.bottom) {
            place(b.hwnd, r);
        }
        self.relayout_tabs();
        self.status.set_rect(l.status, self.dpi);
        self.invalidate(l.tabs);
        self.invalidate(l.status);
        if let Some(s) = l.splitter {
            self.invalidate(s);
        }
    }

    /// 右ペイン（プレビュー）を差し込む・外す。`hwnd` はメインウィンドウの子ウィンドウ。
    pub fn set_right_pane(&mut self, pane: Option<HWND>) {
        let ratio = self.right.map(|r| r.ratio).unwrap_or(0.5);
        self.right = pane.map(|hwnd| RightPane { hwnd, ratio });
        self.relayout();
    }

    /// 右ペインの幅の割合（0.1〜0.9）。
    pub fn set_right_pane_ratio(&mut self, ratio: f32) {
        if let Some(r) = &mut self.right {
            r.ratio = ratio.clamp(0.1, 0.9);
            self.relayout();
        }
    }

    /// 下部の差し込み欄（検索欄など）を差し込む・外す。高さは DIP。
    pub fn set_bottom_panel(&mut self, panel: Option<(HWND, i32)>) {
        self.bottom = panel.map(|(hwnd, height_dip)| BottomPanel { hwnd, height_dip });
        self.relayout();
    }

    /// 明暗の設定を変える（設定ファイルの `theme`）。
    pub fn set_theme_mode(&mut self, mode: ThemeMode) {
        self.theme_mode = mode;
        self.refresh_theme();
    }

    /// 設定と Windows の明暗から配色を選び直し、変わっていれば反映する。
    pub fn refresh_theme(&mut self) {
        let theme = Theme::resolve(self.theme_mode, system::apps_use_light_theme());
        if theme == self.theme {
            return;
        }
        self.theme = theme;
        system::set_dark_title_bar(self.hwnd, self.theme.dark);
        let language = self
            .active
            .map(|i| self.docs[i].language)
            .unwrap_or(Language::PlainText);
        self.editor.apply_theme(&self.theme, language);
        if let Some(panel) = self.memo_panel {
            panel.set_theme(&self.theme);
        }
        if let Some(panel) = self.find_panel {
            panel.set_theme(&self.theme);
        }
        for b in self.bigviews.values() {
            b.view.set_theme(&self.theme);
        }
        self.preview_theme_changed();
        // SAFETY: ウィンドウ全体を描き直す。
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
        if let Some(hook) = self.hooks.theme_changed {
            hook(self);
        }
    }

    // ---------------------------------------------------------------- コマンド

    /// コマンドを実行する。失敗は状態表示にも出す（中止は除く）。
    pub fn execute(&mut self, invocation: &Invocation) -> Result<CommandOutput> {
        self.sync();
        let registry = Rc::clone(&self.registry);
        let result = registry.execute(self, invocation);
        self.sync();
        if let Err(e) = &result {
            if !matches!(e, Error::Cancelled) {
                self.set_status(&e.to_string(), MessageKind::Warning);
            }
        }
        result
    }

    fn execute_on(&mut self, command: CommandId, target: Option<DocumentId>) {
        let mut inv = Invocation::user(command);
        if let Some(id) = target {
            inv = inv.with_target(TargetRef::Document(id), None);
        }
        let _ = self.execute(&inv);
    }

    // ---------------------------------------------------------------- メッセージ

    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if let Some(hook) = self.hooks.message {
            if let Some(r) = hook(self, msg, wparam, lparam) {
                return Some(r);
            }
        }
        let x = (lparam.0 & 0xFFFF) as i16 as i32;
        let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
        match msg {
            WM_SIZE => {
                self.relayout();
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_PAINT => {
                self.paint();
                Some(LRESULT(0))
            }
            WM_APP_REPAINT => {
                // SAFETY: ウィンドウ全体を描き直す。
                unsafe {
                    let _ = InvalidateRect(Some(self.hwnd), None, false);
                }
                Some(LRESULT(0))
            }
            WM_APP_WAKE => {
                self.drain_messages();
                Some(LRESULT(0))
            }
            WM_SETFOCUS => {
                self.focus_active();
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                let active = (wparam.0 & 0xFFFF) != 0;
                if active {
                    // 前面に戻ったとき、表示中の文書のファイルがほかで変更されていないか確かめる。
                    if let Some(id) = self.active_id() {
                        self.check_external_change_of(id);
                    }
                }
                if let Some(hook) = self.hooks.window_activated {
                    hook(self, active);
                }
                None
            }
            WM_TIMER if wparam.0 == TIMER_STATUS => {
                // SAFETY: 自分のタイマーを止める。
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_STATUS);
                }
                self.status.set_message(None);
                self.invalidate(self.layout.status);
                Some(LRESULT(0))
            }
            WM_SETTINGCHANGE => {
                if system::is_color_setting_change(lparam) {
                    self.refresh_theme();
                }
                None
            }
            WM_DPICHANGED => {
                self.dpi = ((wparam.0 >> 16) & 0xFFFF) as u32;
                self.font = UiFont::for_dpi(self.dpi);
                // SAFETY: lParam は推奨の位置（RECT）を指す。
                let r = unsafe { *(lparam.0 as *const RECT) };
                // SAFETY: 自分のウィンドウを推奨の位置へ。
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                self.relayout();
                self.editor.update_line_number_margin();
                if let Some(panel) = self.memo_panel {
                    panel.set_dpi(self.dpi);
                }
                if let Some(panel) = self.find_panel {
                    panel.set_dpi(self.dpi);
                }
                for b in self.bigviews.values() {
                    b.view.set_dpi(self.dpi);
                }
                Some(LRESULT(0))
            }
            WM_COMMAND if Self::is_menu_command(wparam, lparam) => {
                self.on_menu_command((wparam.0 & 0xFFFF) as u16)
                    .then_some(LRESULT(0))
            }
            WM_INITMENUPOPUP => {
                self.on_init_menu_popup(HMENU(wparam.0 as *mut _));
                Some(LRESULT(0))
            }
            WM_HOTKEY => self.on_hotkey(wparam),
            WM_DROPFILES => {
                self.on_drop_files(windows::Win32::UI::Shell::HDROP(wparam.0 as *mut _));
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_AUTOSAVE => {
                self.autosave_tick(false);
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == bigdoc::TIMER_BIG => {
                self.big_tick();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == preview::TIMER_PREVIEW => {
                self.preview_tick();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.on_mouse_move(x, y);
                Some(LRESULT(0))
            }
            0x02A3 /* WM_MOUSELEAVE */ => {
                self.tracking_mouse = false;
                if self.tabs.set_hover(None) {
                    self.invalidate(self.layout.tabs);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                self.on_left_down(x, y);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                self.on_left_up(x, y);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.dragging_splitter = false;
                self.tabs.press_close(None);
                None
            }
            WM_MBUTTONUP => {
                if let Some(TabHit::Tab(i) | TabHit::Close(i)) = self.tabs.hit_test(x, y) {
                    let id = self.docs[i].id();
                    self.execute_on(FILE_CLOSE.id, Some(id));
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDBLCLK => {
                if self.tabs.hit_test(x, y) == Some(TabHit::Empty) {
                    self.execute_on(FILE_NEW.id, None);
                }
                Some(LRESULT(0))
            }
            WM_SETCURSOR => {
                if (lparam.0 & 0xFFFF) as u32 == HTCLIENT && self.cursor_over_splitter() {
                    // SAFETY: 標準のカーソル。
                    unsafe {
                        if let Ok(c) = LoadCursorW(None, IDC_SIZEWE) {
                            SetCursor(Some(c));
                        }
                    }
                    return Some(LRESULT(1));
                }
                None
            }
            _ => None,
        }
    }

    fn cursor_over_splitter(&self) -> bool {
        let Some(s) = self.layout.splitter else {
            return false;
        };
        let mut p = POINT::default();
        // SAFETY: 出力先は POINT 1 つ。
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        window::contains(&s, p.x, p.y)
    }

    fn on_mouse_move(&mut self, x: i32, y: i32) {
        if self.dragging_splitter {
            let mut client = RECT::default();
            // SAFETY: 出力先は RECT 1 つ。
            unsafe {
                let _ = GetClientRect(self.hwnd, &mut client);
            }
            self.set_right_pane_ratio(window::ratio_from_splitter_x(client, x));
            return;
        }
        if !self.tracking_mouse {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: 構造体は呼び出しの間有効。
            self.tracking_mouse = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
        }
        if self.tabs.set_hover(self.tabs.hit_test(x, y)) {
            self.invalidate(self.layout.tabs);
        }
    }

    fn on_left_down(&mut self, x: i32, y: i32) {
        if self
            .layout
            .splitter
            .is_some_and(|s| window::contains(&s, x, y))
        {
            self.dragging_splitter = true;
            // SAFETY: 自分のウィンドウでマウスを捕まえる。
            unsafe {
                SetCapture(self.hwnd);
            }
            return;
        }
        match self.tabs.hit_test(x, y) {
            Some(TabHit::Tab(i)) => {
                let id = self.docs[i].id();
                self.execute_on(VIEW_ACTIVATE.id, Some(id));
            }
            Some(TabHit::Close(i)) => {
                self.tabs.press_close(Some(i));
                // SAFETY: 自分のウィンドウでマウスを捕まえる。
                unsafe {
                    SetCapture(self.hwnd);
                }
            }
            Some(TabHit::Empty) | None => {
                if let Some(seg) = self.status.hit_test(x, y) {
                    if let Some(hook) = self.hooks.status_click {
                        hook(self, seg);
                    }
                }
            }
        }
    }

    fn on_left_up(&mut self, x: i32, y: i32) {
        let pressed = self.tabs.pressed_close();
        let dragging = self.dragging_splitter;
        self.dragging_splitter = false;
        self.tabs.press_close(None);
        if dragging || pressed.is_some() {
            // SAFETY: 捕まえたマウスを放す。
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        if let Some(i) = pressed {
            if self.tabs.hit_test(x, y) == Some(TabHit::Close(i)) && i < self.docs.len() {
                let id = self.docs[i].id();
                self.execute_on(FILE_CLOSE.id, Some(id));
            }
        }
    }

    fn paint(&mut self) {
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: WM_PAINT の中で BeginPaint〜EndPaint。
        unsafe {
            let hdc = BeginPaint(self.hwnd, &mut ps);
            self.tabs.paint(hdc, &self.theme, &self.font);
            self.status.paint(hdc, &self.theme, &self.font);
            if let Some(s) = self.layout.splitter {
                let c = window::Canvas { hdc, dpi: self.dpi };
                c.fill(&s, self.theme.ui.bar_bg);
                let mid = (s.left + s.right) / 2;
                c.fill(
                    &RECT {
                        left: mid,
                        right: mid + 1,
                        ..s
                    },
                    self.theme.ui.splitter,
                );
            }
            // 何も置いていない領域（右ペインが未設定、外部の表示部品の外など）。
            let c = window::Canvas { hdc, dpi: self.dpi };
            if self.active.is_none() {
                c.fill(&self.layout.editor, self.theme.editor.bg);
            }
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    fn drain_messages(&mut self) {
        let messages: Vec<UiMessage> = self.receiver.drain().collect();
        for m in messages {
            match m {
                UiMessage::Status(s) => self.set_status(&s, MessageKind::Info),
                UiMessage::Run(f) => f(self),
            }
            self.sync();
        }
    }

    fn can_exit(&mut self) -> bool {
        self.sync();
        if self.exit_without_confirmation {
            return true;
        }
        // メモは尋ねずに保存する。保存できなければ終了しない（内容を失わないため）。
        if !self.flush_memos_sync() {
            return false;
        }
        if self.docs.iter().any(|d| d.state.is_dirty()) {
            let confirm = self.hooks.confirm_exit;
            confirm(self)
        } else {
            true
        }
    }

    /// 終了の前に文書を手放す（WM_DESTROY）。
    fn teardown(&mut self) {
        self.sync();
        let forced = self.exit_without_confirmation;
        let discarded = std::mem::take(&mut self.exit_discard);
        for d in std::mem::take(&mut self.docs) {
            if let DocView::Scintilla(s) = d.view {
                self.editor.release_document(s);
            }
            // 未保存でなくなった・「保存しない」を選んだ文書の復元データは消す。それ以外
            // （確認なしの終了）は残し、次の起動で復元を勧める。
            if let Some(slot) = d.slot {
                let settled =
                    !d.state.is_dirty() || discarded.get(&d.state.id()) == Some(&d.state.version());
                if settled && !forced {
                    if let Ok(slot) = Arc::try_unwrap(slot) {
                        let _ = slot.discard();
                    }
                }
            }
        }
        self.preview_teardown();
        let big_ids: Vec<DocumentId> = self.bigviews.keys().copied().collect();
        for id in big_ids {
            self.close_big(id);
        }
        self.cancel_search();
        self.hotkey = None;
        self.menu = None;
        self.active = None;
        // SAFETY: 自分のタイマーを止める。
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_STATUS);
            let _ = KillTimer(Some(self.hwnd), TIMER_AUTOSAVE);
        }
    }

    /// 終了を求める（`WM_CLOSE` を送る。未保存があれば確認のフックが呼ばれる）。
    pub fn request_exit(&self) {
        // SAFETY: 自分のウィンドウへの投函。
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    /// 確認せずに終了する（自己診断・テスト用）。未保存の内容は失われる。
    pub fn force_exit(&mut self) {
        self.exit_without_confirmation = true;
        self.request_exit();
    }
}

fn file_title(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// 選択の文字数を数える上限（バイト）。カーソルが動くたびに数えるので、これより大きい
/// 選択では数えない（ステータスバーに文字数を出さない）。
const COUNT_SELECTION_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// 範囲の文字数。大きすぎる範囲は 0（数えない）。
fn count_chars(editor: &Editor, start: u64, end: u64) -> u64 {
    if end.saturating_sub(start) > COUNT_SELECTION_MAX_BYTES {
        return 0;
    }
    editor.count_characters(start, end)
}

// ------------------------------------------------------------------ CommandHost

impl CommandHost for App {
    fn active_document(&self) -> Option<DocumentId> {
        self.active_id()
    }

    fn target_version(&self, target: &TargetRef) -> Result<Version> {
        match target {
            TargetRef::Document(id) => self
                .state(*id)
                .map(|s| s.version())
                .ok_or_else(|| Error::not_found(format!("文書 {id}"))),
            TargetRef::Memo(id) => self.memo_version(id),
        }
    }

    fn document_capabilities(&self, document: DocumentId) -> Result<Capabilities> {
        self.state(document)
            .map(|s| s.capabilities())
            .ok_or_else(|| Error::not_found(format!("文書 {document}")))
    }

    fn begin_edit(&mut self, document: DocumentId, actor: Actor, single_undo: bool) {
        self.sync();
        self.edits.push(EditSession {
            doc: document,
            actor,
            single_undo,
        });
        if single_undo {
            let _ = self.with_text(document, |ed| ed.begin_undo_action());
        }
    }

    fn end_edit(&mut self, document: DocumentId) {
        // その間の通知を、この実行者の変更として反映してから閉じる。
        self.sync();
        if let Some(pos) = self.edits.iter().rposition(|s| s.doc == document) {
            let session = self.edits.remove(pos);
            if session.single_undo {
                let _ = self.with_text(document, |ed| ed.end_undo_action());
            }
        }
    }
}

// ------------------------------------------------------------------ コマンド（B の分）

/// 新しい空の文書。
pub const FILE_NEW: CommandDef = CommandDef::new(
    CommandId::new("file.new"),
    "新規",
    "新しい空の文書をタブで開く。",
)
.result("{\"document\": 文書 ID}")
.default_key("Ctrl+N");

/// 文書（タブ）を閉じる。
pub const FILE_CLOSE: CommandDef = CommandDef::new(
    CommandId::new("file.close"),
    "閉じる",
    "文書のタブを閉じる。未保存の変更があれば確認する。",
)
.target(TargetKind::Document)
.params(&[ParamDef::optional(
    "force",
    ParamType::Boolean,
    "未保存でも確認せずに閉じる",
)])
.result("{\"closed\": 閉じたか}")
.default_key("Ctrl+W");

/// 指定の文書のタブに切り替える。
pub const VIEW_ACTIVATE: CommandDef = CommandDef::new(
    CommandId::new("view.activate"),
    "タブを選ぶ",
    "指定の文書のタブに切り替える。",
)
.target(TargetKind::Document);

/// 次のタブ。
pub const VIEW_NEXT_TAB: CommandDef = CommandDef::new(
    CommandId::new("view.next_tab"),
    "次のタブ",
    "右隣のタブに切り替える（右端では左端へ）。",
)
.default_key("Ctrl+Tab");

/// 前のタブ。
pub const VIEW_PREV_TAB: CommandDef = CommandDef::new(
    CommandId::new("view.prev_tab"),
    "前のタブ",
    "左隣のタブに切り替える（左端では右端へ）。",
)
.default_key("Ctrl+Shift+Tab");

/// B のコマンドを登録する（統合時の登録表のつなぎ込みで呼ぶ）。
pub fn register(registry: &mut Registry<App>) -> Result<()> {
    registry.register(FILE_NEW, |app, _| {
        let id = app.new_untitled()?;
        Ok(serde_json::json!({ "document": id.as_u64() }))
    })?;
    registry.register(FILE_CLOSE, |app, call: &Call<'_>| {
        let id = call.document()?;
        let closed = app.close_document(id, call.bool_or("force", false))?;
        Ok(serde_json::json!({ "closed": closed }))
    })?;
    registry.register(VIEW_ACTIVATE, |app, call| {
        app.activate(call.document()?)?;
        Ok(Value::Null)
    })?;
    registry.register(VIEW_NEXT_TAB, |app, _| {
        app.cycle_tab(1)?;
        Ok(Value::Null)
    })?;
    registry.register(VIEW_PREV_TAB, |app, _| {
        app.cycle_tab(-1)?;
        Ok(Value::Null)
    })?;
    Ok(())
}

/// ウィンドウが存在するか（自己診断用）。
pub(crate) fn window_exists(hwnd: HWND) -> bool {
    // SAFETY: ハンドルを調べるだけ。
    unsafe { IsWindow(Some(hwnd)).as_bool() }
}
