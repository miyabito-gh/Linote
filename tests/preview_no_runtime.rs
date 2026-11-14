//! WebView2 ランタイムがない環境（Wine）でのプレビューの振る舞い:
//! - ホストを作っただけでは何も始めない（遅延初期化）。
//! - ペインを作ると状態が Unavailable になり、通常の編集に影響しない。
//! - 別ウィンドウも同様で、文書の設定や破棄で落ちない。
//!
//! 実機（ランタイムあり）では Ready になるので、どちらも許す。
//! `xvfb-run -a env WINEDEBUG=-all cargo test --test preview_no_runtime`
#![cfg(windows)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, PeekMessageW, TranslateMessage, MSG,
    PM_REMOVE, WS_OVERLAPPED,
};

use linote::core::DocumentId;
use linote::markdown::PreviewPalette;
use linote::ui::preview::{PaneStatus, PreviewHost, PreviewPane, PreviewWindow};

fn pump(until: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        // SAFETY: 自分のスレッドのメッセージを処理するだけ。
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if until() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn make_parent() -> Option<HWND> {
    // SAFETY: 標準クラスの最上位ウィンドウを作るだけ。
    unsafe {
        CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("parent"),
            WS_OVERLAPPED,
            0,
            0,
            400,
            300,
            None,
            None,
            GetModuleHandleW(None).ok().map(Into::into),
            None,
        )
        .ok()
    }
}

#[test]
fn host_is_lazy_and_pane_reports_unavailable_without_runtime() {
    let Some(parent) = make_parent() else {
        if std::env::var_os("LINOTE_REQUIRE_GUI").is_some() {
            panic!("ウィンドウを作れません");
        }
        eprintln!("画面を作れないので飛ばす");
        return;
    };
    let host = PreviewHost::new();
    assert!(!host.is_started(), "作っただけでは始めない");

    let palette = PreviewPalette::dark();
    let pane = PreviewPane::create(&host, parent, &palette).unwrap();
    let seen: Rc<RefCell<Vec<PaneStatus>>> = Rc::default();
    let seen2 = seen.clone();
    pane.set_on_status(move |s| seen2.borrow_mut().push(s.clone()));
    pane.set_bounds(RECT {
        left: 0,
        top: 0,
        right: 300,
        bottom: 200,
    });
    // 初期化の前に文書を渡しても落ちない。
    pane.set_document(
        DocumentId::allocate(),
        Arc::from("<html><body>x</body></html>"),
    );

    assert!(pump(|| pane.status() != PaneStatus::Initializing));
    match pane.status() {
        PaneStatus::Unavailable(reason) => assert!(!reason.is_empty()),
        PaneStatus::Ready => {} // 実機
        PaneStatus::Initializing => unreachable!(),
    }
    pane.set_palette(&PreviewPalette::light());
    pane.set_document(
        DocumentId::allocate(),
        Arc::from("<html><body>y</body></html>"),
    );
    pane.set_visible(false);
    pane.set_visible(true);
    pane.destroy();
    pane.destroy(); // 二重の破棄も安全。

    // 別ウィンドウ（同じホストを共有）。
    let win = PreviewWindow::create(&host, parent, "プレビュー", &palette).unwrap();
    win.set_document(
        DocumentId::allocate(),
        Arc::from("<html><body>z</body></html>"),
    );
    assert!(pump(|| win.status() != Some(PaneStatus::Initializing)));
    win.show();
    assert!(win.is_visible());
    win.hide();
    assert!(!win.is_visible());
    win.destroy();

    // SAFETY: 自分で作ったウィンドウ。
    unsafe {
        let _ = DestroyWindow(parent);
    }
}
