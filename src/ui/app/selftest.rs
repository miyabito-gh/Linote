//! 画面の自己診断。実際にウィンドウと Scintilla を作り、タブの追加・切り替え・閉じる、
//! 本文の設定と取得、版の進み方、Undo と保存位置、AI の編集の 1 単位化、矩形選択、
//! テーマの切り替え、ワーカーからの通路を確かめる。
//!
//! Wine＋Xvfb で動かす: `xvfb-run -a env LINOTE_REQUIRE_GUI=1 cargo test --test ui_selftest`
//! （`tests/ui_selftest.rs`）。統合後は `linote.exe --self-test` からも呼べる（main.rs の変更案）。
//!
//! 戻り値: [`PASSED`]、[`FAILED`]、または画面を作れない環境で [`UNAVAILABLE`]。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows::core::w;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    WINDOW_EX_STYLE, WS_CHILD,
};

use super::{
    message_loop, start, with_app, App, DocView, RunOptions, UiMessage, FILE_CLOSE, FILE_NEW,
    VIEW_ACTIVATE, VIEW_NEXT_TAB,
};
use crate::appdata::ThemeMode;
use crate::command::{CommandDef, CommandId, Effect, Invocation, Registry, TargetKind, Value};
use crate::core::{Actor, Error, TargetRef};
use crate::document::{
    Capabilities, Capability, DocumentKind, DocumentState, EditTarget, Selection, TextRange,
    TextSource,
};
use crate::ui::editor::Language;

pub const PASSED: i32 = 0;
pub const FAILED: i32 = 1;
pub const UNAVAILABLE: i32 = 2;

/// 試験用の編集コマンド（AI からも呼べる）。
const TEST_APPEND: CommandDef = CommandDef::new(
    CommandId::new("test.append"),
    "末尾に追加",
    "試験用: 末尾に 2 回に分けて文字列を足す。",
)
.target(TargetKind::Document)
.requires(Capabilities::of(&[Capability::Edit]))
.effect(Effect::Edit)
.ai_callable();

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!()));
        }
    };
}

type Check = std::result::Result<(), String>;

