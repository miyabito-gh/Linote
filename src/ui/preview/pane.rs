//! プレビューのペイン: WebView2 を 1 つ載せる子ウィンドウ。右ペインにも別ウィンドウの中にも
//! 使う（どちらも同じ HTML を `NavigateToString` で表示する）。
//!
//! ペイン自身のウィンドウは、WebView2 が使えないとき（ランタイムなし、初期化中）の案内を
//! 描く。WebView2 が準備できたらその上を覆う。
//!
//! 同じ文書の表示を新しい内容に替えるときは、スクロール位置を保つ: 読み込みの前に
//! `ExecuteScript` で位置を読み、読み込みが終わったら（`NavigationCompleted`）戻す。
//! 別の文書に替わったときは先頭から表示する。

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use webview2_com::take_pwstr;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    CreateCoreWebView2ControllerCompletedHandler, ExecuteScriptCompletedHandler,
    NavigationCompletedEventHandler, NavigationStartingEventHandler,
    NewWindowRequestedEventHandler, WebResourceRequestedEventHandler,
};
use windows::core::{w, Interface, BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject,
    InvalidateRect, SelectObject, SetBkMode, SetTextColor, DEFAULT_GUI_FONT, DT_LEFT, DT_NOPREFIX,
    DT_WORDBREAK, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::Com::IStream;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW, LoadCursorW,
    MoveWindow, RegisterClassExW, SetWindowLongPtrW, ShowWindow, GWLP_USERDATA, IDC_ARROW, SW_HIDE,
    SW_SHOW, SW_SHOWNORMAL, WM_ERASEBKGND, WM_NCDESTROY, WM_PAINT, WM_SETFOCUS, WM_SIZE,
    WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE,
};

use super::host::PreviewHost;
use super::policy::{
    decide_navigation, fits_navigate_to_string, guidance_text, navigation_failed_notice_html,
    oversize_notice_html, parse_scroll_result, scroll_restore_script, NavDecision, ScrollPos,
    SCROLL_CAPTURE_SCRIPT,
};
use crate::core::{DocumentId, Error, Result};
use crate::markdown::{PreviewPalette, Rgb};
use crate::platform::system::to_wide;

const CLASS: PCWSTR = w!("LinotePreviewPane");

/// スクロール位置の読み取りの完了をこれ以上待たない（次の表示の更新で読み取りを捨てる）。
const CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// ペインの状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneStatus {
    /// 環境またはコントローラーの完了待ち。
    Initializing,
    /// WebView2 が表示できる。
    Ready,
    /// 使えない（ランタイムなしなど）。理由つき。案内はペインに表示済み。
    Unavailable(String),
}

struct PaneInner {
    hwnd: Cell<HWND>,
    destroyed: Cell<bool>,
    host: PreviewHost,
    status: RefCell<PaneStatus>,
    controller: RefCell<Option<ICoreWebView2Controller>>,
    webview: RefCell<Option<ICoreWebView2>>,
    /// 最後に渡された文書（初期化の完了前に渡されたものも、ここに溜めて後で表示する）。
    document: RefCell<Option<Arc<str>>>,
    /// `document` がどの文書のものか。
    document_key: Cell<Option<DocumentId>>,
    /// 最後に表示した文書（同じものの再表示を省く）。
    shown: RefCell<Option<Arc<str>>>,
    /// `shown` がどの文書のものか。
    shown_key: Cell<Option<DocumentId>>,
    /// `NavigateToString` を呼んだ直後で、その `NavigationStarting` をまだ見ていない。
    ours: Cell<bool>,
    /// こちらが始めた読み込みの ID（`NavigationStarting` で知る）。完了を待っている間だけ。
    loading: Cell<Option<u64>>,
    /// `NavigateToString` を呼んでから、その読み込みが終わるまでの間。
    navigating: Cell<bool>,
    /// スクロール位置を読んでいる最中（`ExecuteScript` の完了待ち）。その文書の ID。
    capturing: Cell<Option<DocumentId>>,
    /// 読み取りを始めた時刻（応答がないまま居座らないよう、長すぎれば待つのをやめる）。
    capture_started: Cell<Option<std::time::Instant>>,
    /// 読み取りの世代。待つのをやめた読み取りの完了が、あとで届いても使わない。
    capture_gen: Cell<u64>,
    /// 読み込みの失敗を知らせる案内を出している最中（失敗が続いても繰り返さない）。
    failure_notice: Cell<bool>,
    /// 次の読み込みが終わったら戻すスクロール位置。
    restore: Cell<Option<ScrollPos>>,
    palette: RefCell<PreviewPalette>,
    on_status: RefCell<Option<StatusCallback>>,
}

