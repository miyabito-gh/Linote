//! 統合 I2 の自己診断: 検索・置換（Scintilla の `TextSource`／`EditTarget` 経由、Undo は 1 回）、
//! Grep の結果を開いて該当行へ、巨大ファイルのビュー（小さな閾値で強制）と行への移動・検索・
//! タブの切り替え、Markdown プレビュー（WebView2 がなければ案内が出て編集は続けられる）。

use serde_json::json;

use super::integration::{cmd, open_and_wait, step, wait};
use super::{text_of, Check, Ctx};
use crate::bigfile::LargeModeThresholds;
use crate::command::{CommandId, KeyChord, Value};
use crate::core::{DocumentId, TaskContext};
use crate::document::{Capability, DocumentKind};
use crate::search::{grep_collect, Direction, GrepOptions, SearchOptions};
use crate::ui::app::{App, DocView};
use crate::ui::find::FindEvent;
use crate::ui::preview::PaneStatus;

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!()));
        }
    };
}

/// 起動しただけでは、検索欄・プレビュー・巨大ファイルの部品を何も作らない。
pub(super) fn startup_is_light() -> Check {
    step(|app| -> Check {
        check!(app.find_panel.is_none(), "検索欄を起動時に作っている");
        check!(app.preview.is_none(), "プレビューを起動時に作っている");
        check!(app.memo_panel.is_none(), "メモの小窓を起動時に作っている");
        check!(
            app.bigviews.is_empty() && !app.big_timer,
            "巨大ファイルの部品"
        );
        Ok(())
    })?
}

pub(super) fn run(ctx: &Ctx) -> Check {
    keys_and_menu()?;
    find_and_replace()?;
    search_on_worker()?;
    grep_and_open_hit(ctx)?;
    big_view(ctx)?;
    preview(ctx)?;
    preview_tab_switch_stress(ctx)?;
    save_as_memo(ctx)?;
    Ok(())
}

/// 検索・プレビューのキー割り当てとメニュー項目。
fn keys_and_menu() -> Check {
    step(|app| -> Check {
        let key = |s: &str| app.keymap().command_for(&KeyChord::parse(s).unwrap());
        for (chord, command) in [
            ("Ctrl+F", "search.find"),
            ("F3", "search.find_next"),
            ("Shift+F3", "search.find_prev"),
            ("Ctrl+Shift+F", "search.grep"),
            ("Ctrl+R", "search.replace"),
            ("Ctrl+Shift+V", "view.preview_pane"),
            ("Ctrl+Shift+H", "view.preview_window"),
        ] {
            check!(key(chord) == Some(CommandId::new(command)), "{chord}");
        }
        let entries = app.menu().ok_or("メニューがない")?.entries().to_vec();
        for command in [
            "search.find",
            "search.replace",
            "search.find_next",
            "search.find_prev",
            "search.grep",
            "view.preview_pane",
            "view.preview_window",
        ] {
            check!(
                entries.iter().any(|e| e.command == CommandId::new(command)),
                "メニューに {command} がない"
            );
        }
        Ok(())
    })?
}

fn selection(app: &mut App) -> (u64, u64) {
    let r = app.editor().selection().range();
    (r.start, r.end)
}

fn new_doc(text: &str) -> Result<DocumentId, String> {
    cmd("file.new", Value::Null)?;
    let id = step(|app| app.active_id().unwrap())?;
    step(|app| app.with_text(id, |ed| ed.set_text(text)))?.map_err(super::e2s)?;
    Ok(id)
}

fn options(query: &str, case_sensitive: bool) -> SearchOptions {
    SearchOptions {
        query: query.to_owned(),
        case_sensitive,
        ..Default::default()
    }
}

