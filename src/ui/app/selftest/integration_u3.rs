//! 改善 U3 の自己診断: 検索の全一致の強調（件数、現在の一致、編集・Esc・タブ切り替えで消える、
//! ワーカー、上限）、検索欄が閉じていても効く F3／Shift+F3／Ctrl+F3、巨大モードの F3、
//! 選択範囲内の置換、編集コマンド（行の複製・削除・移動、大文字小文字、行末の空白、
//! タブ⇔スペース、日時、対応する括弧）。編集はどれも「元に戻す」1 回で戻ること。
//! 色や見た目は Windows 実機で確認する（`docs/verification/U.md`）。

use serde_json::json;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, MSG, WM_KEYDOWN};

use super::integration::{cmd, step, wait};
use super::integration_i2::{find_big, write_numbered_lines};
use super::{text_of, Check, Ctx};
use crate::bigfile::LargeModeThresholds;
use crate::command::{CommandId, Invocation, KeyChord, Value};
use crate::core::DocumentId;
use crate::document::Selection;
use crate::search::SearchOptions;
use crate::ui::app::App;
use crate::ui::find::FindEvent;

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!()));
        }
    };
}

const SCI_INDICATORVALUEAT: u32 = 2507;
const INDIC_ALL: usize = 9;
const INDIC_CURRENT: usize = 10;

fn indicator(app: &App, indic: usize, pos: usize) -> isize {
    // SAFETY: 自分の編集欄への同期メッセージ（ポインタを渡さない読み出し）。
    unsafe {
        SendMessageW(
            app.editor().hwnd(),
            SCI_INDICATORVALUEAT,
            Some(WPARAM(indic)),
            Some(LPARAM(pos as isize)),
        )
        .0
    }
}

fn options(query: &str, case_sensitive: bool) -> SearchOptions {
    SearchOptions {
        query: query.to_owned(),
        case_sensitive,
        ..Default::default()
    }
}

fn new_doc(text: &str) -> Result<DocumentId, String> {
    cmd("file.new", Value::Null)?;
    let id = step(|app| app.active_id().unwrap())?;
    step(|app| {
        app.with_text(id, |ed| {
            ed.set_eol(crate::text::Eol::Lf);
            ed.set_text(text);
            ed.empty_undo_buffer();
        })
    })?
    .map_err(super::e2s)?;
    Ok(id)
}

fn select(id: DocumentId, anchor: u64, caret: u64) -> Check {
    step(|app| app.with_text(id, |ed| ed.set_selection(Selection { anchor, caret })))?
        .map_err(super::e2s)
}

fn selection(app: &mut App) -> (u64, u64) {
    let r = app.editor().selection().range();
    (r.start, r.end)
}

fn text(id: DocumentId) -> Result<String, String> {
    step(|app| text_of(app, id))
}

fn status(app: &App) -> String {
    app.status_message().map(str::to_owned).unwrap_or_default()
}

/// 検索の前の状態を空にする（検索欄が前の試験の条件を持っていても使わない）。
fn reset_search() -> Check {
    step(|app| {
        if let Some(p) = app.find_panel {
            p.set_query("");
        }
        app.last_search = None;
        app.clear_highlight_and_count();
    })
}

fn close(id: DocumentId) -> Check {
    step(|app| app.close_document(id, true))?
        .map(|_| ())
        .map_err(super::e2s)
}

pub(super) fn run(ctx: &Ctx) -> Check {
    keys_and_menu()?;
    highlight()?;
    highlight_worker_and_cap()?;
    f3_without_panel()?;
    big_mode_f3(ctx)?;
    replace_in_selection()?;
    scope_search()?;
    search_history()?;
    line_commands()?;
    text_commands()?;
    brace()?;
    Ok(())
}