type StatusCallback = Rc<dyn Fn(&PaneStatus)>;

/// プレビューのペイン（UI スレッド専用）。`Clone` は同じペインへの参照。
#[derive(Clone)]
pub struct PreviewPane {
    inner: Rc<PaneInner>,
}

impl PreviewPane {
    /// `parent` の子としてペインを作り、WebView2 の初期化を始める（ここが「初めて開く」操作。
    /// ランタイムの探索はこの中の [`PreviewHost::with_environment`] で始まる）。
    ///
    /// 位置は [`PreviewPane::set_bounds`] で決める。ランタイムがなければ状態は
    /// [`PaneStatus::Unavailable`] になり、ペインに案内が出る（`on_status` で知らせる）。
    pub fn create(host: &PreviewHost, parent: HWND, palette: &PreviewPalette) -> Result<Self> {
        register_class()?;
        let inner = Rc::new(PaneInner {
            hwnd: Cell::new(HWND::default()),
            destroyed: Cell::new(false),
            host: host.clone(),
            status: RefCell::new(PaneStatus::Initializing),
            controller: RefCell::new(None),
            webview: RefCell::new(None),
            document: RefCell::new(None),
            document_key: Cell::new(None),
            shown: RefCell::new(None),
            shown_key: Cell::new(None),
            ours: Cell::new(false),
            loading: Cell::new(None),
            navigating: Cell::new(false),
            capturing: Cell::new(None),
            capture_started: Cell::new(None),
            capture_gen: Cell::new(0),
            failure_notice: Cell::new(false),
            restore: Cell::new(None),
            palette: RefCell::new(palette.clone()),
            on_status: RefCell::new(None),
        });
        // SAFETY: クラスは登録済み。親は呼び出し側が渡した有効なウィンドウ。
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                CLASS,
                w!(""),
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                0,
                0,
                Some(parent),
                None,
                GetModuleHandleW(None).ok().map(Into::into),
                None,
            )
        }
        .map_err(|e| Error::unavailable("プレビュー", format!("ウィンドウを作れません: {e}")))?;
        inner.hwnd.set(hwnd);
        // ウィンドウの持つ参照。WM_NCDESTROY で戻す。
        let raw = Rc::into_raw(inner.clone());
        // SAFETY: hwnd は今作ったウィンドウ。
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };
        let pane = PreviewPane { inner };
        pane.start();
        Ok(pane)
    }

    fn start(&self) {
        let weak = Rc::downgrade(&self.inner);
        self.inner.host.with_environment(Box::new(move |res| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.destroyed.get() {
                return;
            }
            match res {
                Ok(env) => begin_controller(&inner, env),
                Err(reason) => inner.set_status(PaneStatus::Unavailable(reason)),
            }
        }));
    }

    pub fn hwnd(&self) -> HWND {
        self.inner.hwnd.get()
    }

    pub fn status(&self) -> PaneStatus {
        self.inner.status.borrow().clone()
    }

    /// 状態が変わったときに呼ばれる（`Ready` や `Unavailable` になったとき）。ウィンドウ
    /// プロシージャやハンドラーの中から呼ばれるので、重い処理や破棄はしない。
    pub fn set_on_status(&self, f: impl Fn(&PaneStatus) + 'static) {
        *self.inner.on_status.borrow_mut() = Some(Rc::new(f));
    }

    /// 親の中の位置と大きさ（親のクライアント座標、物理ピクセル）。
    pub fn set_bounds(&self, r: RECT) {
        if self.inner.destroyed.get() {
            return;
        }
        // SAFETY: 有効なウィンドウ。WM_SIZE がコントローラーの大きさを合わせる。
        unsafe {
            let _ = MoveWindow(
                self.hwnd(),
                r.left,
                r.top,
                (r.right - r.left).max(0),
                (r.bottom - r.top).max(0),
                true,
            );
        }
    }

    pub fn set_visible(&self, visible: bool) {
        if self.inner.destroyed.get() {
            return;
        }
        let controller = self.inner.controller.borrow().clone();
        // SAFETY: 有効なウィンドウとコントローラー。
        unsafe {
            let _ = ShowWindow(self.hwnd(), if visible { SW_SHOW } else { SW_HIDE });
            if let Some(c) = controller {
                let _ = c.SetIsVisible(visible);
            }
        }
    }

    /// 表示する文書（[`crate::markdown::compose_document`] の結果）。`key` はどの文書か
    /// （同じ文書の更新ならスクロール位置を保つ）。初期化の完了前に呼んでもよい
    /// （完了後に表示する）。同じ `Arc` の再設定は何もしない。
    pub fn set_document(&self, key: DocumentId, html: Arc<str>) {
        *self.inner.document.borrow_mut() = Some(html);
        self.inner.document_key.set(Some(key));
        PaneInner::navigate(&self.inner);
    }

    /// テーマの変更。背景色と案内の配色を替える。文書の見た目は新しい文書で替わる。
    pub fn set_palette(&self, palette: &PreviewPalette) {
        *self.inner.palette.borrow_mut() = palette.clone();
        self.inner.apply_background();
        // SAFETY: 有効なウィンドウ。
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd()), None, true);
        }
    }

    /// WebView にフォーカスを移す。
    pub fn focus(&self) {
        let controller = self.inner.controller.borrow().clone();
        if let Some(c) = controller {
            // SAFETY: 有効なコントローラー。
            unsafe {
                let _ = c.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
            }
        }
    }

    /// WebView2 を閉じ、ウィンドウを破棄する。
    pub fn destroy(&self) {
        if self.inner.destroyed.replace(true) {
            return;
        }
        self.inner.close_controller();
        // SAFETY: 有効なウィンドウ。WM_NCDESTROY で参照を戻す。
        unsafe {
            let _ = DestroyWindow(self.hwnd());
        }
    }
}

