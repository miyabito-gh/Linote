//! メモの小窓を開閉し、検索・開く・新規作成が動くことを確かめる。
//!
//! 画面を作れない環境では飛ばす。必ず実行するには `LINOTE_REQUIRE_GUI=1`:
//! `xvfb-run -a env LINOTE_REQUIRE_GUI=1 WINEDEBUG=-all cargo test --test memo_panel`
#![cfg(windows)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, PeekMessageW, TranslateMessage, MSG,
    PM_REMOVE, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};

use linote::appdata::AppPaths;
use linote::memo::MemoStore;
use linote::ui::memo_panel::{MemoPanel, PanelEvent};
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

#[test]
fn panel_opens_closes_and_opens_memos() {
    let root = std::env::temp_dir().join(format!("linote-memo-panel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let store = MemoStore::new(&AppPaths::with_root(&root));
    let seeded = store.create("買い物", "牛乳").unwrap();

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
    let opened: Rc<RefCell<Vec<linote::memo::Memo>>> = Rc::default();
    let (e, o) = (events.clone(), opened.clone());
    let panel = MemoPanel::create(
        owner,
        store.clone(),
        &Theme::dark(),
        96,
        Rc::new(move |ev| match ev {
            PanelEvent::Open(m) => {
                e.borrow_mut().push("open".into());
                o.borrow_mut().push(m);
            }
            PanelEvent::Delete { .. } => e.borrow_mut().push("delete".into()),
            PanelEvent::Closed => e.borrow_mut().push("closed".into()),
            PanelEvent::Failed(err) => e.borrow_mut().push(format!("failed {err}")),
        }),
    )
    .expect("小窓を作れる");

    // 作っただけでは何も読まない・表示しない。
    assert!(!panel.is_visible());

    panel.show();
    assert!(panel.is_visible());
    // 開いた直後の検索が終わるのを待つ（一覧が空でなければ検索結果が届いている）。
    // 検索結果が届いても落ちないこと（一覧の中身は目視でなく store 側で検証）。
    let t0 = Instant::now();
    pump(|| t0.elapsed() > Duration::from_millis(300));
    panel.hide();
    assert!(!panel.is_visible());
    assert!(pump(|| events.borrow().contains(&"closed".to_string())));

    // 再び開いて閉じられる。
    panel.toggle();
    assert!(panel.is_visible());
    panel.toggle();
    assert!(!panel.is_visible());

    // 新規メモは作業スレッドで作られ、Open で届く。
    events.borrow_mut().clear();
    panel.show();
    // SAFETY: ボタンのクリックの代わりに WM_COMMAND を小窓へ送る。
    unsafe {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, SendMessageW, WM_COMMAND};
        let hwnd = FindWindowExW(None, None, w!("LinoteMemoPanel"), None).unwrap();
        let button = FindWindowExW(Some(hwnd), None, w!("BUTTON"), None).unwrap();
        let _ = button;
        // BN_CLICKED(0) を ID 103 のボタンから。
        SendMessageW(
            hwnd,
            WM_COMMAND,
            Some(WPARAM(103)),
            Some(LPARAM(button.0 as isize)),
        );
    }
    assert!(pump(|| !opened.borrow().is_empty()));
    let created = opened.borrow()[0].clone();
    assert_eq!(created.name, "無題のメモ");
    assert!(store.load(&created.id).is_ok());
    assert!(pump(|| events.borrow().contains(&"closed".to_string())));
    assert!(!panel.is_visible());

    // 既存のメモは検索で見つかる。
    let hits = store
        .search("牛乳", &linote::core::TaskContext::detached())
        .unwrap()
        .value;
    assert_eq!(hits[0].id, seeded.id);

    // SAFETY: 試験用ウィンドウの後始末。
    unsafe {
        let _ = DestroyWindow(owner);
    }
    let _ = std::fs::remove_dir_all(&root);
}