fn keys_and_menu() -> Check {
    step(|app| -> Check {
        let key = |s: &str| app.keymap().command_for(&KeyChord::parse(s).unwrap());
        for (chord, command) in [
            ("Ctrl+D", "edit.duplicate_line"),
            ("Ctrl+Shift+K", "edit.delete_line"),
            ("Alt+Up", "edit.move_line_up"),
            ("Alt+Down", "edit.move_line_down"),
            ("Ctrl+]", "edit.goto_matching_brace"),
            ("Ctrl+F3", "search.find_selection"),
            ("F3", "search.find_next"),
            ("Shift+F3", "search.find_prev"),
        ] {
            check!(key(chord) == Some(CommandId::new(command)), "{chord}");
        }
        let entries = app.menu().ok_or("メニューがない")?.entries().to_vec();
        for command in [
            "edit.duplicate_line",
            "edit.delete_line",
            "edit.move_line_up",
            "edit.move_line_down",
            "edit.uppercase",
            "edit.lowercase",
            "edit.trim_trailing_whitespace",
            "edit.tabs_to_spaces",
            "edit.spaces_to_tabs",
            "edit.insert_datetime",
            "edit.goto_matching_brace",
            "search.find_selection",
        ] {
            check!(
                entries.iter().any(|e| e.command == CommandId::new(command)),
                "メニューに {command} がない"
            );
        }
        Ok(())
    })?
}

/// 全一致の強調、件数、現在の一致、編集・Esc・タブ切り替えで消える。
fn highlight() -> Check {
    reset_search()?;
    let id = new_doc("foo bar foo\nFoo baz\nfoo\n")?;
    step(|app| app.on_find_event(FindEvent::Changed(options("foo", false))))?;
    check!(
        step(|app| app.highlight_count())? == 4,
        "大文字小文字を区別しない: 4 件 {}",
        step(|app| app.highlight_count())?
    );
    step(|app| -> Check {
        for (pos, want) in [(0, 1), (2, 1), (3, 0), (8, 1), (12, 1), (20, 1), (23, 0)] {
            check!(
                indicator(app, INDIC_ALL, pos) == want,
                "強調の位置 {pos}: {}",
                indicator(app, INDIC_ALL, pos)
            );
        }
        Ok(())
    })??;
    // 区別する: 3 件。
    step(|app| app.on_find_event(FindEvent::Changed(options("foo", true))))?;
    check!(step(|app| app.highlight_count())? == 3, "区別する: 3 件");
    check!(
        step(|app| indicator(app, INDIC_ALL, 12))? == 0,
        "Foo の強調が消える"
    );
    // 無効なパターンと空は強調を消す。
    step(|app| {
        app.on_find_event(FindEvent::Changed(SearchOptions {
            query: "(".into(),
            regex: true,
            ..Default::default()
        }))
    })?;
    check!(step(|app| app.highlight_count())? == 0, "無効なパターン");
    step(|app| app.on_find_event(FindEvent::Changed(options("foo", true))))?;

    // 次の一致: 「n / 全 m 件」と現在の一致の印。
    let o = options("foo", true);
    step({
        let o = o.clone();
        move |app| app.search_step(o, crate::search::Direction::Forward)
    })?;
    check!(step(selection)? == (0, 3), "最初の一致");
    check!(
        step(|app| status(app).contains("1 / 全 3 件"))?,
        "件数 {:?}",
        step(|app| status(app))?
    );
    check!(
        step(|app| indicator(app, INDIC_CURRENT, 0))? == 1
            && step(|app| indicator(app, INDIC_CURRENT, 8))? == 0,
        "現在の一致の印"
    );
    step({
        let o = o.clone();
        move |app| app.search_step(o, crate::search::Direction::Forward)
    })?;
    check!(step(selection)? == (8, 11), "次の一致");
    check!(
        step(|app| status(app).contains("2 / 全 3 件"))?,
        "件数 {:?}",
        step(|app| status(app))?
    );
    check!(
        step(|app| indicator(app, INDIC_CURRENT, 0))? == 0
            && step(|app| indicator(app, INDIC_CURRENT, 8))? == 1,
        "現在の一致の印の付け替え"
    );

    // 文書の編集で消える。
    step(|app| app.with_text(id, |ed| ed.replace_selection_text("Z")))?.map_err(super::e2s)?;
    check!(step(|app| app.highlight_count())? == 0, "編集で消える");
    check!(
        step(|app| indicator(app, INDIC_ALL, 0))? == 0,
        "編集で印が消える"
    );

    // Esc で消える（消えるものがなければ Esc を横取りしない）。
    step(move |app| app.search_step(o, crate::search::Direction::Forward))?;
    check!(
        step(|app| app.highlight_count())? == 2,
        "再び強調 {}",
        step(|app| app.highlight_count())?
    );
    let esc = |app: &mut App| {
        let msg = MSG {
            hwnd: app.editor().hwnd(),
            message: WM_KEYDOWN,
            wParam: WPARAM(0x1B),
            ..Default::default()
        };
        app.translate_shortcut(&msg)
    };
    check!(step(esc)?, "Esc を処理する");
    check!(step(|app| app.highlight_count())? == 0, "Esc で消える");
    check!(!step(esc)?, "強調がなければ Esc を処理しない");

    // 別のタブへ切り替えると消える（元の文書の印も残らない）。
    step(|app| app.on_find_event(FindEvent::Changed(options("foo", true))))?;
    check!(
        step(|app| app.highlight_count())? == 2,
        "強調 {}",
        step(|app| app.highlight_count())?
    );
    cmd("file.new", Value::Null)?;
    check!(
        step(|app| app.highlight_count())? == 0,
        "タブ切り替えで消える"
    );
    step(|app| app.activate(id))?.map_err(super::e2s)?;
    check!(
        step(|app| indicator(app, INDIC_ALL, 0))? == 0,
        "元の文書に印が残らない"
    );
    close(id)
}