impl PaneInner {
    fn set_status(&self, s: PaneStatus) {
        *self.status.borrow_mut() = s.clone();
        // SAFETY: 有効なウィンドウ（破棄後は無効な呼び出しが失敗するだけ）。
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, true);
        }
        let cb = self.on_status.borrow().clone();
        if let Some(cb) = cb {
            cb(&s);
        }
    }

    fn close_controller(&self) {
        self.webview.borrow_mut().take();
        if let Some(c) = self.controller.borrow_mut().take() {
            // SAFETY: 有効なコントローラー。閉じると WebView2 のプロセスとの接続が切れる。
            unsafe {
                let _ = c.Close();
            }
        }
    }

    fn apply_background(&self) {
        let controller = self.controller.borrow().clone();
        let Some(c) = controller else { return };
        let bg = self.palette.borrow().background;
        if let Ok(c2) = c.cast::<ICoreWebView2Controller2>() {
            // SAFETY: 有効なコントローラー。
            unsafe {
                let _ = c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                    A: 255,
                    R: bg.r,
                    G: bg.g,
                    B: bg.b,
                });
            }
        }
    }

    fn resize_controller(&self) {
        let controller = self.controller.borrow().clone();
        let Some(c) = controller else { return };
        let mut r = RECT::default();
        // SAFETY: 有効なウィンドウとコントローラー。
        unsafe {
            if GetClientRect(self.hwnd.get(), &mut r).is_ok() {
                let _ = c.SetBounds(r);
            }
        }
    }

    /// 渡された文書を表示する。同じ文書の更新なら、先にスクロール位置を読んでから。
    fn navigate(this: &Rc<PaneInner>) {
        if *this.status.borrow() != PaneStatus::Ready {
            return;
        }
        if let Some(captured) = this.capturing.get() {
            // 位置の読み取り中。同じ文書の更新なら、その完了で最新の文書を表示する。
            // 別の文書に替わった・応答が長く返らないときは待たない（待つと、読み取りの完了が
            // 届かない限り表示が替わらず、空白のまま固まって見える）。
            let other_document = this.document_key.get() != Some(captured);
            let stalled = this
                .capture_started
                .get()
                .is_some_and(|t| t.elapsed() > CAPTURE_TIMEOUT);
            if !other_document && !stalled {
                return;
            }
            this.capture_gen.set(this.capture_gen.get().wrapping_add(1));
            this.capturing.set(None);
            this.restore.set(None);
        }
        let Some(doc) = this.document.borrow().clone() else {
            return;
        };
        if let Some(shown) = this.shown.borrow().as_ref() {
            if Arc::ptr_eq(shown, &doc) {
                return;
            }
        }
        let key = this.document_key.get();
        let same_document = this.shown.borrow().is_some() && key == this.shown_key.get();
        if !same_document {
            this.restore.set(None);
        } else if !this.navigating.get() && this.restore.get().is_none() {
            // 表示し終わっている同じ文書の更新。いまの位置を読んでから替える。
            // （読み込み中なら、前に読んだ位置がまだ `restore` にあるので、それを使う。）
            if Self::capture_scroll(this, key) {
                return;
            }
        }
        this.navigate_now();
    }

    /// スクロール位置を読み始める。始められなければ `false`。
    fn capture_scroll(this: &Rc<PaneInner>, key: Option<DocumentId>) -> bool {
        let webview = this.webview.borrow().clone();
        let Some(wv) = webview else { return false };
        let Some(key) = key else { return false };
        let weak = Rc::downgrade(this);
        let generation = this.capture_gen.get();
        let handler = ExecuteScriptCompletedHandler::create(Box::new(move |hr, json| {
            let Some(inner) = weak.upgrade() else {
                return Ok(());
            };
            if inner.capture_gen.get() != generation {
                // 待つのをやめた読み取り。いまの状態には触れない。
                return Ok(());
            }
            inner.capturing.set(None);
            if inner.destroyed.get() {
                return Ok(());
            }
            // 読んでいる間に別の文書へ替わっていれば、その位置は使わない。
            if hr.is_ok() && inner.document_key.get() == Some(key) {
                inner.restore.set(parse_scroll_result(&json));
            }
            PaneInner::navigate(&inner);
            Ok(())
        }));
        let script = to_wide(SCROLL_CAPTURE_SCRIPT);
        this.capturing.set(Some(key));
        this.capture_started.set(Some(std::time::Instant::now()));
        // SAFETY: 有効な WebView。文字列は NUL 終端で呼び出しの間有効。
        let r = unsafe { wv.ExecuteScript(PCWSTR(script.as_ptr()), &handler) };
        if r.is_err() {
            this.capturing.set(None);
            return false;
        }
        true
    }

    fn navigate_now(&self) {
        let Some(doc) = self.document.borrow().clone() else {
            return;
        };
        self.failure_notice.set(false);
        let webview = self.webview.borrow().clone();
        let Some(wv) = webview else { return };
        let html: std::borrow::Cow<str> = if fits_navigate_to_string(&doc) {
            std::borrow::Cow::Borrowed(&doc)
        } else {
            std::borrow::Cow::Owned(oversize_notice_html(&self.palette.borrow(), doc.len()))
        };
        let wide = to_wide(&html);
        self.ours.set(true);
        // 前の読み込みの完了（中断）は、もう待たない。新しい ID は NavigationStarting で知る。
        self.loading.set(None);
        // SAFETY: 有効な WebView。文字列は NUL 終端で呼び出しの間有効。
        let r = unsafe { wv.NavigateToString(PCWSTR(wide.as_ptr())) };
        if r.is_err() {
            self.ours.set(false);
            self.restore.set(None);
        } else {
            *self.shown.borrow_mut() = Some(doc);
            self.shown_key.set(self.document_key.get());
            self.navigating.set(true);
        }
    }

    /// 読み込みが終わった（`NavigationCompleted`）。こちらの読み込みなら位置を戻す。
    fn on_navigation_completed(&self, navigation_id: u64, success: bool) {
        if self.loading.get() != Some(navigation_id) {
            return;
        }
        self.loading.set(None);
        self.navigating.set(false);
        if !success {
            // 表示できなかった（大きすぎる URL など）。空白のままにせず、案内を出す。
            // 案内の読み込みまで失敗しても繰り返さない。
            self.restore.set(None);
            if !self.failure_notice.replace(true) {
                self.show_failure_notice();
            }
            return;
        }
        self.failure_notice.set(false);
        let Some(script) = self.restore.take().and_then(scroll_restore_script) else {
            return;
        };
        let webview = self.webview.borrow().clone();
        let Some(wv) = webview else { return };
        let handler = ExecuteScriptCompletedHandler::create(Box::new(|_, _| Ok(())));
        let script = to_wide(&script);
        // SAFETY: 有効な WebView。文字列は NUL 終端で呼び出しの間有効。
        unsafe {
            let _ = wv.ExecuteScript(PCWSTR(script.as_ptr()), &handler);
        }
    }
}

