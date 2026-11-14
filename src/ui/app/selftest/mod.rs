//! 画面の自己診断。実際にウィンドウと Scintilla を作り、タブの追加・切り替え・閉じる、
//! 本文の設定と取得、版の進み方、Undo と保存位置、AI の編集の 1 単位化、矩形選択、
//! テーマの切り替え、ワーカーからの通路を確かめる。
//!
//! Wine＋Xvfb で動かす: `xvfb-run -a env LINOTE_REQUIRE_GUI=1 cargo test --test ui_selftest`
//! （`tests/ui_selftest.rs`）。統合後は `linote.exe --self-test` からも呼べる（main.rs の変更案）。
//!
//! 戻り値: [`PASSED`]、[`FAILED`]、または画面を作れない環境で [`UNAVAILABLE`]。

use std::rc::Rc;
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
use crate::appdata::{AppPaths, ThemeMode};
use crate::command::{CommandDef, CommandId, Effect, Invocation, Registry, TargetKind, Value};
use crate::core::{Actor, Error, TargetRef};
use crate::document::{
    Capabilities, Capability, DocumentState, EditTarget, Selection, TextRange, TextSource,
};
use crate::ui::dialogs::ScriptedPrompts;
use crate::ui::editor::Language;

mod integration;
mod integration_i2;
mod integration_u1;
mod integration_u2;
mod integration_u3;
mod integration_u4;
mod integration_u5;
mod integration_v1;
mod integration_v2;
mod integration_v4;

/// 統合の試験の道具立て。
pub(crate) struct Ctx {
    pub dir: std::path::PathBuf,
    pub paths: AppPaths,
    pub prompts: Rc<ScriptedPrompts>,
}

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

fn editor_visible(app: &App) -> bool {
    // SAFETY: ウィンドウの表示状態を調べるだけ。
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(app.editor().hwnd()).as_bool()
    }
}

fn text_of(app: &mut App, id: crate::core::DocumentId) -> String {
    app.with_text(id, |ed| ed.text()).unwrap_or_default()
}

