//! WebView2 環境の遅延生成。右ペインと別ウィンドウで 1 つの環境を共有する。
//!
//! [`PreviewHost::new`] は何もしない（レジストリも DLL も COM も触らない）。
//! 最初の [`PreviewHost::with_environment`] で初めて自前ローダーを呼ぶ。

use std::cell::RefCell;
use std::rc::Rc;

use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

use crate::platform::webview2_loader;

/// 環境が使えるようになったとき（または失敗したとき）に呼ばれる。失敗は利用者に見せる理由。
pub type EnvWaiter = Box<dyn FnOnce(Result<ICoreWebView2Environment, String>)>;

enum EnvState {
    /// まだ何もしていない（失敗後もここに戻り、次の要求で再試行する）。
    Idle,
    /// 生成の完了待ち。
    Creating,
    Ready(ICoreWebView2Environment),
}

struct HostInner {
    state: RefCell<EnvState>,
    waiters: RefCell<Vec<EnvWaiter>>,
}

/// WebView2 環境の持ち主。UI スレッド専用（`Rc`）。
#[derive(Clone)]
pub struct PreviewHost {
    inner: Rc<HostInner>,
}

impl Default for PreviewHost {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewHost {
    /// 何もしない。
    pub fn new() -> Self {
        Self {
            inner: Rc::new(HostInner {
                state: RefCell::new(EnvState::Idle),
                waiters: RefCell::new(Vec::new()),
            }),
        }
    }

    /// 環境の生成を始めたか（または済んだか）。起動時に何も呼ばれていないことのテスト用。
    pub fn is_started(&self) -> bool {
        !matches!(*self.inner.state.borrow(), EnvState::Idle)
    }

    /// 環境が使えるようになったら `waiter` を呼ぶ。初めてなら、ここでランタイムを探して
    /// 生成を始める。失敗したら `Err(理由)` で呼ぶ（次の要求で再試行する）。
    pub fn with_environment(&self, waiter: EnvWaiter) {
        let ready = match &*self.inner.state.borrow() {
            EnvState::Ready(env) => Some(env.clone()),
            _ => None,
        };
        if let Some(env) = ready {
            waiter(Ok(env));
            return;
        }
        self.inner.waiters.borrow_mut().push(waiter);
        if matches!(*self.inner.state.borrow(), EnvState::Creating) {
            return;
        }
        *self.inner.state.borrow_mut() = EnvState::Creating;
        // WebView2 のコールバックはメッセージループ経由で届く。STA が必要（既に初期化済み、
        // 別のモードで初期化済みの場合の失敗は無視してよい）。
        // SAFETY: スレッドの COM 初期化だけ。
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let Some(dir) = webview2_loader::default_user_data_dir() else {
            self.finish(Err("LOCALAPPDATA が設定されていません".to_owned()));
            return;
        };
        let me = self.clone();
        let started = webview2_loader::create_environment(&dir, move |res| {
            me.finish(res.map_err(|e| format!("環境を作れませんでした: {}", e.message())));
        });
        if let Err(e) = started {
            self.finish(Err(unavailable_reason(&e)));
        }
    }

    fn finish(&self, result: Result<ICoreWebView2Environment, String>) {
        *self.inner.state.borrow_mut() = match &result {
            Ok(env) => EnvState::Ready(env.clone()),
            Err(_) => EnvState::Idle,
        };
        let waiters = std::mem::take(&mut *self.inner.waiters.borrow_mut());
        for w in waiters {
            w(result.clone());
        }
    }
}

fn unavailable_reason(e: &crate::core::Error) -> String {
    match e {
        crate::core::Error::Unavailable { reason, .. } => reason.clone(),
        other => other.to_string(),
    }
}