impl PaneInner {
    /// 読み込みに失敗した文書の代わりに、案内を表示する。
    fn show_failure_notice(&self) {
        let webview = self.webview.borrow().clone();
        let Some(wv) = webview else { return };
        let html = navigation_failed_notice_html(&self.palette.borrow());
        let wide = to_wide(&html);
        self.ours.set(true);
        self.loading.set(None);
        // SAFETY: 有効な WebView。文字列は NUL 終端で呼び出しの間有効。
        let r = unsafe { wv.NavigateToString(PCWSTR(wide.as_ptr())) };
        if r.is_err() {
            self.ours.set(false);
        } else {
            self.navigating.set(true);
        }
    }
}

fn begin_controller(inner: &Rc<PaneInner>, env: ICoreWebView2Environment) {
    let weak = Rc::downgrade(inner);
    let env2 = env.clone();
    let handler =
        CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |hr, ctrl| {
            let Some(inner) = weak.upgrade() else {
                if let (Ok(()), Some(c)) = (&hr, &ctrl) {
                    // SAFETY: 受け取ったコントローラーを使わずに閉じる。
                    unsafe {
                        let _ = c.Close();
                    }
                }
                return Ok(());
            };
            match (hr, ctrl) {
                (Ok(()), Some(c)) => {
                    if inner.destroyed.get() {
                        // SAFETY: 受け取ったコントローラーを使わずに閉じる。
                        unsafe {
                            let _ = c.Close();
                        }
                    } else if let Err(reason) = configure(&inner, &weak, &env2, c.clone()) {
                        // SAFETY: 設定に失敗したコントローラーは使わずに閉じる。
                        unsafe {
                            let _ = c.Close();
                        }
                        inner.set_status(PaneStatus::Unavailable(reason));
                    }
                }
                (Err(e), _) => inner.set_status(PaneStatus::Unavailable(format!(
                    "WebView を作れませんでした: {}",
                    e.message()
                ))),
                (Ok(()), None) => inner.set_status(PaneStatus::Unavailable(
                    "WebView を作れませんでした".to_owned(),
                )),
            }
            Ok(())
        }));
    // SAFETY: 有効な環境とウィンドウ。ハンドラーは呼び出し先が保持する。
    let r = unsafe { env.CreateCoreWebView2Controller(inner.hwnd.get(), &handler) };
    if let Err(e) = r {
        inner.set_status(PaneStatus::Unavailable(format!(
            "WebView を作れませんでした: {}",
            e.message()
        )));
    }
}