/// 次・前の検索、折り返し、置換、すべて置換（Undo は 1 回）、不正な正規表現。
fn find_and_replace() -> Check {
    let original = "foo bar foo\nFoo baz\nfoo\n";
    let id = new_doc(original)?;
    let o = options("foo", true);
    let go = |dir: Direction, o: &SearchOptions| {
        let o = o.clone();
        step(move |app| {
            app.search_step(o, dir);
            selection(app)
        })
    };
    check!(go(Direction::Forward, &o)? == (0, 3), "最初の一致");
    check!(go(Direction::Forward, &o)? == (8, 11), "次の一致");
    check!(
        go(Direction::Forward, &o)? == (20, 23),
        "大文字小文字を区別する"
    );
    check!(go(Direction::Forward, &o)? == (0, 3), "折り返し");
    check!(
        step(|app| app.status_message().is_some_and(|m| m.contains("折り返")))?,
        "折り返しの案内 {:?}",
        step(|app| app.status_message().map(str::to_owned))?
    );
    check!(go(Direction::Backward, &o)? == (20, 23), "前へ（折り返し）");
    check!(go(Direction::Backward, &o)? == (8, 11), "前の一致");

    // 不正な正規表現は何も変えず、案内を出す。
    let bad = SearchOptions {
        query: "(".into(),
        regex: true,
        ..Default::default()
    };
    let before = go(Direction::Forward, &bad)?;
    check!(before == (8, 11), "不正なパターンで選択が動いた");
    check!(
        step(|app| app.status_message().is_some())?,
        "不正なパターンの案内"
    );

    // 選択が一致なら置換して次へ。
    step(|app| app.replace_one(options("foo", true), "qux"))?;
    check!(
        step(|app| text_of(app, id))? == "foo bar qux\nFoo baz\nfoo\n",
        "1 件の置換 {:?}",
        step(|app| text_of(app, id))?
    );
    check!(step(selection)? == (20, 23), "置換のあと次の一致を選ぶ");

    // すべて置換（大文字小文字を区別しない）。Undo は 1 回で全部戻る。
    step(|app| app.with_text(id, |ed| ed.set_text(original)))?.map_err(super::e2s)?;
    step(|app| app.replace_all(options("foo", false), "x".into()))?;
    check!(
        step(|app| text_of(app, id))? == "x bar x\nx baz\nx\n",
        "すべて置換 {:?}",
        step(|app| text_of(app, id))?
    );
    cmd("edit.undo", Value::Null)?;
    check!(
        step(|app| text_of(app, id))? == original,
        "Undo 1 回で元に戻る {:?}",
        step(|app| text_of(app, id))?
    );

    // 正規表現の置換（$1）。
    let re = SearchOptions {
        query: "(f)(oo)".into(),
        regex: true,
        case_sensitive: true,
        ..Default::default()
    };
    step(|app| app.replace_all(re, "$2$1".into()))?;
    check!(
        step(|app| text_of(app, id))? == "oof bar oof\nFoo baz\noof\n",
        "正規表現の置換"
    );
    step(|app| app.close_document(id, true))?.map_err(super::e2s)?;
    Ok(())
}

/// 大きな文書の検索・置換はワーカーで行い、結果は版を照合してから反映する。
fn search_on_worker() -> Check {
    let original = "alpha beta alpha\ngamma alpha\n";
    let id = new_doc(original)?;
    step(|app| app.worker_search_bytes = 0)?;
    let o = options("alpha", true);
    step({
        let o = o.clone();
        move |app| app.search_step(o, Direction::Forward)
    })?;
    wait("ワーカーの検索結果", |app| {
        selection(app) == (0, 5)
    })?;
    step({
        let o = o.clone();
        move |app| app.search_step(o, Direction::Forward)
    })?;
    wait("次の検索結果", |app| selection(app) == (11, 16))?;

    step({
        let o = o.clone();
        move |app| app.replace_all(o, "Z".into())
    })?;
    wait("ワーカーの置換結果", |app| {
        text_of(app, id) == "Z beta Z\ngamma Z\n"
    })?;
    cmd("edit.undo", Value::Null)?;
    check!(
        step(|app| text_of(app, id))? == original,
        "ワーカーの置換も Undo 1 回で戻る"
    );

    // 置換している間に編集されたら、結果は捨てる（何も置換しない）。
    step({
        let o = o.clone();
        move |app| {
            app.replace_all(o, "Q".into());
            // 結果が届く前に本文を変える。
            app.with_text(id, |ed| ed.set_text("alpha only\n"))
        }
    })?
    .map_err(super::e2s)?;
    std::thread::sleep(std::time::Duration::from_millis(300));
    wait("古い結果の破棄", |app| {
        app.status_message()
            .is_some_and(|m| m.contains("変更されたため"))
    })?;
    check!(
        step(|app| text_of(app, id))? == "alpha only\n",
        "古い結果で置換された"
    );
    step(|app| app.worker_search_bytes = crate::ui::app::search::WORKER_SEARCH_BYTES)?;
    step(|app| app.close_document(id, true))?.map_err(super::e2s)?;
    Ok(())
}

