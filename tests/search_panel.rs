//! 検索の小窓: 開閉、プレフィル、Grep の逐次表示、結果を開く通知、置換の無効化。
//!
//! 画面を作れない環境では飛ばす。必ず実行するには `LINOTE_REQUIRE_GUI=1`:
//! `xvfb-run -a env LINOTE_REQUIRE_GUI=1 WINEDEBUG=-all cargo test --test search_panel`
#![cfg(windows)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DispatchMessageW, FindWindowExW, PeekMessageW, SendMessageW, TranslateMessage,
    LBN_DBLCLK, LB_GETCOUNT, LB_SETCURSEL, MSG, PM_REMOVE, WM_COMMAND, WS_EX_TOOLWINDOW,
    WS_OVERLAPPED,
};

use linote::ui::find::{FindEvent, FindPanel};
use linote::ui::theme::Theme;

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

fn command(hwnd: HWND, id: usize, code: u32) {
    // SAFETY: クリックの代わりに WM_COMMAND を小窓へ送る。
    unsafe {
        SendMessageW(
            hwnd,
            WM_COMMAND,
            Some(WPARAM(id | ((code as usize) << 16))),
            Some(LPARAM(0)),
        );
    }
}

#[test]
fn panel_greps_opens_hits_and_disables_replace() {
    let root = std::env::temp_dir().join(format!("linote-find-panel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), "x\nneedle here\n").unwrap();
    std::fs::write(root.join("b.txt"), "needle again\n").unwrap();

    // SAFETY: 試験用の最上位ウィンドウ（所有者にする）。
    let owner: HWND = unsafe {
        let instance = GetModuleHandleW(None).unwrap();
        match CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("STATIC"),
            w!("owner"),
            WS_OVERLAPPED,
            0,
            0,
            100,
            100,
            None,
            None,
            Some(instance.into()),
            None,
        ) {
            Ok(h) => h,
            Err(_) => {
                if std::env::var_os("LINOTE_REQUIRE_GUI").is_none() {
                    eprintln!("画面を作れないので飛ばします");
                    let _ = std::fs::remove_dir_all(&root);
                    return;
                }
                panic!("所有者のウィンドウを作れない");
            }
        }
    };

    let events: Rc<RefCell<Vec<String>>> = Rc::default();
    let e = events.clone();
    let panel = FindPanel::create(
        owner,
        &Theme::dark(),
        96,
        Rc::new(move |ev| {
            e.borrow_mut().push(match ev {
                FindEvent::Next(o) => format!("next {}", o.query),
                FindEvent::Prev(o) => format!("prev {}", o.query),
                FindEvent::Replace { replacement, .. } => format!("replace {replacement}"),
                FindEvent::ReplaceAll { .. } => "replace_all".into(),
                FindEvent::OpenHit { path, line, column } => format!(
                    "open {} {line}:{column}",
                    path.file_name().unwrap().to_string_lossy()
                ),
                FindEvent::Closed => "closed".into(),
            })
        }),
    )
    .expect("小窓を作れる");
    assert!(!panel.is_visible());

    panel.show(Some("needle"));
    assert!(panel.is_visible());
    assert_eq!(panel.options().unwrap().query, "needle");

    // SAFETY: 小窓の子ウィンドウを探すだけ。
    let hwnd = unsafe { FindWindowExW(None, None, w!("LinoteFindPanel"), None).unwrap() };
    let list = unsafe { FindWindowExW(Some(hwnd), None, w!("LISTBOX"), None).unwrap() };

    // 次へ → イベント。
    command(hwnd, 207, 0);
    assert_eq!(events.borrow().last().unwrap(), "next needle");

    // Grep。
    panel.set_grep_folder(&root);
    command(hwnd, 211, 0);
    let count = || unsafe { SendMessageW(list, LB_GETCOUNT, None, None).0 };
    assert!(
        pump(|| count() == 2),
        "2 件が一覧に出る（実際 {}）",
        count()
    );
    // SAFETY: 1 件目を選んでダブルクリック相当を送る。
    unsafe {
        SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
    }
    command(hwnd, 214, LBN_DBLCLK);
    assert_eq!(events.borrow().last().unwrap(), "open a.txt 2:1");

    // 置換を無効にすると、置換のイベントは出ない。
    panel.set_replace_enabled(false, Some("巨大モードでは置換できません"));
    let before = events.borrow().len();
    command(hwnd, 210, 0);
    command(hwnd, 209, 0);
    assert_eq!(events.borrow().len(), before);
    panel.set_replace_enabled(true, None);
    command(hwnd, 210, 0);
    assert_eq!(events.borrow().last().unwrap(), "replace_all");

    panel.hide();
    assert!(!panel.is_visible());
    assert!(events.borrow().contains(&"closed".to_string()));
    panel.destroy();
    let _ = std::fs::remove_dir_all(&root);
}