/// 安全設定を適用する。必須の設定が適用できなければ `Err`（表示しない）。
fn configure(
    inner: &Rc<PaneInner>,
    weak: &Weak<PaneInner>,
    env: &ICoreWebView2Environment,
    controller: ICoreWebView2Controller,
) -> std::result::Result<(), String> {
    let fail = |what: &str, e: windows::core::Error| format!("{what}: {}", e.message());
    // SAFETY: すべて、今受け取った有効な COM オブジェクトへの呼び出し。
    unsafe {
        let webview = controller
            .CoreWebView2()
            .map_err(|e| fail("CoreWebView2", e))?;
        let settings = webview.Settings().map_err(|e| fail("Settings", e))?;
        // 必須: 失敗したら表示しない。
        settings
            .SetIsScriptEnabled(false)
            .map_err(|e| fail("スクリプトの無効化", e))?;
        settings
            .SetAreDevToolsEnabled(false)
            .map_err(|e| fail("開発者ツールの無効化", e))?;
        settings
            .SetAreDefaultContextMenusEnabled(false)
            .map_err(|e| fail("右クリックメニューの無効化", e))?;
        // 次のものは新しい版でだけある。失敗しても続ける（スクリプトなしなので実害は小さい）。
        let _ = settings.SetIsWebMessageEnabled(false);
        let _ = settings.SetAreDefaultScriptDialogsEnabled(false);
        let _ = settings.SetIsStatusBarEnabled(false);
        let _ = settings.SetIsZoomControlEnabled(false);
        if let Ok(s) = settings.cast::<ICoreWebView2Settings3>() {
            let _ = s.SetAreBrowserAcceleratorKeysEnabled(false);
        }
        if let Ok(s) = settings.cast::<ICoreWebView2Settings4>() {
            let _ = s.SetIsPasswordAutosaveEnabled(false);
            let _ = s.SetIsGeneralAutofillEnabled(false);
        }
        if let Ok(s) = settings.cast::<ICoreWebView2Settings6>() {
            let _ = s.SetIsSwipeNavigationEnabled(false);
        }

        // ナビゲーション: 取り消しが基本。
        let nav_weak = weak.clone();
        let nav = NavigationStartingEventHandler::create(Box::new(move |sender, args| {
            let Some(args) = args else { return Ok(()) };
            if handle_navigation(&nav_weak, sender, &args).is_err() {
                let _ = args.SetCancel(true);
            }
            Ok(())
        }));
        let mut token = 0i64;
        webview
            .add_NavigationStarting(&nav, &mut token)
            .map_err(|e| fail("ナビゲーションの監視", e))?;

        // 読み込みの完了: スクロール位置を戻す。
        let done_weak = weak.clone();
        let done = NavigationCompletedEventHandler::create(Box::new(move |_sender, args| {
            let (Some(args), Some(inner)) = (args, done_weak.upgrade()) else {
                return Ok(());
            };
            let mut id = 0u64;
            if args.NavigationId(&mut id).is_ok() {
                let mut ok = BOOL(0);
                // 取れなければ成功として扱う（位置を戻す従来の動き）。
                let success = args.IsSuccess(&mut ok).map_or(true, |()| ok.as_bool());
                inner.on_navigation_completed(id, success);
            }
            Ok(())
        }));
        // 失敗しても表示はできる（位置が保たれないだけ）。
        let _ = webview.add_NavigationCompleted(&done, &mut token);

        // 新しいウィンドウは開かせない。
        let nw = NewWindowRequestedEventHandler::create(Box::new(|_s, args| {
            if let Some(args) = args {
                let _ = args.SetHandled(true);
            }
            Ok(())
        }));
        webview
            .add_NewWindowRequested(&nw, &mut token)
            .map_err(|e| fail("新しいウィンドウの監視", e))?;

        // 外部リソースの要求は 403 で拒否する（文書そのものは NavigateToString なので対象外。
        // 文書の読み込みは NavigationStarting で見る）。
        let env2 = env.clone();
        let wr = WebResourceRequestedEventHandler::create(Box::new(move |_s, args| {
            if let Some(args) = args {
                let resp =
                    env2.CreateWebResourceResponse(None::<&IStream>, 403, w!("Blocked"), w!(""))?;
                args.SetResponse(&resp)?;
            }
            Ok(())
        }));
        webview
            .add_WebResourceRequested(&wr, &mut token)
            .map_err(|e| fail("リソース要求の監視", e))?;
        let mut filters = 0;
        for ctx in [
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_STYLESHEET,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_TEXT_TRACK,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_EVENT_SOURCE,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MANIFEST,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SIGNED_EXCHANGE,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_PING,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_OTHER,
        ] {
            if webview.AddWebResourceRequestedFilter(w!("*"), ctx).is_ok() {
                filters += 1;
            }
        }
        if filters == 0 {
            return Err("リソース要求のフィルターを登録できません".to_owned());
        }

        *inner.controller.borrow_mut() = Some(controller.clone());
        *inner.webview.borrow_mut() = Some(webview);
        inner.apply_background();
        inner.resize_controller();
        let _ = controller.SetIsVisible(true);
    }
    inner.set_status(PaneStatus::Ready);
    PaneInner::navigate(inner);
    Ok(())
}