/// Grep の結果を開いて該当行（と桁）へ移る。開いていない文書は開いてから、開いている文書は
/// そのタブへ切り替えて。
fn grep_and_open_hit(ctx: &Ctx) -> Check {
    let root = ctx.dir.join("grep");
    std::fs::create_dir_all(root.join("sub")).map_err(|e| e.to_string())?;
    std::fs::write(root.join("a.txt"), "alpha\nneedle here\nomega\n").map_err(|e| e.to_string())?;
    std::fs::write(root.join("sub").join("b.md"), "# T\nxx 日本語 needle\n")
        .map_err(|e| e.to_string())?;
    let out = grep_collect(
        &GrepOptions::new(&root, SearchOptions::literal("needle")),
        &TaskContext::detached(),
    )
    .map_err(super::e2s)?;
    let (summary, files) = out.value;
    check!(summary.hits == 2, "Grep の件数 {}", summary.hits);
    let hit_in = |name: &str| {
        files
            .iter()
            .flat_map(|f| f.hits.iter())
            .find(|h| h.path.file_name().is_some_and(|n| n == name))
            .cloned()
    };
    let b = hit_in("b.md").ok_or("b.md のヒットがない")?;
    check!(
        b.line == 2 && b.column == 8,
        "桁 {} 行 {}",
        b.column,
        b.line
    );

    // 開いていないファイル: 開いてからジャンプする。
    let (path, line, column) = (b.path.clone(), b.line, b.column);
    step(move |app| app.on_find_event(FindEvent::OpenHit { path, line, column }))?;
    let b_path = b.path.clone();
    wait("ヒットのファイルが開く", move |app| {
        app.active_id()
            .and_then(|id| app.state(id))
            .and_then(|s| s.path())
            .is_some_and(|p| p.ends_with("b.md"))
            && app.editor().selection().caret > 0
    })?;
    let _ = b_path;
    step(|app| -> Check {
        let caret = app.editor().selection().caret;
        let l1 = app.editor().line_start(1).ok_or("行がない")?;
        check!(
            app.editor().line_from_position(caret) == 1,
            "該当行へ移る（0 始まりの行 {}）",
            app.editor().line_from_position(caret)
        );
        // 「xx 日本語 」は 3 + 9 + 1 バイト。
        check!(caret == l1 + 13, "桁へ移る {} / {}", caret, l1 + 13);
        Ok(())
    })??;

    // 開いているファイル: そのタブへ切り替えて移る。
    let a = hit_in("a.txt").ok_or("a.txt のヒットがない")?;
    let a_id = open_and_wait(&a.path)?;
    cmd("file.new", Value::Null)?;
    step(|app| check_active_is_not(app, a_id))??;
    let (path, line, column) = (a.path.clone(), a.line, a.column);
    step(move |app| app.on_find_event(FindEvent::OpenHit { path, line, column }))?;
    step(|app| -> Check {
        check!(app.active_id() == Some(a_id), "開いているタブへ切り替える");
        let caret = app.editor().selection().caret;
        check!(caret == 6, "行頭へ移る {caret}");
        Ok(())
    })??;
    Ok(())
}

fn check_active_is_not(app: &App, id: DocumentId) -> Check {
    check!(app.active_id() != Some(id), "別のタブがアクティブ");
    Ok(())
}