/// 大きい文書のワーカーでの強調と、件数の上限。
fn highlight_worker_and_cap() -> Check {
    reset_search()?;
    let id = new_doc("alpha beta alpha\ngamma alpha\n")?;
    step(|app| app.worker_search_bytes = 0)?;
    step(|app| app.on_find_event(FindEvent::Changed(options("alpha", true))))?;
    wait("ワーカーの強調", |app| app.highlight_count() == 3)?;
    check!(
        step(|app| status(app).contains("全 3 件"))?,
        "件数 {:?}",
        step(|app| status(app))?
    );
    check!(
        step(|app| indicator(app, INDIC_ALL, 12))? == 1,
        "強調の位置"
    );
    // 検索している間の編集は、古い結果を捨てる。
    step(|app| app.on_find_event(FindEvent::Changed(options("beta", true))))?;
    step(|app| app.with_text(id, |ed| ed.replace_selection_text("")))?.map_err(super::e2s)?;
    step(|_| ())?;
    check!(step(|app| app.highlight_count())? == 0, "編集で消える");
    step(|app| app.worker_search_bytes = super::super::search::WORKER_SEARCH_BYTES)?;
    close(id)?;

    // 上限: 20000 件で打ち切り、「+」を付ける。
    reset_search()?;
    let id = new_doc(&"a ".repeat(20_005))?;
    step(|app| app.on_find_event(FindEvent::Changed(options("a", true))))?;
    check!(
        step(|app| app.highlight_count())? == 20_000,
        "上限 {}",
        step(|app| app.highlight_count())?
    );
    check!(
        step(|app| status(app).contains("20000+"))?,
        "上限の案内 {:?}",
        step(|app| status(app))?
    );
    step(|app| app.clear_highlight_and_count())?;
    close(id)
}

/// 検索欄が閉じていても F3／Shift+F3／Ctrl+F3 が効く。
fn f3_without_panel() -> Check {
    reset_search()?;
    let id = new_doc("alpha beta alpha\ngamma alpha\n")?;
    // 条件がなければ、カーソル位置の単語で検索する。
    select(id, 7, 7)?;
    cmd("search.find_next", Value::Null)?;
    check!(
        step(selection)? == (6, 10),
        "カーソル位置の単語 {:?}",
        step(selection)?
    );
    check!(
        step(|app| app.last_search.as_ref().map(|o| o.query.clone()))? == Some("beta".into()),
        "検索語"
    );
    check!(
        step(|app| app.find_panel.is_none_or(|p| !p.is_visible()))?,
        "検索欄は開かない"
    );
    // 条件がなければ、選択中の文字列で検索する（Shift+F3 は前へ）。
    reset_search()?;
    select(id, 0, 5)?;
    cmd("search.find_next", Value::Null)?;
    check!(
        step(selection)? == (11, 16),
        "選択文字列で次へ {:?}",
        step(selection)?
    );
    check!(step(|app| app.highlight_count())? == 3, "全一致を強調");
    check!(
        step(|app| status(app).contains("2 / 全 3 件"))?,
        "件数 {:?}",
        step(|app| status(app))?
    );
    cmd("search.find_prev", Value::Null)?;
    check!(step(selection)? == (0, 5), "前へ {:?}", step(selection)?);
    // 以後は直前の条件で動く。
    cmd("search.find_next", Value::Null)?;
    cmd("search.find_next", Value::Null)?;
    check!(
        step(selection)? == (23, 28),
        "直前の条件で次へ {:?}",
        step(selection)?
    );
    // Ctrl+F3: 選択文字列を検索語にして次へ（直前の条件を置き換える）。
    select(id, 17, 22)?;
    cmd("search.find_selection", Value::Null)?;
    check!(
        step(|app| app.last_search.as_ref().map(|o| o.query.clone()))? == Some("gamma".into()),
        "Ctrl+F3 の検索語"
    );
    check!(
        step(selection)? == (17, 22),
        "Ctrl+F3 {:?}",
        step(selection)?
    );
    cmd("search.find_next", Value::Null)?;
    check!(step(selection)? == (17, 22), "検索語が置き換わっている");
    step(|app| app.clear_highlight_and_count())?;
    reset_search()?;
    close(id)
}

