//! プレビューの別ウィンドウ。右ペインと同じ文書・同じスタイルを [`PreviewPane`] で表示する。
//! メインウィンドウの所有ウィンドウなので、メインウィンドウと一緒に閉じる。
//! 閉じるボタンは破棄せず隠す（次に開くときに WebView2 を作り直さない）。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
    IsWindowVisible, LoadCursorW, RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowTextW, ShowWindow, CW_USEDEFAULT, GWLP_USERDATA, IDC_ARROW, SW_HIDE, SW_SHOW,
    WM_CLOSE, WM_NCDESTROY, WM_SETFOCUS, WM_SIZE, WNDCLASSEXW, WS_CLIPCHILDREN,
    WS_OVERLAPPEDWINDOW,
};

use super::host::PreviewHost;
use super::pane::{PaneStatus, PreviewPane};
use crate::core::{DocumentId, Error, Result};
use crate::markdown::PreviewPalette;
use crate::platform::system::{set_dark_title_bar, to_wide};

const CLASS: PCWSTR = w!("LinotePreviewWindow");

struct WinInner {
    hwnd: Cell<HWND>,
    pane: RefCell<Option<PreviewPane>>,
    on_closed: RefCell<Option<Rc<dyn Fn()>>>,
}

/// プレビューの別ウィンドウ（UI スレッド専用）。
#[derive(Clone)]
pub struct PreviewWindow {
    inner: Rc<WinInner>,
}

impl PreviewWindow {
    /// ウィンドウを作る（まだ表示しない）。中のペインの作成が WebView2 の初期化を始めるので、
    /// 「別ウィンドウで開く」操作の中で呼ぶこと。
    pub fn create(
        host: &PreviewHost,
        owner: HWND,
        title: &str,
        palette: &PreviewPalette,
    ) -> Result<Self> {
        register_class()?;
        let inner = Rc::new(WinInner {
            hwnd: Cell::new(HWND::default()),
            pane: RefCell::new(None),
            on_closed: RefCell::new(None),
        });
        let title_w = to_wide(title);
        // SAFETY: クラスは登録済み。owner は呼び出し側が渡した有効なウィンドウ。
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                CLASS,
                PCWSTR(title_w.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                900,
                700,
                Some(owner),
                None,
                GetModuleHandleW(None).ok().map(Into::into),
                None,
            )
        }
        .map_err(|e| Error::unavailable("プレビュー", format!("ウィンドウを作れません: {e}")))?;
        inner.hwnd.set(hwnd);
        set_dark_title_bar(hwnd, palette.dark);
        let raw = Rc::into_raw(inner.clone());
        // SAFETY: hwnd は今作ったウィンドウ。参照は WM_NCDESTROY で戻す。
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };
        let pane = match PreviewPane::create(host, hwnd, palette) {
            Ok(p) => p,
            Err(e) => {
                // SAFETY: 今作ったウィンドウ。
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return Err(e);
            }
        };
        *inner.pane.borrow_mut() = Some(pane);
        let win = PreviewWindow { inner };
        win.layout();
        Ok(win)
    }

    fn layout(&self) {
        let mut r = RECT::default();
        // SAFETY: 有効なウィンドウ。
        let ok = unsafe { GetClientRect(self.hwnd(), &mut r).is_ok() };
        if ok {
            if let Some(p) = self.inner.pane.borrow().as_ref() {
                p.set_bounds(r);
            }
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.inner.hwnd.get()
    }

    /// 中のペイン（文書やテーマの設定、状態の監視に使う）。
    pub fn pane(&self) -> Option<PreviewPane> {
        self.inner.pane.borrow().clone()
    }

    pub fn status(&self) -> Option<PaneStatus> {
        self.pane().map(|p| p.status())
    }

    /// 閉じるボタンで隠されたときに呼ばれる（`ui::app` はここで本文にフォーカスを戻す）。
    pub fn set_on_closed(&self, f: impl Fn() + 'static) {
        *self.inner.on_closed.borrow_mut() = Some(Rc::new(f));
    }

    pub fn set_document(&self, key: DocumentId, html: Arc<str>) {
        if let Some(p) = self.pane() {
            p.set_document(key, html);
        }
    }

    pub fn set_palette(&self, palette: &PreviewPalette) {
        set_dark_title_bar(self.hwnd(), palette.dark);
        if let Some(p) = self.pane() {
            p.set_palette(palette);
        }
    }

    pub fn set_title(&self, title: &str) {
        let t = to_wide(title);
        // SAFETY: 有効なウィンドウ。文字列は NUL 終端。
        unsafe {
            let _ = SetWindowTextW(self.hwnd(), PCWSTR(t.as_ptr()));
        }
    }

    pub fn is_visible(&self) -> bool {
        // SAFETY: 有効なウィンドウ。
        unsafe { IsWindowVisible(self.hwnd()).as_bool() }
    }

    pub fn show(&self) {
        // SAFETY: 有効なウィンドウ。
        unsafe {
            let _ = ShowWindow(self.hwnd(), SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd());
        }
    }

    pub fn hide(&self) {
        // SAFETY: 有効なウィンドウ。
        unsafe {
            let _ = ShowWindow(self.hwnd(), SW_HIDE);
        }
    }

    /// ペインごと破棄する。
    pub fn destroy(&self) {
        if let Some(p) = self.inner.pane.borrow_mut().take() {
            p.destroy();
        }
        // SAFETY: 有効なウィンドウ（破棄済みなら失敗するだけ）。
        unsafe {
            let _ = DestroyWindow(self.hwnd());
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
                lpfnWndProc: Some(window_proc),
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

fn state_of(hwnd: HWND) -> Option<&'static WinInner> {
    // SAFETY: GWLP_USERDATA には作成時に `Rc::into_raw` した参照を入れ、WM_NCDESTROY で
    // 外して戻す。UI スレッドだけが触る。
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WinInner;
        ptr.as_ref()
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_SIZE => {
            if let Some(s) = state_of(hwnd) {
                let mut r = RECT::default();
                if GetClientRect(hwnd, &mut r).is_ok() {
                    let pane = s.pane.borrow().clone();
                    if let Some(p) = pane {
                        p.set_bounds(r);
                    }
                }
            }
            return LRESULT(0);
        }
        WM_SETFOCUS => {
            if let Some(s) = state_of(hwnd) {
                let pane = s.pane.borrow().clone();
                if let Some(p) = pane {
                    p.focus();
                }
            }
            return LRESULT(0);
        }
        WM_CLOSE => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            if let Some(s) = state_of(hwnd) {
                let cb = s.on_closed.borrow().clone();
                if let Some(cb) = cb {
                    cb();
                }
            }
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WinInner;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if !ptr.is_null() {
                drop(Rc::from_raw(ptr));
            }
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