pub(super) fn write_numbered_lines(path: &std::path::Path, n: usize) -> Check {
    let mut text = String::new();
    for i in 1..=n {
        text.push_str(&format!("line {i} some text\n"));
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

pub(super) fn find_big(app: &App, path: &std::path::Path) -> Option<DocumentId> {
    app.documents()
        .iter()
        .find(|d| d.state.path() == Some(path))
        .map(|d| d.id())
}

/// 巨大ファイル（閾値を小さくして強制）: 読み取り専用のビューで開く、行索引、行への移動、
/// 検索、置換と保存の拒否、タブの切り替えでの状態の保持、長大な行での切り替え。
fn big_view(ctx: &Ctx) -> Check {
    step(|app| {
        app.large_thresholds = LargeModeThresholds {
            max_bytes: 16 * 1024,
            max_lines: 1_000_000,
            max_line_bytes: 1 << 20,
        }
    })?;
    let path = ctx.dir.join("big.log");
    write_numbered_lines(&path, 3000)?;
    cmd("file.open", json!({ "path": path.to_string_lossy() }))?;
    let p = path.clone();
    wait("巨大ファイルが開く", move |app| {
        find_big(app, &p).is_some()
    })?;
    let id = step(|app| find_big(app, &path).unwrap())?;
    step(|app| -> Check {
        let d = app.document(id).unwrap();
        check!(
            matches!(d.view(), DocView::External(_)),
            "巨大ファイルは別のビュー"
        );
        check!(
            *d.state.kind() == DocumentKind::LargeFile,
            "文書の種類 {:?}",
            d.state.kind()
        );
        check!(
            !d.state.capabilities().contains(Capability::Edit),
            "編集できてはいけない"
        );
        check!(
            app.bigviews.contains_key(&id) && app.big_timer,
            "ビューとタイマー"
        );
        app.refresh_status();
        let segs = app.status.info().segments();
        check!(
            segs.iter().any(|(_, s)| s == "読み取り専用"),
            "読み取り専用の表示 {segs:?}"
        );
        Ok(())
    })??;
    wait("行索引の作成", |app| {
        app.bigviews
            .get(&id)
            .is_some_and(|b| b.file.index_status().complete)
    })?;
    step(|app| -> Check {
        app.big_tick();
        let segs = app.status.info().segments();
        check!(
            segs.iter().any(|(_, s)| s.contains("索引済み")),
            "索引の表示 {segs:?}"
        );
        Ok(())
    })??;

    // 行へ移動（引数、入力欄）。
    let line_of = |app: &mut App| {
        app.bigviews
            .get(&id)
            .and_then(|b| b.view.status())
            .and_then(|s| s.line)
    };
    cmd("edit.goto_line", json!({ "line": 1500 }))?;
    check!(
        step(line_of)? == Some(1500),
        "行 1500 へ {:?}",
        step(line_of)?
    );
    ctx.prompts
        .input
        .borrow_mut()
        .push_back(Some("2000".into()));
    cmd("edit.goto_line", Value::Null)?;
    check!(step(line_of)? == Some(2000), "入力欄で行 2000 へ");

    // 検索（置換は使えない）。
    step(|app| app.search_step(SearchOptions::literal("line 2500 "), Direction::Forward))?;
    wait("巨大ファイルの検索結果", |app| {
        app.bigviews
            .get(&id)
            .is_some_and(|b| b.view.hit().is_some())
    })?;
    step(|app| -> Check {
        let b = &app.bigviews[&id];
        let hit = b.view.hit().ok_or("ヒットがない")?;
        let text = b.file.read_text(hit, 64).map_err(super::e2s)?;
        check!(text == "line 2500 ", "ヒットの本文 {text:?}");
        Ok(())
    })??;
    step(|app| app.replace_all(options("line", true), "x".into()))?;
    check!(
        step(|app| app
            .status_message()
            .is_some_and(|m| m.contains("読み取り専用")))?,
        "置換は使えない {:?}",
        step(|app| app.status_message().map(str::to_owned))?
    );
    let before = std::fs::read(&path).map_err(|e| e.to_string())?;
    cmd("file.save", Value::Null)?;
    check!(
        std::fs::read(&path).map_err(|e| e.to_string())? == before,
        "巨大ファイルを書き換えた"
    );

    // タブを切り替えて戻っても、表示位置が保たれる。
    let top_before = step(|app| app.bigviews[&id].view.state().top)?;
    let line_before = step(line_of)?;
    cmd("file.new", Value::Null)?;
    check!(step(|app| app.active_id() != Some(id))?, "別のタブへ");
    step(|app| app.activate(id))?.map_err(super::e2s)?;
    check!(
        step(|app| app.bigviews[&id].view.state().top)? == top_before,
        "表示位置の保持"
    );
    check!(step(line_of)? == line_before, "切り替え後の行");

    // 長大な行: 容量は小さくても巨大モードで開く。
    step(|app| {
        app.large_thresholds = LargeModeThresholds {
            max_bytes: 1 << 20,
            max_lines: 1_000_000,
            max_line_bytes: 1024,
        }
    })?;
    let long = ctx.dir.join("long.txt");
    std::fs::write(&long, format!("short\n{}\nend\n", "x".repeat(3000)))
        .map_err(|e| e.to_string())?;
    cmd("file.open", json!({ "path": long.to_string_lossy() }))?;
    let l = long.clone();
    wait("長大な行のファイルが開く", move |app| {
        find_big(app, &l).is_some()
    })?;
    let long_id = step(|app| find_big(app, &long).unwrap())?;
    check!(
        step(|app| app.bigviews.contains_key(&long_id))?,
        "長大な行は巨大モード"
    );
    // 閾値の範囲内の小さなファイルは、これまでどおり通常編集で開く。
    let small = ctx.dir.join("small.txt");
    std::fs::write(&small, "a\nb\n").map_err(|e| e.to_string())?;
    let small_id = open_and_wait(&small)?;
    check!(
        step(|app| matches!(
            app.document(small_id).unwrap().view(),
            DocView::Scintilla(_)
        ))?,
        "小さなファイルは通常編集"
    );

    for close in [small_id, long_id, id] {
        step(move |app| app.close_document(close, true))?.map_err(super::e2s)?;
    }
    check!(
        step(|app| app.bigviews.is_empty() && !app.big_timer)?,
        "巨大ファイルの部品を片づける"
    );
    step(|app| app.large_thresholds = LargeModeThresholds::DEFAULT)?;
    Ok(())
}

/// Markdown プレビュー: 初めて使うまで何も作らない。無題・任意の拡張子でも開け、巨大文書だけが対象外。WebView2 が
/// なければ案内を出し、編集は続けられる（あれば表示できる状態になる）。
fn preview(ctx: &Ctx) -> Check {
    let md = ctx.dir.join("note.md");
    std::fs::write(&md, "# 見出し\n\n本文 *強調*\n").map_err(|e| e.to_string())?;
    let id = open_and_wait(&md)?;
    check!(
        step(|app| app.preview.is_none())?,
        "開く前はプレビューがない"
    );

    // 無題の文書・Markdown でない拡張子のファイルでも開ける（本文を Markdown として変換する）。
    let txt = new_doc("plain\n")?;
    check!(
        step(|app| app.preview_available())?,
        "無題の文書はプレビューできる"
    );
    cmd("view.preview_pane", Value::Null)?;
    check!(
        step(move |app| app.preview_pane_open()
            && app.preview.as_ref().and_then(|p| p.target) == Some(txt))?,
        "無題の文書で右ペインが開く"
    );
    cmd("view.preview_pane", Value::Null)?;
    check!(step(|app| !app.preview_pane_open())?, "閉じる");
    step(move |app| app.close_document(txt, true))?.map_err(super::e2s)?;
    let plain = ctx.dir.join("plain.txt");
    std::fs::write(&plain, "# text\n").map_err(|e| e.to_string())?;
    let plain_id = open_and_wait(&plain)?;
    cmd("view.preview_window", Value::Null)?;
    check!(
        step(move |app| app.preview_window_open()
            && app.preview.as_ref().and_then(|p| p.target) == Some(plain_id))?,
        ".txt のファイルで別ウィンドウが開く"
    );
    // タブの切り替えに追従する。
    step(move |app| app.activate(id))?.map_err(super::e2s)?;
    check!(
        step(move |app| app.preview.as_ref().and_then(|p| p.target) == Some(id))?,
        "切り替えに追従する"
    );
    // Markdown でないファイルに切り替えたら、プレビュー（右ペインも別ウィンドウも）を閉じる。
    step(move |app| app.activate(plain_id))?.map_err(super::e2s)?;
    check!(
        step(|app| !app.preview_window_open()
            && !app.preview_pane_open()
            && app.right.is_none()
            && app.preview.as_ref().and_then(|p| p.target).is_none())?,
        "Markdown でない文書に替えるとプレビューが閉じる"
    );
    // 巨大文書: プレビュー対象外。Markdown で開いたプレビューは、巨大文書に替えると閉じる。
    step(|app| {
        app.large_thresholds = LargeModeThresholds {
            max_bytes: 16,
            max_lines: 1_000_000,
            max_line_bytes: 1 << 20,
        }
    })?;
    let big = ctx.dir.join("big-preview.txt");
    std::fs::write(&big, "line\n".repeat(100)).map_err(|e| e.to_string())?;
    cmd("file.open", json!({ "path": big.to_string_lossy() }))?;
    let b = big.clone();
    wait("巨大文書が開く", move |app| {
        find_big(app, &b).is_some()
    })?;
    let big_id = step(|app| find_big(app, &big).unwrap())?;
    check!(
        step(move |app| app.bigviews.contains_key(&big_id) && app.active_id() == Some(big_id))?,
        "巨大文書がアクティブ"
    );
    check!(
        step(|app| !app.preview_available())?,
        "巨大文書はプレビュー対象外"
    );
    step(move |app| app.activate(id))?.map_err(super::e2s)?;
    cmd("view.preview_pane", Value::Null)?;
    check!(step(|app| app.preview_pane_open())?, "右ペインが開く");
    step(move |app| app.activate(big_id))?.map_err(super::e2s)?;
    check!(
        step(|app| !app.preview_pane_open() && app.right.is_none())?,
        "巨大文書に替えると右ペインが閉じる"
    );
    // 巨大文書では新しく開けない。
    step(|app| app.toggle_preview_pane())?.map_err(super::e2s)?;
    check!(
        step(|app| !app.preview_pane_open())?,
        "巨大文書では開かない"
    );
    step(|app| app.large_thresholds = LargeModeThresholds::DEFAULT)?;
    // 対象の文書を閉じたらプレビューも閉じる。
    step(move |app| app.activate(plain_id))?.map_err(super::e2s)?;
    cmd("view.preview_window", Value::Null)?;
    check!(step(|app| app.preview_window_open())?, "別ウィンドウが開く");
    step(move |app| app.close_document(big_id, true))?.map_err(super::e2s)?;
    step(move |app| app.close_document(plain_id, true))?.map_err(super::e2s)?;
    // 隣が Markdown なら追従して残り、そうでなければ閉じる。閉じた文書は表示しない。
    check!(
        step(move |app| {
            let t = app.preview.as_ref().and_then(|p| p.target);
            t != Some(plain_id)
                && (app.preview_window_open() == t.is_some())
                && (t.is_none() || t.is_some_and(|d| app.document(d).is_some()))
        })?,
        "対象の文書を閉じても、閉じた文書の表示は残らない"
    );
    if step(|app| app.preview_window_open())? {
        cmd("view.preview_window", Value::Null)?;
    }

    step(move |app| app.activate(id))?.map_err(super::e2s)?;
    cmd("view.preview_pane", Value::Null)?;
    check!(step(|app| app.preview_pane_open())?, "右ペインが開く");
    wait("プレビューの初期化の結果", |app| {
        app.preview
            .as_ref()
            .and_then(|p| p.pane.as_ref())
            .is_some_and(|p| p.status() != PaneStatus::Initializing)
    })?;
    let unavailable = step(|app| {
        matches!(
            app.preview
                .as_ref()
                .and_then(|p| p.pane.as_ref())
                .map(|p| p.status()),
            Some(PaneStatus::Unavailable(_))
        )
    })?;
    if unavailable {
        // ランタイムがない: 案内が出て、編集は続けられる。
        wait("ランタイムの案内", |app| {
            app.status_message().is_some_and(|m| m.contains("WebView2"))
        })?;
    }
    step(|app| app.with_text(id, |ed| ed.set_text("# 追記\n")))?.map_err(super::e2s)?;
    check!(
        step(|app| text_of(app, id))? == "# 追記\n",
        "編集は続けられる"
    );
    check!(
        step(|app| app.state(id).unwrap().is_dirty())?,
        "編集で未保存"
    );
    // 閉じる。
    cmd("view.preview_pane", Value::Null)?;
    check!(
        step(|app| !app.preview_pane_open() && app.right.is_none())?,
        "右ペインが閉じる"
    );
    // 別ウィンドウ。
    cmd("view.preview_window", Value::Null)?;
    check!(step(|app| app.preview_window_open())?, "別ウィンドウが開く");
    cmd("view.preview_window", Value::Null)?;
    check!(
        step(|app| !app.preview_window_open())?,
        "別ウィンドウが閉じる"
    );
    step(move |app| app.close_document(id, true))?.map_err(super::e2s)?;
    Ok(())
}

/// プレビューを出したまま、種類の違う文書（Markdown・小さい文字列・大きい文字列・上限超え・
/// 病的な入れ子）の間でタブを切り替え続けても、画面が固まらず、表示が対象に追いつく。
pub(super) fn preview_tab_switch_stress(ctx: &Ctx) -> Check {
    let write = |name: &str, body: String| -> Result<std::path::PathBuf, String> {
        let p = ctx.dir.join(name);
        std::fs::write(&p, body).map_err(|e| e.to_string())?;
        Ok(p)
    };
    let md = write("s-note.md", "# 見出し\n\n本文\n".repeat(50))?;
    let small = write("s-small.txt", "plain text\nline2\n".to_owned())?;
    let log = write(
        "s-log.log",
        (0..40_000)
            .map(|i| {
                format!(
                    "2026-11-16 12:00:{:02} INFO line {i} <b>&amp;</b>\n",
                    i % 60
                )
            })
            .collect::<String>(),
    )?;
    let long_line = write("s-oneline.txt", "x".repeat(1_500_000))?;
    let nested = write("s-nested.txt", format!("{}a\n", "> ".repeat(20_000)))?;
    let list = write("s-list.txt", "- a\n".repeat(200_000))?;
    let files = [md, small, log, long_line, nested, list];
    let mut ids = Vec::new();
    for f in &files {
        ids.push(open_and_wait(f)?);
    }
    // メモ（小さい・上限付近・上限超え）も同じ切り替えの対象にする。
    let store = crate::memo::MemoStore::new(&ctx.paths);
    for (name, body) in [
        ("stress small", "メモ本文\n".repeat(10)),
        ("stress mid", "- a\n".repeat(120_000)),
        ("stress big", "- a\n".repeat(200_000)),
    ] {
        let memo = store.create(name, &body).map_err(|e| e.to_string())?;
        ids.push(step(|app| app.open_memo(memo.clone()))?.map_err(super::e2s)?);
    }
    // 保存していない無題の文書（小さい・上限超え）も対象にする。
    for body in ["無題の本文\n".repeat(5), "- a\n".repeat(200_000)] {
        let id = step(|app| app.new_untitled())?.map_err(super::e2s)?;
        step(move |app| app.with_text(id, |ed| ed.set_text(&body)))?.map_err(super::e2s)?;
        ids.push(id);
    }
    // 最初は .txt（Markdown でない）でプレビューを開く。
    let first = ids[1];
    step(move |app| app.activate(first))?.map_err(super::e2s)?;
    cmd("view.preview_pane", Value::Null)?;
    wait("プレビューの初期化の結果", |app| {
        app.preview
            .as_ref()
            .and_then(|p| p.pane.as_ref())
            .is_some_and(|p| p.status() != PaneStatus::Initializing)
    })?;
    let unavailable = step(|app| {
        matches!(
            app.preview
                .as_ref()
                .and_then(|p| p.pane.as_ref())
                .map(|p| p.status()),
            Some(PaneStatus::Unavailable(_))
        )
    })?;
    if unavailable {
        return Ok(());
    }
    let md_id = ids[0];
    let shown_wait = |what: String, id: DocumentId| {
        wait(&what, move |app| {
            app.preview.as_ref().is_some_and(|p| {
                !p.scheduler.is_running() && p.scheduler.shown().is_some_and(|s| s.document == id)
            }) || app
                .status_message()
                .is_some_and(|m| m.contains("プレビューを作れませんでした"))
        })
    };
    for round in 0..3 {
        for (n, &heavy) in ids.iter().enumerate().skip(1) {
            // 重い文書で開く（直前に閉じているので、開く操作の対象はアクティブな文書）。
            step(move |app| app.activate(heavy))?.map_err(super::e2s)?;
            if !step(|app| app.preview_pane_open())? {
                cmd("view.preview_pane", Value::Null)?;
            }
            shown_wait(format!("{round} 周目 {n}: 開いた文書のプレビュー"), heavy)?;
            // Markdown へ切り替える（追従する）。
            step(move |app| app.activate(md_id))?.map_err(super::e2s)?;
            shown_wait(format!("{round} 周目 {n}: 切り替え先のプレビュー"), md_id)?;
            // 軽い応答の確認。
            check!(step(|app| app.preview_pane_open())?, "右ペインが開いている");
            // もう一度重い文書へ（追従しないのでプレビューが閉じる）。
            step(move |app| app.activate(heavy))?.map_err(super::e2s)?;
            let follows = step(move |app| {
                app.document(heavy)
                    .is_some_and(crate::ui::app::preview::follows_on_switch)
            })?;
            check!(
                step(|app| app.preview_pane_open())? == follows,
                "追従しない文書に替えると右ペインが閉じる（追従する文書なら開いたまま）"
            );
        }
    }
    if step(|app| app.preview_pane_open())? {
        cmd("view.preview_pane", Value::Null)?;
    }
    // 別ウィンドウ: 開く → 文書を替える → 閉じるボタンで戻る／右ペインに替える。
    for round in 0..2 {
        for (n, &doc) in ids.iter().enumerate() {
            step(move |app| app.activate(doc))?.map_err(super::e2s)?;
            cmd("view.preview_window", Value::Null)?;
            check!(step(|app| app.preview_window_open())?, "別ウィンドウが開く");
            shown_wait(format!("窓 {round} 周目 {n}: 開いた文書"), doc)?;
            step(move |app| app.activate(md_id))?.map_err(super::e2s)?;
            if n % 2 == 0 {
                // 表示の更新が終わる前に閉じるボタン（WM_CLOSE）で閉じる。
                let hwnd = step(|app| {
                    app.preview
                        .as_ref()
                        .and_then(|p| p.window.as_ref())
                        .map(|w| w.hwnd().0 as isize)
                })?
                .ok_or("別ウィンドウがない")?;
                // SAFETY: UI スレッドで、自分のウィンドウへ WM_CLOSE を送る。
                unsafe {
                    windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                        windows::Win32::Foundation::HWND(hwnd as *mut _),
                        windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                        None,
                        None,
                    );
                }
                wait(
                    "閉じるボタンで別ウィンドウが閉じる",
                    |app| !app.preview_window_open(),
                )?;
            } else {
                // 別ウィンドウから右ペインへ。
                cmd("view.preview_pane", Value::Null)?;
                check!(
                    step(|app| app.preview_pane_open() && !app.preview_window_open())?,
                    "右ペインに替わる"
                );
                shown_wait(format!("窓→ペイン {round} 周目 {n}"), md_id)?;
                cmd("view.preview_pane", Value::Null)?;
            }
        }
    }
    for id in ids {
        step(move |app| app.close_document(id, true))?.map_err(super::e2s)?;
    }
    Ok(())
}

/// 「メモとして保存」: 無題は同じタブがメモになる（名前は最初の一文）、ファイルはメモに複製、
/// 設定で `Ctrl+S` もメモ、閉じる確認の「メモに保存」。
pub(super) fn save_as_memo(ctx: &Ctx) -> Check {
    use crate::core::MemoId;
    use crate::ui::dialogs::CloseChoice;
    let store = crate::memo::MemoStore::new(&ctx.paths);
    let find_by_name = |name: &str| -> Option<MemoId> {
        let listing = store
            .list(&crate::core::TaskContext::detached())
            .ok()?
            .value;
        listing
            .memos
            .into_iter()
            .find(|m| m.name == name)
            .map(|m| m.id)
    };

    // 1. 無題の文書: 同じタブがメモになる。
    let id = new_doc("# 買い物リスト\n牛乳と卵。\n")?;
    cmd("file.save_as_memo", Value::Null)?;
    check!(
        step(move |app| matches!(
            app.document(id).map(|d| d.state.kind().clone()),
            Some(DocumentKind::Memo(_))
        ))?,
        "無題のタブがメモになる"
    );
    check!(
        step(move |app| app.document(id).map(|d| d.title.clone()))? == Some("買い物リスト".into()),
        "名前は最初の一文（見出しの記号は除く）"
    );
    check!(
        step(move |app| !app.state(id).unwrap().is_dirty())?,
        "メモにした直後は未保存でない"
    );
    check!(
        step(move |app| app.tabs.items().iter().any(|t| t.id == id && t.memo))?,
        "メモのタブにしるしが付く"
    );
    let mid = find_by_name("買い物リスト").ok_or("メモが保存領域にない")?;
    check!(
        store.load(&mid).map_err(|e| e.to_string())?.body == "# 買い物リスト\n牛乳と卵。\n",
        "本文がそのまま保存される"
    );
    // 以降は自動保存。
    step(move |app| app.with_text(id, |ed| ed.set_text("# 買い物リスト\n追記\n")))?
        .map_err(super::e2s)?;
    let s = store.clone();
    let m2 = mid.clone();
    wait("メモの自動保存", move |_| {
        s.load(&m2).is_ok_and(|m| m.body.contains("追記"))
    })?;
    step(move |app| app.close_document(id, true))?.map_err(super::e2s)?;

    // 2. 名前を指定する。空の本文は既定の名前。
    let id = new_doc("")?;
    cmd("file.save_as_memo", Value::Null)?;
    check!(
        step(move |app| app.document(id).map(|d| d.title.clone()))? == Some("無題のメモ".into()),
        "空の本文は既定の名前"
    );
    step(move |app| app.close_document(id, true))?.map_err(super::e2s)?;
    let id = new_doc("本文")?;
    cmd("file.save_as_memo", json!({ "name": "指定した名前" }))?;
    check!(find_by_name("指定した名前").is_some(), "指定した名前で保存");
    step(move |app| app.close_document(id, true))?.map_err(super::e2s)?;

    // 3. ファイルを開いているタブ: メモに複製し、元のタブは変えない。
    let path = ctx.dir.join("to-memo.txt");
    std::fs::write(&path, "ファイルの中身\n").map_err(|e| e.to_string())?;
    let fid = open_and_wait(&path)?;
    cmd("file.save_as_memo", Value::Null)?;
    check!(
        step(move |app| matches!(
            app.document(fid).map(|d| d.state.kind().clone()),
            Some(DocumentKind::File)
        ))?,
        "ファイルのタブは変わらない"
    );
    let copied = find_by_name("to-memo.txt").ok_or("複製のメモがない")?;
    check!(
        store.load(&copied).map_err(|e| e.to_string())?.body == "ファイルの中身\n",
        "複製の本文"
    );
    check!(
        step(move |app| app.active_id() != Some(fid))?,
        "複製は新しいタブで開く"
    );
    step(move |app| app.close_document(fid, true))?.map_err(super::e2s)?;

    ctx.prompts.shown.borrow_mut().clear();
    // 4. 設定: 無題の Ctrl+S がメモになる（保存先は尋ねない）。
    step(|app| app.settings.save.untitled_to_memo = true)?;
    let id = new_doc("設定でメモ。次の文")?;
    cmd("file.save", Value::Null)?;
    check!(
        step(move |app| matches!(
            app.document(id).map(|d| d.state.kind().clone()),
            Some(DocumentKind::Memo(_))
        ))?,
        "設定で無題の保存がメモになる"
    );
    check!(
        find_by_name("設定でメモ").is_some(),
        "最初の一文が名前になる"
    );
    check!(
        !ctx.prompts
            .shown
            .borrow()
            .iter()
            .any(|s| s.starts_with("save_file")),
        "保存先を尋ねていない"
    );
    // 閉じる確認の「保存する」もメモ。
    let id2 = new_doc("閉じるときの本文\n")?;
    ctx.prompts.close.borrow_mut().push_back(CloseChoice::Save);
    step(move |app| app.close_document(id2, false))?.map_err(super::e2s)?;
    check!(
        find_by_name("閉じるときの本文").is_some(),
        "閉じる確認の保存もメモ"
    );
    check!(step(move |app| app.document(id2).is_none())?, "閉じた");
    step(|app| app.settings.save.untitled_to_memo = false)?;
    step(move |app| app.close_document(id, true))?.map_err(super::e2s)?;

    // 5. 設定なし: 閉じる確認の「メモに保存」で、保存先を尋ねずに残して閉じる。
    let id = new_doc("退避する内容\n")?;
    ctx.prompts
        .close
        .borrow_mut()
        .push_back(CloseChoice::SaveAsMemo);
    step(move |app| app.close_document(id, false))?.map_err(super::e2s)?;
    check!(step(move |app| app.document(id).is_none())?, "閉じる");
    check!(find_by_name("退避する内容").is_some(), "メモに残る");
    Ok(())
}