/// 巨大モードでも、検索欄なしの F3 で直前の条件を探す。強調は付けない。
fn big_mode_f3(ctx: &Ctx) -> Check {
    reset_search()?;
    step(|app| {
        app.large_thresholds = LargeModeThresholds {
            max_bytes: 16 * 1024,
            max_lines: 1_000_000,
            max_line_bytes: 1 << 20,
        }
    })?;
    let path = ctx.dir.join("u3big.log");
    write_numbered_lines(&path, 3000)?;
    cmd("file.open", json!({ "path": path.to_string_lossy() }))?;
    let p = path.clone();
    wait("巨大ファイルが開く", move |app| {
        find_big(app, &p).is_some()
    })?;
    let id = step(|app| find_big(app, &path).unwrap())?;
    step(|app| app.last_search = Some(SearchOptions::literal("line 100 ")))?;
    cmd("search.find_next", Value::Null)?;
    wait("巨大ファイルの F3", |app| {
        app.bigviews
            .get(&id)
            .is_some_and(|b| b.view.hit().is_some())
    })?;
    check!(
        step(|app| app.highlight_count())? == 0,
        "巨大モードは強調しない"
    );
    cmd("search.find_prev", Value::Null)?;
    check!(
        step(|app| app.bigviews.contains_key(&id))?,
        "巨大モードのまま"
    );
    // 巨大モードで編集コマンドは使えない（読み取り専用）。
    check!(
        step(|app| {
            let registry = app.registry();
            registry
                .check_available(app, &Invocation::user(CommandId::new("edit.delete_line")))
                .is_err()
        })?,
        "巨大ファイルで行の削除は無効"
    );
    close(id)
}

fn scope_of_doc(id: DocumentId) -> Result<Option<(u64, u64)>, String> {
    step(|app| app.with_text(id, |ed| ed.search_scope().map(|r| (r.start, r.end))))?
        .map_err(super::e2s)
}

fn replace_in_selection() -> Check {
    reset_search()?;
    let original = "foo foo\nfoo foo\n";
    let id = new_doc(original)?;
    select(id, 0, 7)?;
    step(|app| app.set_scope(true))?;
    check!(
        scope_of_doc(id)? == Some((0, 7)),
        "範囲の印 {:?}",
        scope_of_doc(id)?
    );
    step(|app| {
        app.on_find_event(FindEvent::ReplaceAll {
            options: options("foo", true),
            replacement: "XY".into(),
        })
    })?;
    check!(
        text(id)? == "XY XY\nfoo foo\n",
        "選択範囲だけ置換 {:?}",
        text(id)?
    );
    check!(
        step(selection)? == (0, 5),
        "置換後の範囲を選ぶ {:?}",
        step(selection)?
    );
    check!(
        scope_of_doc(id)? == Some((0, 5)),
        "置換後の範囲の印 {:?}",
        scope_of_doc(id)?
    );
    cmd("edit.undo", Value::Null)?;
    check!(text(id)? == original, "Undo 1 回で戻る {:?}", text(id)?);
    step(|app| app.set_scope(false))?;
    check!(scope_of_doc(id)?.is_none(), "解除で印が消える");
    // 選択がなければオンにできない（何も置換しない）。
    select(id, 3, 3)?;
    step(|app| app.set_scope(true))?;
    check!(
        step(|app| app.scope_doc.is_none())?,
        "選択なしでは範囲にならない"
    );
    check!(
        step(|app| status(app).contains("選択範囲がありません"))?,
        "案内 {:?}",
        step(|app| status(app))?
    );
    // 正規表現（$1）とワーカー。
    step(|app| app.worker_search_bytes = 0)?;
    select(id, 8, 15)?;
    step(|app| app.set_scope(true))?;
    step(|app| {
        app.replace_all(
            SearchOptions {
                query: "(f)(oo)".into(),
                regex: true,
                case_sensitive: true,
                ..Default::default()
            },
            "$2$1".into(),
        )
    })?;
    wait("選択範囲内の置換（ワーカー）", |app| {
        text_of(app, id) == "foo foo\noof oof\n"
    })?;
    step(|app| app.worker_search_bytes = super::super::search::WORKER_SEARCH_BYTES)?;
    step(|app| app.set_scope(false))?;
    close(id)
}

