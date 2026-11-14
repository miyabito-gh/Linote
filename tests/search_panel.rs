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

use linote::appdata::settings::GrepSettings;
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

    std::fs::write(root.join("c.rs"), "needle excluded\n").unwrap();

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
                FindEvent::Scope(on) => format!("scope {on}"),
                FindEvent::Ran {
                    find,
                    replace,
                    grep_dir,
                } => format!(
                    "ran {find}|{}|{}",
                    replace.unwrap_or_default(),
                    grep_dir.is_some()
                ),
                FindEvent::GrepSettings(s) => format!(
                    "grep {}|{}|{}",
                    s.extensions, s.exclude_dirs, s.include_hidden
                ),
                FindEvent::Changed(o) => format!("changed {}", o.query),
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
    panel.set_grep_settings(&GrepSettings {
        extensions: "*.txt".into(),
        ..Default::default()
    });
    panel.set_grep_folder(&root);
    command(hwnd, 211, 0);
    assert!(
        events.borrow().iter().any(|s| s.starts_with("grep *.txt|")),
        "実行条件を保存する通知"
    );
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

    // 「選択範囲内」のチェックを押すと、切り替えの通知が届く（範囲は ui::app が決める）。
    // SAFETY: 小窓の子のチェックボックスを探して押す。
    unsafe {
        let check = FindWindowExW(Some(hwnd), None, w!("BUTTON"), w!("選択範囲内だけ"))
            .expect("選択範囲内の項目");
        SendMessageW(check, 0x00F1, Some(WPARAM(1)), None); // BM_SETCHECK
    }
    assert!(panel.in_selection());
    command(hwnd, 215, 0);
    assert_eq!(events.borrow().last().unwrap(), "scope true");
    panel.set_in_selection(false);
    assert!(!panel.in_selection());

    // 実行した語が届く（履歴用）。置換は置換語つき。
    command(hwnd, 210, 0);
    {
        let ev = events.borrow();
        assert_eq!(ev[ev.len() - 2], "ran needle||false");
    }

    // 検索欄の入力が変わると、条件の変更が届く（強調表示の更新用）。
    panel.set_query("changed-query");
    assert!(
        pump(|| events.borrow().iter().any(|e| e == "changed changed-query")),
        "条件の変更の通知"
    );

    // 履歴の一覧: 項目数、入力中の文字列は保つ。
    panel.set_history(
        &["x".into(), "y".into(), "z".into()],
        &["r".into()],
        &["C:\\src".into()],
    );
    // SAFETY: 小窓の検索欄（最初のコンボボックス）の項目数を読む。
    unsafe {
        let combo = FindWindowExW(Some(hwnd), None, w!("COMBOBOX"), None).unwrap();
        assert_eq!(SendMessageW(combo, 0x0146, None, None).0, 3); // CB_GETCOUNT
    }
    assert_eq!(panel.options().unwrap().query, "changed-query");

    // A multi-line error must retain all text, with a scrollable viewport.
    let long_status = "長い検索エラーと対象ファイルの説明 ".repeat(40);
    panel.set_status(&long_status, true);
    unsafe {
        let status = windows::Win32::UI::WindowsAndMessaging::GetDlgItem(Some(hwnd), 213).unwrap();
        assert!(SendMessageW(status, 0x00BA /* EM_GETLINECOUNT */, None, None).0 > 1);
        let mut text = vec![0u16; long_status.encode_utf16().count() + 1];
        let len = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(status, &mut text).max(0)
            as usize;
        assert_eq!(String::from_utf16_lossy(&text[..len]), long_status);
        let style = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
            status,
            windows::Win32::UI::WindowsAndMessaging::GWL_STYLE,
        ) as u32;
        assert_ne!(
            style & windows::Win32::UI::WindowsAndMessaging::WS_VSCROLL.0,
            0
        );
        SendMessageW(
            status,
            windows::Win32::UI::WindowsAndMessaging::WM_KEYDOWN,
            Some(WPARAM(0x1B)),
            None,
        );
        assert!(
            !panel.is_visible(),
            "長い状態文を読んでいるときもEscで閉じる"
        );
    }

    panel.hide();
    assert!(!panel.is_visible());
    assert!(events.borrow().contains(&"closed".to_string()));
    panel.destroy();
    let _ = std::fs::remove_dir_all(&root);
}