/// 自己診断を行い、結果のコードを返す。標準エラーに経過を書く。
pub fn run_self_test() -> i32 {
    crate::platform::system::enable_per_monitor_dpi();
    // データ（設定・メモ・復元データ）は試験用の場所に置く。
    let dir = std::env::temp_dir().join(format!(
        "linote-selftest-{}-{}",
        std::process::id(),
        crate::core::MemoId::generate()
    ));
    let paths = AppPaths::with_root(dir.join("data"));
    if std::fs::create_dir_all(&dir).is_err() {
        eprintln!("self-test: 作業用のフォルダーを作れません");
        return UNAVAILABLE;
    }
    let options = RunOptions {
        theme: Some(ThemeMode::Dark),
        paths: Some(paths.clone()),
        ..Default::default()
    };
    if let Err(e) = start(options) {
        eprintln!("self-test: 画面を作れません: {e}");
        return UNAVAILABLE;
    }
    // 利用者への確認は台本どおりに答える。
    let prompts = Rc::new(ScriptedPrompts::new());
    with_app(|app| app.set_prompts(prompts.clone()));
    let ctx = Ctx {
        dir: dir.clone(),
        paths,
        prompts,
    };
    let result = with_app(checks_basic)
        .unwrap_or_else(|| Err("アプリを借りられません".into()))
        .and_then(|()| check_ui_channel())
        .and_then(|()| with_app(checks_commands).unwrap_or_else(|| Err("借用".into())))
        .and_then(|()| with_app(checks_close).unwrap_or_else(|| Err("借用".into())))
        .and_then(|()| with_app(checks_misc).unwrap_or_else(|| Err("借用".into())))
        .and_then(|()| integration::run(&ctx));
    with_app(|app| app.force_exit());
    let code = message_loop();
    let _ = std::fs::remove_dir_all(&dir);
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
    // 起動直後は文書 0 件（無題を作らない。V1）。編集欄は隠れ、案内を描く。
    check!(app.documents().is_empty(), "起動時の文書の数");
    check!(app.active_id().is_none(), "起動時はアクティブな文書がない");
    check!(app.tabs.items().is_empty(), "起動時のタブ");
    check!(!editor_visible(app), "文書 0 件では編集欄を隠す");
    check!(
        app.status.info() == &Default::default(),
        "文書 0 件のステータス"
    );
    // 文書を前提とするコマンドは何もしない（パニックしない）。
    for cmd in [
        "file.save",
        "file.save_as",
        "file.save_all",
        "file.close",
        "file.print",
        "edit.undo",
        "edit.select_all",
        "edit.goto_line",
        "edit.rectangular_select",
        "search.find",
        "search.find_next",
        "view.preview_pane",
        "view.next_tab",
        "view.prev_tab",
    ] {
        app.run_command(CommandId::new(cmd), Value::Null);
    }
    check!(
        app.documents().is_empty(),
        "文書を前提とするコマンドが何かした"
    );
    check!(app.find_panel.is_none(), "検索欄が開いた");
    check!(
        app.needs_document(CommandId::new("file.save"))
            && app.needs_document(CommandId::new("search.find"))
            && !app.needs_document(CommandId::new("file.new"))
            && !app.needs_document(CommandId::new("file.open")),
        "文書を前提とするコマンドの判定"
    );
    // 最近使ったファイルの案内も描ける（当たり判定ができる）。
    app.settings.recent_files = vec!["C:\\work\\a.txt".into(), "C:\\work\\b.txt".into()];
    app.paint_for_test();
    check!(
        app.empty_links.len() == 2,
        "案内の最近使ったファイル {}",
        app.empty_links.len()
    );
    app.settings.recent_files.clear();
    // 新規（file.new）で文書ができ、編集欄が現れる。
    app.execute(&Invocation::user(FILE_NEW.id)).map_err(e2s)?;
    check!(app.documents().len() == 1, "新規で文書ができる");
    check!(editor_visible(app), "文書があれば編集欄を出す");
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
    super::commands::register(&mut registry).map_err(e2s)?;
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
    // 未保存の文書は、確認で取り消せば閉じない（台本の既定は取り消し）。
    let dirty = app.active_id().unwrap();
    check!(app.state(dirty).unwrap().is_dirty(), "未保存の文書");
    let r =
        app.execute(&Invocation::user(FILE_CLOSE.id).with_target(TargetRef::Document(dirty), None));
    check!(
        r.map(|o| o.value["closed"] == false).unwrap_or(false),
        "未保存の文書を確認なしで閉じた"
    );
    check!(app.state(dirty).is_some(), "閉じていない");

    // 強制なら閉じ、隣のタブがアクティブになる。
    let n = app.documents().len();
    check!(
        app.close_document(dirty, true).map_err(e2s)?,
        "強制で閉じる"
    );
    check!(app.documents().len() == n - 1, "文書の数");
    check!(app.state(dirty).is_none(), "閉じた文書が残っている");
    check!(app.active_id().is_some(), "隣のタブ");

    // 全部閉じると文書 0 件になる（無題は作らない。V1）。
    let ids: Vec<_> = app.documents().iter().map(|d| d.id()).collect();
    for id in ids {
        app.close_document(id, true).map_err(e2s)?;
    }
    check!(app.documents().is_empty(), "最後の文書を閉じた後");
    check!(app.active_id().is_none(), "アクティブな文書がない");
    check!(app.tabs.items().is_empty(), "タブが空");
    check!(!editor_visible(app), "編集欄を隠す");
    check!(app.editor().length() == 0, "編集欄は空のドキュメント");
    check!(app.status.info() == &Default::default(), "ステータスは空");
    // 以降の試験のために、文書を 1 つ開き直す。
    app.new_untitled().map_err(e2s)?;
    check!(editor_visible(app), "文書を開けば編集欄が出る");
    check!(app.documents().len() == 1, "開き直した文書");
    Ok(())
}

fn checks_misc(app: &mut App) -> Check {
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