/// 選択範囲内の検索（次へ・前へ・強調・件数）と、編集で範囲がずれても追従すること、
/// 別のタブへ切り替えると解除されること、文書 0 件でも安全なこと。
fn scope_search() -> Check {
    reset_search()?;
    let id = new_doc("foo\nfoo foo\nfoo\n")?;
    step(|app| {
        let _ = app.show_find(false);
        if let Some(p) = app.find_panel {
            p.set_query("foo");
        }
    })?;
    select(id, 4, 11)?;
    step(|app| app.set_scope(true))?;
    wait("範囲内だけ強調", |app| app.highlight_count() == 2)?;
    select(id, 0, 0)?;
    let o = options("foo", true);
    for (dir, want) in [
        (crate::search::Direction::Forward, (4, 7)),
        (crate::search::Direction::Forward, (8, 11)),
        (crate::search::Direction::Forward, (4, 7)),
        (crate::search::Direction::Backward, (8, 11)),
    ] {
        let o = o.clone();
        step(move |app| app.search_step(o, dir))?;
        check!(
            step(selection)? == want,
            "範囲内の移動 {:?} != {want:?}",
            step(selection)?
        );
    }
    // 範囲の手前に挿入すると、範囲もずれる。
    step(|app| {
        app.with_text(id, |ed| {
            crate::document::EditTarget::replace(ed, crate::document::TextRange::new(0, 0), "ab\n")
        })
    })?
    .map_err(super::e2s)?
    .map_err(super::e2s)?;
    check!(
        scope_of_doc(id)? == Some((7, 14)),
        "範囲が追従する {:?}",
        scope_of_doc(id)?
    );
    // 「置換」は範囲の外の一致を置換しない。
    select(id, 0, 0)?;
    step(|app| app.replace_one(options("foo", true), "Q"))?;
    check!(
        text(id)? == "ab\nfoo\nfoo foo\nfoo\n" && step(selection)? == (7, 10),
        "範囲外は置換せず範囲内の次へ {:?} {:?}",
        text(id)?,
        step(selection)?
    );
    // 別のタブへ切り替えると範囲は解除される。
    step(|app| {
        if let Some(p) = app.find_panel {
            p.set_in_selection(true);
        }
    })?;
    let other = new_doc("zzz")?;
    check!(
        step(|app| app.scope_doc.is_none() && !app.find_panel.is_some_and(|p| p.in_selection()))?,
        "タブ切り替えで解除"
    );
    check!(scope_of_doc(id)?.is_none(), "元の文書の印も消える");
    close(other)?;
    // 解除すれば全体が対象に戻る。
    step(move |app| app.activate(id)).and_then(|r| r.map_err(super::e2s))?;
    wait("全体を強調", |app| {
        app.highlight_count() == 0 || app.highlight_count() == 4
    })?;
    close(id)?;
    // 文書 0 件でも、オン・オフ・検索・置換が安全。
    cmd("file.close_all", Value::Null)?;
    step(|app| {
        app.set_scope(true);
        app.search_step(options("foo", true), crate::search::Direction::Forward);
        app.replace_all(options("foo", true), "x".into());
        app.set_scope(false);
        app.clear_scope();
    })?;
    check!(step(|app| app.active_id().is_none())?, "0 件のまま");
    cmd("file.new", Value::Null)?;
    Ok(())
}