fn handle_navigation(
    weak: &Weak<PaneInner>,
    sender: Option<ICoreWebView2>,
    args: &ICoreWebView2NavigationStartingEventArgs,
) -> windows::core::Result<()> {
    // SAFETY: イベントの引数として渡された有効な COM オブジェクト。
    unsafe {
        let mut p = PWSTR::null();
        args.Uri(&mut p)?;
        let uri = take_pwstr(p);
        let mut user = BOOL(0);
        args.IsUserInitiated(&mut user)?;
        let source = sender
            .and_then(|s| {
                let mut p = PWSTR::null();
                s.Source(&mut p).ok()?;
                Some(take_pwstr(p))
            })
            .unwrap_or_default();
        let inner = weak.upgrade();
        let ours = inner.as_ref().is_some_and(|i| i.ours.replace(false));
        match decide_navigation(&uri, ours, &source, user.as_bool()) {
            NavDecision::Allow => {
                if ours {
                    let mut id = 0u64;
                    if let (Some(i), Ok(())) = (&inner, args.NavigationId(&mut id)) {
                        i.loading.set(Some(id));
                    }
                }
            }
            NavDecision::Cancel => {
                if ours {
                    // こちらの読み込みが通らなかった。完了を待たない。
                    if let Some(i) = &inner {
                        i.navigating.set(false);
                        i.restore.set(None);
                    }
                }
                args.SetCancel(true)?
            }
            NavDecision::OpenExternal(url) => {
                args.SetCancel(true)?;
                open_external(&url);
            }
        }
    }
    Ok(())
}