fn pump_until(mut done: impl FnMut() -> bool) -> bool {
    let start = std::time::Instant::now();
    let mut msg = MSG::default();
    while start.elapsed() < std::time::Duration::from_secs(10) {
        if done() {
            return true;
        }
        // SAFETY: 自分のスレッドのメッセージを処理する。
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    done()
}

fn text_of(app: &mut App, id: crate::core::DocumentId) -> String {
    app.with_text(id, |ed| ed.text()).unwrap_or_default()
}

/// 自己診断を行い、結果のコードを返す。標準エラーに経過を書く。
pub fn run_self_test() -> i32 {
    crate::platform::system::enable_per_monitor_dpi();
    let options = RunOptions {
        theme: ThemeMode::Dark,
        ..Default::default()
    };
    if let Err(e) = start(options) {
        eprintln!("self-test: 画面を作れません: {e}");
        return UNAVAILABLE;
    }
    let result = with_app(checks_basic)
        .unwrap_or_else(|| Err("アプリを借りられません".into()))
        .and_then(|()| check_ui_channel())
        .and_then(|()| with_app(checks_commands).unwrap_or_else(|| Err("借用".into())))
        .and_then(|()| with_app(checks_close).unwrap_or_else(|| Err("借用".into())))
        .and_then(|()| with_app(checks_misc).unwrap_or_else(|| Err("借用".into())));
    with_app(|app| app.force_exit());
    let code = message_loop();
    match result {
        Ok(()) if code == 0 => {
            eprintln!("self-test: ok");
            PASSED
        }
        Ok(()) => {
            eprintln!("self-test: メッセージループの終了コード {code}");
            FAILED
        }
        Err(e) => {
            eprintln!("self-test: 失敗: {e}");
            FAILED
        }
    }
}

fn checks_basic(app: &mut App) -> Check {
    // 起動直後は空の文書が 1 つ。
    check!(app.documents().len() == 1, "起動時の文書の数");
    let first = app.active_id().ok_or("アクティブな文書がない")?;
    check!(!app.state(first).unwrap().is_dirty(), "空の文書は未変更");
    check!(app.theme().dark, "ダークテーマ");
    check!(
        app.layout().tabs.bottom > 0 && app.layout().status.top > app.layout().editor.top,
        "配置 {:?}",
        app.layout()
    );

    // 本文を設定すると版が進み、未保存になる。取得すると同じ内容。
    let v0 = app.state(first).unwrap().version();
    let text = "日本語\tタブ\r\n二行目\r\n";
    app.with_text(first, |ed| ed.set_text(text)).map_err(e2s)?;
    let st = app.state(first).unwrap();
    check!(st.version() > v0, "版が進まない");
    check!(st.is_dirty(), "未保存にならない");
    check!(text_of(app, first) == text, "本文の取得");
    check!(app.tabs.items()[0].dirty, "タブの未保存の丸");
    check!(
        app.editor().line_count() == 3,
        "行数 {}",
        app.editor().line_count()
    );

    // Undo で保存位置に戻ると未変更。版は戻らない。
    let v1 = app.state(first).unwrap().version();
    app.with_text(first, |ed| ed.undo()).map_err(e2s)?;
    let st = app.state(first).unwrap();
    check!(st.version() > v1, "Undo でも版が進む");
    check!(!st.is_dirty(), "保存位置に戻ったのに未保存のまま");
    check!(text_of(app, first).is_empty(), "Undo 後の本文");
    app.with_text(first, |ed| ed.redo()).map_err(e2s)?;
    check!(app.state(first).unwrap().is_dirty(), "Redo で未保存に戻る");

    // 2 つ目のタブ。切り替えても本文はそれぞれに残る。
    let second = app
        .open_text(
            DocumentState::clipboard(),
            "fn main() {\n    println!(\"hi\");\n}\n",
            "clip.rs".into(),
            Language::Rust,
            true,
        )
        .map_err(e2s)?;
    check!(app.active_id() == Some(second), "新しいタブがアクティブ");
    check!(app.documents().len() == 2, "タブの数");
    check!(app.tabs.items().len() == 2, "タブバーの項目の数");
    // クリップボード由来は作成直後から未保存。読み込みでは版は進まない。
    let st = app.state(second).unwrap();
    check!(st.is_dirty(), "クリップボード由来は未保存");
    check!(
        st.version() == crate::core::Version::INITIAL,
        "読み込みで版が進んだ"
    );
    check!(
        app.editor().length() == 34,
        "長さ {}",
        app.editor().length()
    );
    // 読み込んだ文書も Undo できる（読み込み口が切った記録を戻している）。
    app.with_text(second, |ed| {
        ed.set_selection(Selection::caret_at(0));
        ed.replace(TextRange::at(0), "// x\n")
    })
    .map_err(e2s)?
    .map_err(e2s)?;
    check!(app.editor().can_undo(), "読み込んだ文書の Undo");
    app.with_text(second, |ed| ed.undo()).map_err(e2s)?;
    // 未保存で開いた文書は、Undo で読み込み時点に戻っても未保存のまま。
    check!(
        app.state(second).unwrap().is_dirty(),
        "読み込み時点で未変更扱い"
    );

    // 矩形選択と行ジャンプ。
    app.editor().set_rectangular_selection(0, 20);
    check!(app.editor().is_rectangular_selection(), "矩形選択");
    app.editor().goto_line(2);
    check!(
        app.editor()
            .line_from_position(app.editor().selection().caret)
            == 2,
        "行ジャンプ"
    );
    let vs_second = app.editor().selection();

    // 切り替え。
    app.activate(first).map_err(e2s)?;
    check!(app.editor().text() == text, "切り替え後の本文");
    app.activate(second).map_err(e2s)?;
    check!(app.editor().text().starts_with("fn main"), "戻した本文");
    check!(app.editor().selection() == vs_second, "選択の復元");

    // 表示していない文書の編集（隠れた編集欄）。版はその文書だけ進む。
    let v_first = app.state(first).unwrap().version();
    let v_second = app.state(second).unwrap().version();
    app.with_text(first, |ed| ed.replace(TextRange::at(0), "先頭"))
        .map_err(e2s)?
        .map_err(e2s)?;
    check!(
        app.state(first).unwrap().version() > v_first,
        "裏の文書の版"
    );
    check!(
        app.state(second).unwrap().version() == v_second,
        "表の文書の版"
    );
    check!(
        text_of(app, first).starts_with("先頭日本語"),
        "裏の文書の本文"
    );
    // 文字の途中は拒否する。
    let bad = app
        .with_text(first, |ed| ed.replace(TextRange::new(1, 2), ""))
        .map_err(e2s)?;
    check!(
        matches!(bad, Err(Error::InvalidArgument { .. })),
        "文字の途中の置換"
    );
    // TextSource の窓口。
    let (len, line1) = app
        .with_text(first, |ed| (ed.len(), TextSource::line_start(ed, 1)))
        .map_err(e2s)?;
    check!(len == text.len() as u64 + 6, "長さ");
    check!(line1.ok().flatten() == Some(6 + 18), "2 行目の先頭");
    Ok(())
}

fn check_ui_channel() -> Check {
    let flag = Arc::new(AtomicBool::new(false));
    let sender = with_app(|app| app.sender()).ok_or("借用")?;
    let f = Arc::clone(&flag);
    std::thread::spawn(move || {
        sender.send(UiMessage::Status("ワーカーから".into()));
        sender.send(UiMessage::Run(Box::new(move |app: &mut App| {
            if app.status_message() == Some("ワーカーから") {
                f.store(true, Ordering::SeqCst);
            }
        })));
    });
    check!(
        pump_until(|| flag.load(Ordering::SeqCst)),
        "ワーカーからの通路"
    );
    Ok(())
}

fn checks_commands(app: &mut App) -> Check {
    let mut registry: Registry<App> = Registry::new();
    super::register(&mut registry).map_err(e2s)?;
    registry
        .register(TEST_APPEND, |app, call| {
            let id = call.document()?;
            app.with_text(id, |ed| -> crate::core::Result<()> {
                let end = ed.length();
                ed.replace(TextRange::at(end), "A")?;
                let end = ed.length();
                ed.replace(TextRange::at(end), "B")
            })??;
            Ok(Value::Null)
        })
        .map_err(e2s)?;
    app.set_registry(registry);

    let before = app.documents().len();
    app.execute(&Invocation::user(FILE_NEW.id)).map_err(e2s)?;
    check!(app.documents().len() == before + 1, "file.new");
    let doc = app.active_id().unwrap();

    // AI の編集: 前提の版が必要。1 回の Undo で戻る。
    let v = app.state(doc).unwrap().version();
    let mut inv = Invocation::user(TEST_APPEND.id).with_target(TargetRef::Document(doc), Some(v));
    inv.actor = Actor::Ai;
    let out = app.execute(&inv).map_err(e2s)?;
    let st = app.state(doc).unwrap();
    check!(out.version == Some(st.version()), "実行後の版");
    check!(st.version() > v, "AI の編集で版が進む");
    check!(st.has_ai_edits(), "AI の変更の印");
    check!(st.last_actor() == Actor::Ai, "最後の実行者");
    check!(app.tabs.items().last().unwrap().ai_edits, "タブの AI の印");
    check!(text_of(app, doc) == "AB", "AI の編集の本文");
    // 古い版を前提にした書き込みは拒否される。
    let stale = app.execute(&inv);
    check!(
        matches!(stale, Err(Error::VersionMismatch { .. })),
        "古い版の拒否"
    );
    app.with_text(doc, |ed| ed.undo()).map_err(e2s)?;
    check!(
        text_of(app, doc).is_empty(),
        "AI の編集は 1 回の Undo で戻る"
    );
    // 利用者の通常の編集は User として記録される。
    app.with_text(doc, |ed| ed.set_text("x")).map_err(e2s)?;
    check!(
        app.state(doc).unwrap().last_actor() == Actor::User,
        "利用者の編集"
    );

    // タブの切り替えのコマンド。
    let ids: Vec<_> = app.documents().iter().map(|d| d.id()).collect();
    app.execute(&Invocation::user(VIEW_NEXT_TAB.id))
        .map_err(e2s)?;
    check!(app.active_id() == Some(ids[0]), "次のタブ（端で回る）");
    app.execute(&Invocation::user(VIEW_ACTIVATE.id).with_target(TargetRef::Document(ids[1]), None))
        .map_err(e2s)?;
    check!(app.active_id() == Some(ids[1]), "view.activate");
    Ok(())
}

fn checks_close(app: &mut App) -> Check {
    // 未保存の文書は、確認のフックが断れば閉じない（既定のフックは断る）。
    let dirty = app.active_id().unwrap();
    check!(app.state(dirty).unwrap().is_dirty(), "未保存の文書");
    let r =
        app.execute(&Invocation::user(FILE_CLOSE.id).with_target(TargetRef::Document(dirty), None));
    check!(
        r.map(|o| o.value["closed"] == false).unwrap_or(false),
        "未保存の文書を確認なしで閉じた"
    );
    check!(app.state(dirty).is_some(), "閉じていない");
    check!(app.status_message().is_some(), "状態表示");

    // 強制なら閉じ、隣のタブがアクティブになる。
    let n = app.documents().len();
    check!(
        app.close_document(dirty, true).map_err(e2s)?,
        "強制で閉じる"
    );
    check!(app.documents().len() == n - 1, "文書の数");
    check!(app.state(dirty).is_none(), "閉じた文書が残っている");
    check!(app.active_id().is_some(), "隣のタブ");

    // 全部閉じると新しい空の文書が 1 つ残る。
    let ids: Vec<_> = app.documents().iter().map(|d| d.id()).collect();
    for id in ids {
        app.close_document(id, true).map_err(e2s)?;
    }
    check!(app.documents().len() == 1, "最後の文書を閉じた後");
    let id = app.active_id().unwrap();
    check!(
        *app.state(id).unwrap().kind() == DocumentKind::Untitled,
        "空の文書"
    );
    check!(app.editor().length() == 0, "空の本文");
    Ok(())
}

fn checks_misc(app: &mut App) -> Check {
    // 仮の入出力で保存し、もう一度開くと同じ文書になる。
    let dir = std::env::temp_dir().join(format!(
        "linote-selftest-{}-{}",
        std::process::id(),
        crate::core::MemoId::generate()
    ));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("saved.md");
    let id = app.active_id().unwrap();
    app.with_text(id, |ed| ed.set_text("# 見出し\n本文\n"))
        .map_err(e2s)?;
    app.save_stub(id, Some(path.clone())).map_err(e2s)?;
    let st = app.state(id).unwrap();
    check!(!st.is_dirty(), "保存後は未変更");
    check!(
        *st.kind() == DocumentKind::File,
        "保存で通常のファイルになる"
    );
    check!(
        app.document(id).unwrap().language == Language::Markdown,
        "言語"
    );
    check!(app.document(id).unwrap().title == "saved.md", "題名");
    check!(
        std::fs::read_to_string(&path).map_err(|e| e.to_string())? == "# 見出し\n本文\n",
        "保存した内容"
    );
    // 保存後の編集を Undo すると、保存位置で未変更に戻る。
    app.with_text(id, |ed| ed.replace(TextRange::at(0), "x"))
        .map_err(e2s)?
        .map_err(e2s)?;
    check!(app.state(id).unwrap().is_dirty(), "保存後の編集");
    app.with_text(id, |ed| ed.undo()).map_err(e2s)?;
    check!(!app.state(id).unwrap().is_dirty(), "保存位置への Undo");
    let again = app.open_path_stub(&path).map_err(e2s)?;
    check!(again == id, "同じファイルは同じタブ");

    // 別のファイルとして開き直すと、読み込んだ本文と形式が入る。
    let other = dir.join("b.txt");
    std::fs::write(&other, "a\nb\n").map_err(|e| e.to_string())?;
    let b = app.open_path_stub(&other).map_err(e2s)?;
    check!(
        app.state(b).unwrap().format.eol == crate::text::Eol::Lf,
        "改行形式"
    );
    check!(app.editor().text() == "a\nb\n", "開いた本文");
    check!(app.status.info().eol == "LF", "ステータスバーの改行");
    check!(app.status.info().line == 1, "ステータスバーの行");
    let _ = std::fs::remove_dir_all(&dir);

    // テーマの切り替え。
    app.set_theme_mode(ThemeMode::Light);
    check!(!app.theme().dark, "ライトテーマ");
    app.set_theme_mode(ThemeMode::Dark);
    check!(app.theme().dark, "ダークテーマに戻す");

    // 右ペインと下部の差し込み欄。
    // SAFETY: 試験用の子ウィンドウ（static コントロール）。
    let pane = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!(""),
            WS_CHILD,
            0,
            0,
            0,
            0,
            Some(app.hwnd()),
            None,
            Some(app.instance()),
            None,
        )
    }
    .map_err(|e| e.to_string())?;
    app.set_right_pane(Some(pane));
    let l = *app.layout();
    check!(
        l.right.is_some() && l.editor.right < l.right.unwrap().left,
        "右ペイン"
    );
    app.set_bottom_panel(Some((pane, 30)));
    check!(app.layout().bottom.is_some(), "下部の差し込み欄");
    app.set_right_pane(None);
    app.set_bottom_panel(None);
    check!(app.layout().right.is_none(), "右ペインを外す");
    check!(
        super::window_exists(pane),
        "差し込んだウィンドウは持ち主のもの"
    );

    // 外部の表示部品の文書（巨大ファイルの表示の差し込み口）。
    let ext = app
        .add_external(
            DocumentState::open_file(
                "big.log".into(),
                true,
                crate::text::TextFormat::DEFAULT,
                crate::file::FileStamp {
                    len: 0,
                    modified: None,
                },
            ),
            pane,
            "big.log".into(),
            true,
        )
        .map_err(e2s)?;
    check!(
        matches!(app.document(ext).unwrap().view(), DocView::External(h) if h == pane),
        "外部の表示部品"
    );
    check!(app.status.info().read_only, "読み取り専用の表示");
    check!(
        app.with_text(ext, |_| ()).is_err(),
        "外部の文書は編集欄で扱わない"
    );
    app.close_document(ext, true).map_err(e2s)?;
    Ok(())
}

fn e2s(e: Error) -> String {
    e.to_string()
}