/// 検索・置換・Grep の起点の履歴: 新しい順、重複は先頭へ、最大 20 件、空は残さない。
fn search_history() -> Check {
    step(|app| {
        app.settings.search_history = Default::default();
        for w in ["a", "b", "a"] {
            app.on_find_event(FindEvent::Ran {
                find: w.into(),
                replace: Some(format!("{w}!")),
                grep_dir: None,
            });
        }
        app.on_find_event(FindEvent::Ran {
            find: String::new(),
            replace: Some(String::new()),
            grep_dir: Some("C:\\src".into()),
        });
        for i in 0..30 {
            app.on_find_event(FindEvent::Ran {
                find: format!("w{i}"),
                replace: None,
                grep_dir: None,
            });
        }
    })?;
    step(|app| -> Check {
        let h = &app.settings.search_history;
        check!(h.replace == ["a!", "b!"], "置換の履歴 {:?}", h.replace);
        check!(h.grep_dir == ["C:\\src"], "Grep の履歴 {:?}", h.grep_dir);
        check!(
            h.find.len() == 20 && h.find[0] == "w29",
            "検索の履歴 {:?}",
            h.find
        );
        Ok(())
    })??;
    step(|app| app.settings.search_history = Default::default())
}

/// 元に戻す 1 回で `original` に戻ることを確かめる。
fn undo_once(id: DocumentId, original: &str, what: &str) -> Check {
    cmd("edit.undo", Value::Null)?;
    check!(
        text(id)? == original,
        "{what}: Undo 1 回で戻る {:?}",
        text(id)?
    );
    Ok(())
}

fn line_commands() -> Check {
    let original = "one\ntwo\nthree\n";
    let id = new_doc(original)?;
    // 行の複製（選択なし）。
    select(id, 5, 5)?;
    cmd("edit.duplicate_line", Value::Null)?;
    check!(
        text(id)? == "one\ntwo\ntwo\nthree\n",
        "行の複製 {:?}",
        text(id)?
    );
    undo_once(id, original, "行の複製")?;
    // 選択の複製。
    select(id, 0, 3)?;
    cmd("edit.duplicate_line", Value::Null)?;
    check!(
        text(id)? == "oneone\ntwo\nthree\n",
        "選択の複製 {:?}",
        text(id)?
    );
    undo_once(id, original, "選択の複製")?;
    // 行の削除。
    select(id, 5, 5)?;
    cmd("edit.delete_line", Value::Null)?;
    check!(text(id)? == "one\nthree\n", "行の削除 {:?}", text(id)?);
    undo_once(id, original, "行の削除")?;
    select(id, 1, 6)?;
    cmd("edit.delete_line", Value::Null)?;
    check!(text(id)? == "three\n", "複数行の削除 {:?}", text(id)?);
    undo_once(id, original, "複数行の削除")?;
    // 選択が次の行の行頭で終わるときは、その行を含めない。
    select(id, 0, 4)?;
    cmd("edit.delete_line", Value::Null)?;
    check!(
        text(id)? == "two\nthree\n",
        "行頭で終わる選択 {:?}",
        text(id)?
    );
    undo_once(id, original, "行頭で終わる選択")?;
    // 行の移動。
    select(id, 5, 5)?;
    cmd("edit.move_line_up", Value::Null)?;
    check!(text(id)? == "two\none\nthree\n", "上へ {:?}", text(id)?);
    undo_once(id, original, "上へ移動")?;
    select(id, 5, 5)?;
    cmd("edit.move_line_down", Value::Null)?;
    check!(text(id)? == "one\nthree\ntwo\n", "下へ {:?}", text(id)?);
    undo_once(id, original, "下へ移動")?;
    close(id)?;

    // 最後の行（改行なし）の削除は、直前の改行ごと取り除く。
    let id = new_doc("a\nb")?;
    select(id, 3, 3)?;
    cmd("edit.delete_line", Value::Null)?;
    check!(text(id)? == "a", "最後の行の削除 {:?}", text(id)?);
    undo_once(id, "a\nb", "最後の行の削除")?;
    close(id)
}

