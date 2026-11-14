//! 巨大ファイルのビュー（`ui::bigview`）の画面テスト。スクロール、行ジャンプ、選択とコピー、
//! 検索、描画（Direct2D と GDI の両方）を確かめる。
//!
//! 画面を作れない環境では飛ばす。必ず実行するには `LINOTE_REQUIRE_GUI=1` を付ける:
//! `xvfb-run -a env LINOTE_REQUIRE_GUI=1 WINEDEBUG=-all cargo test --test bigview_gui`
#![cfg(windows)]

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;

use linote::bigfile::{BigFile, BigFileConfig, Direction, SearchPattern};
use linote::core::TaskContext;
use linote::document::TextRange;
use linote::ui::bigview::{BigView, BigViewEvent, GotoResult};
use windows::core::w;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::UpdateWindow;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, PeekMessageW, TranslateMessage, MSG,
    PM_REMOVE, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

fn fixture(lines: usize) -> (std::path::PathBuf, Arc<BigFile>) {
    let dir = std::env::temp_dir().join(format!("linote-bigview-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("big.txt");
    let mut f = std::fs::File::create(&path).unwrap();
    for i in 1..=lines {
        writeln!(
            f,
            "line {i} 日本語 needle{}",
            if i % 100 == 0 { "!" } else { "" }
        )
        .unwrap();
    }
    drop(f);
    let file = Arc::new(BigFile::open(&path, BigFileConfig::default()).unwrap());
    file.build_index(&TaskContext::detached()).unwrap();
    (dir, file)
}

fn pump() {
    let mut msg = MSG::default();
    // SAFETY: 自分のスレッドのメッセージだけを処理する。
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn host() -> Option<HWND> {
    // SAFETY: 標準の STATIC クラスで親ウィンドウを作る。
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("host"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            0,
            0,
            700,
            500,
            None,
            None,
            GetModuleHandleW(None).ok().map(|h| h.into()),
            None,
        )
        .ok()
    }
}

fn exercise(view: BigView, file: Arc<BigFile>, events: Rc<RefCell<Vec<BigViewEvent>>>) {
    view.set_source(Some(file.clone()));
    pump();
    // 先頭: 行番号は 1 から。
    let lines = view.visible_lines();
    assert!(lines.len() >= 5, "表示行が少ない: {}", lines.len());
    assert_eq!(lines[0].0, Some(1));
    assert!(lines[0].1.starts_with("line 1 "));

    // 行ジャンプ。
    assert_eq!(view.goto_line(1234), GotoResult::Done);
    let lines = view.visible_lines();
    assert!(lines
        .iter()
        .any(|(n, t)| *n == Some(1234) && t.starts_with("line 1234 ")));
    assert_eq!(view.goto_line(0), GotoResult::OutOfRange { total: 0 });
    assert_eq!(
        view.goto_line(10_000_000),
        GotoResult::OutOfRange { total: 5001 }
    );

    // 選択とコピー範囲。
    let start = file.rows_forward(0, 1).unwrap()[0].next;
    view.set_selection(TextRange::new(0, start));
    assert_eq!(view.selected_text().unwrap(), "line 1 日本語 needle\n");
    view.select_all();
    assert_eq!(
        view.selection(),
        Some(TextRange::new(0, linote::document::TextSource::len(&*file)))
    );

    // 末尾へ。
    view.scroll_to_end();
    let lines = view.visible_lines();
    assert_eq!(lines.last().unwrap().0, Some(5001));

    // 先頭へ戻ってスクロール。
    view.scroll_to_start();
    view.scroll_rows(10);
    assert_eq!(view.visible_lines()[0].0, Some(11));
    view.scroll_rows(-3);
    assert_eq!(view.visible_lines()[0].0, Some(8));
    view.scroll_rows(-100);
    assert_eq!(view.top_offset(), 0);

    // 描画（例外なく終わる）。
    // SAFETY: 自分のウィンドウ。
    unsafe {
        let _ = UpdateWindow(view.hwnd());
    }
    pump();

    // 検索: 100 行ごとの "needle!"。
    events.borrow_mut().clear();
    let pattern = Arc::new(SearchPattern::literal("needle!", true).unwrap());
    assert!(view.find(pattern.clone(), Direction::Forward));
    wait_found(&events);
    let first = view.hit().expect("ヒットがある");
    assert_eq!(file.read_text(first, 100).unwrap(), "needle!");
    assert!(view.visible_lines().iter().any(|(n, _)| *n == Some(100)));
    events.borrow_mut().clear();
    assert!(view.find(pattern.clone(), Direction::Forward));
    wait_found(&events);
    let second = view.hit().unwrap();
    assert!(second.start > first.start);
    assert!(view.visible_lines().iter().any(|(n, _)| *n == Some(200)));
    events.borrow_mut().clear();
    assert!(view.find(pattern, Direction::Backward));
    wait_found(&events);
    assert_eq!(view.hit(), Some(first));

    // 見つからない。
    events.borrow_mut().clear();
    let none = Arc::new(SearchPattern::literal("zzzzqqq", true).unwrap());
    view.find(none, Direction::Forward);
    wait_found(&events);
    assert!(events
        .borrow()
        .iter()
        .any(|e| matches!(e, BigViewEvent::Found { found: false, .. })));

    // 状態
    let st = view.status().unwrap();
    assert!(st.read_only);
    assert_eq!(st.index.lines, 5001);
    let saved = view.state();
    view.scroll_to_end();
    view.restore_state(saved);
    assert_eq!(view.state().top, saved.top);
}

fn wait_found(events: &Rc<RefCell<Vec<BigViewEvent>>>) {
    for _ in 0..500 {
        pump();
        if events
            .borrow()
            .iter()
            .any(|e| matches!(e, BigViewEvent::Found { .. }))
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("検索の結果が届かない");
}

#[test]
fn bigview_scroll_goto_copy_find() {
    let Some(parent) = host() else {
        if std::env::var_os("LINOTE_REQUIRE_GUI").is_some() {
            panic!("画面を作れない");
        }
        eprintln!("画面を作れないので飛ばします");
        return;
    };
    // SAFETY: 自分のモジュールのハンドルを取るだけ。
    let instance = unsafe { GetModuleHandleW(None) }
        .map(|h| h.into())
        .expect("module handle");
    let (dir, file) = fixture(5000);
    for gdi in [false, true] {
        let view = BigView::create(parent, instance, 200, true).unwrap();
        view.move_to(windows::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: 600,
            bottom: 400,
        });
        view.set_force_gdi(gdi);
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        view.set_callback(Rc::new(move |e| sink.borrow_mut().push(e.clone())));
        eprintln!("renderer (force_gdi={gdi}): {}", view.renderer());
        if gdi {
            assert_eq!(view.renderer(), "GDI");
        }
        exercise(view, file.clone(), events);
        view.destroy();
    }
    // SAFETY: 自分で作ったウィンドウ。
    unsafe {
        let _ = DestroyWindow(parent);
    }
    drop(file);
    let _ = std::fs::remove_dir_all(dir);
}