/// 既定のブラウザー（`mailto` はメールソフト）で開く。`decide_navigation` が許したものだけ。
fn open_external(url: &str) {
    let wide = to_wide(url);
    // SAFETY: 文字列は NUL 終端で呼び出しの間有効。
    unsafe {
        let _ = ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

fn colorref(c: Rgb) -> COLORREF {
    COLORREF(c.r as u32 | (c.g as u32) << 8 | (c.b as u32) << 16)
}

fn register_class() -> Result<()> {
    static REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ok = *REGISTERED.get_or_init(|| {
        // SAFETY: クラスの情報は呼び出しの間有効。
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(pane_proc),
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
            "プレビュー",
            "ウィンドウクラスを登録できませんでした",
        ))
    }
}

fn state_of(hwnd: HWND) -> Option<&'static PaneInner> {
    // SAFETY: GWLP_USERDATA には作成時に `Rc::into_raw` した参照を入れ、WM_NCDESTROY で
    // 外して戻す。UI スレッドだけが触り、WM_NCDESTROY の後は呼ばれない。
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const PaneInner;
        ptr.as_ref()
    }
}

unsafe extern "system" fn pane_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_SIZE => {
            if let Some(s) = state_of(hwnd) {
                s.resize_controller();
            }
            return LRESULT(0);
        }
        WM_SETFOCUS => {
            if let Some(s) = state_of(hwnd) {
                let c = s.controller.borrow().clone();
                if let Some(c) = c {
                    let _ = c.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
                }
            }
            return LRESULT(0);
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if let Some(s) = state_of(hwnd) {
                let palette = s.palette.borrow().clone();
                let mut r = RECT::default();
                let _ = GetClientRect(hwnd, &mut r);
                let brush = CreateSolidBrush(colorref(palette.background));
                FillRect(hdc, &r, brush);
                let _ = DeleteObject(brush.into());
                if let PaneStatus::Unavailable(reason) = &*s.status.borrow() {
                    let text = to_wide(&guidance_text(reason));
                    let old = SelectObject(hdc, GetStockObject(DEFAULT_GUI_FONT));
                    SetBkMode(hdc, TRANSPARENT);
                    SetTextColor(hdc, colorref(palette.foreground));
                    r.left += 16;
                    r.top += 16;
                    r.right -= 16;
                    let mut wide = text;
                    DrawTextW(hdc, &mut wide, &mut r, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);
                    SelectObject(hdc, old);
                }
            }
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const PaneInner;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if !ptr.is_null() {
                let inner = Rc::from_raw(ptr);
                inner.destroyed.set(true);
                inner.close_controller();
                drop(inner);
            }
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