fn text_commands() -> Check {
    // 大文字・小文字。
    let id = new_doc("abc Def")?;
    select(id, 0, 7)?;
    cmd("edit.uppercase", Value::Null)?;
    check!(text(id)? == "ABC DEF", "大文字 {:?}", text(id)?);
    undo_once(id, "abc Def", "大文字")?;
    select(id, 0, 7)?;
    cmd("edit.lowercase", Value::Null)?;
    check!(text(id)? == "abc def", "小文字 {:?}", text(id)?);
    undo_once(id, "abc Def", "小文字")?;
    select(id, 2, 2)?;
    cmd("edit.uppercase", Value::Null)?;
    check!(text(id)? == "abc Def", "選択なしは変えない");
    check!(
        step(|app| status(app).contains("選択"))?,
        "選択を促す {:?}",
        step(|app| status(app))?
    );
    close(id)?;

    // 行末の空白（文書全体は 1 回の Undo。選択があればその行だけ）。
    let original = "a  \nb\t\n c \nd";
    let id = new_doc(original)?;
    cmd("edit.trim_trailing_whitespace", Value::Null)?;
    check!(text(id)? == "a\nb\n c\nd", "行末の空白 {:?}", text(id)?);
    undo_once(id, original, "行末の空白")?;
    select(id, 4, 5)?;
    cmd("edit.trim_trailing_whitespace", Value::Null)?;
    check!(
        text(id)? == "a  \nb\n c \nd",
        "選択した行だけ {:?}",
        text(id)?
    );
    undo_once(id, original, "選択した行の行末の空白")?;
    close(id)?;

    // タブ⇔スペース（タブ幅 4）。
    step(|app| app.update_view(|v| v.tab_width = 4))?;
    let original = "\ta\n  \tb\n";
    let id = new_doc(original)?;
    cmd("edit.tabs_to_spaces", Value::Null)?;
    check!(
        text(id)? == "    a\n    b\n",
        "タブをスペースへ {:?}",
        text(id)?
    );
    undo_once(id, original, "タブをスペースへ")?;
    close(id)?;
    let original = "        x\n    y\n  z\nq   r\n";
    let id = new_doc(original)?;
    cmd("edit.spaces_to_tabs", Value::Null)?;
    check!(
        text(id)? == "\t\tx\n\ty\n  z\nq   r\n",
        "行頭のスペースをタブへ {:?}",
        text(id)?
    );
    undo_once(id, original, "スペースをタブへ")?;
    close(id)?;

    // 日時の挿入（2026-11-14 09:05 の形式）。
    let id = new_doc("x")?;
    select(id, 1, 1)?;
    cmd("edit.insert_datetime", Value::Null)?;
    let t = text(id)?;
    let b = t.as_bytes();
    check!(
        t.len() == 17
            && b[0] == b'x'
            && b[5] == b'-'
            && b[8] == b'-'
            && b[11] == b' '
            && b[14] == b':'
            && b[1..5].iter().all(u8::is_ascii_digit)
            && b[12..14].iter().all(u8::is_ascii_digit)
            && b[15..17].iter().all(u8::is_ascii_digit),
        "日時の形式 {t:?}"
    );
    undo_once(id, "x", "日時の挿入")?;
    close(id)
}

fn brace() -> Check {
    let id = new_doc("f(a[b]c) {x}")?;
    let caret = |app: &mut App| app.editor().selection().caret;
    select(id, 1, 1)?;
    cmd("edit.goto_matching_brace", Value::Null)?;
    check!(step(caret)? == 7, "( から ) へ {}", step(caret)?);
    cmd("edit.goto_matching_brace", Value::Null)?;
    check!(step(caret)? == 1, ") から ( へ {}", step(caret)?);
    select(id, 8, 8)?;
    cmd("edit.goto_matching_brace", Value::Null)?;
    check!(step(caret)? == 1, "閉じ括弧の直後から {}", step(caret)?);
    select(id, 3, 3)?;
    cmd("edit.goto_matching_brace", Value::Null)?;
    check!(step(caret)? == 5, "[ から ] へ {}", step(caret)?);
    select(id, 0, 0)?;
    cmd("edit.goto_matching_brace", Value::Null)?;
    check!(step(caret)? == 0, "括弧がなければ動かない");
    check!(
        step(|app| status(app).contains("対応する括弧がありません"))?,
        "案内 {:?}",
        step(|app| status(app))?
    );
    close(id)
}
