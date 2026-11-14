//! V6: 行整理の Undo と無変更、文書別ブックマークと折り返し移動。
use super::integration::{cmd, step, wait};
use super::{Check, Ctx};
use crate::command::Value;
use crate::document::Selection;

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond { return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!())); }
    };
}

pub(super) fn run(_ctx: &Ctx) -> Check {
    cmd("file.new", Value::Null)?;
    let id = step(|app| app.active_id().unwrap())?;
    step(|app| app.editor().set_text("z\r\nc\r\nb\r\na\r\nx"))?;
    // 選択の終わりが行頭なら、その行を含めない。
    step(|app| {
        app.editor().set_selection(Selection {
            anchor: 4,
            caret: 9,
        })
    })?;
    cmd("edit.sort_lines_asc", Value::Null)?;
    step(|app| -> Check {
        check!(
            app.editor().text() == "z\r\nb\r\nc\r\na\r\nx",
            "選択にかかる行だけを整理"
        );
        app.editor().undo();
        check!(
            app.editor().text() == "z\r\nc\r\nb\r\na\r\nx",
            "行整理の Undo"
        );
        Ok(())
    })??;
    // 選択なしなら全文。重複除去も 1 回で戻る。
    step(|app| {
        app.editor().set_text("b\na\nb\n");
        app.editor().set_selection(Selection::caret_at(0));
    })?;
    cmd("edit.remove_duplicate_lines", Value::Null)?;
    step(|app| -> Check {
        check!(app.editor().text() == "b\na\n", "重複除去と末尾改行");
        app.editor().undo();
        check!(app.editor().text() == "b\na\nb\n", "重複除去の Undo");
        app.editor().set_text("a\nb\n");
        Ok(())
    })??;
    // 大きな本文と同じワーカー経路でも Undo と版の照合を確認する。
    let threshold = step(|app| {
        let old = app.worker_search_bytes;
        app.worker_search_bytes = 0;
        app.editor().set_text("b\na\n");
        old
    })?;
    cmd("edit.sort_lines_asc", Value::Null)?;
    wait("非同期の行整理", |app| {
        app.editor().text() == "a\nb\n"
    })?;
    step(|app| -> Check {
        app.editor().undo();
        check!(app.editor().text() == "b\na\n", "非同期整理のUndo");
        Ok(())
    })??;
    cmd("edit.sort_lines_asc", Value::Null)?;
    step(|app| app.editor().set_text("利用者の編集"))?;
    wait("行整理の古い版を拒否", |app| {
        app.status_message() == Some("行を整理している間に文書が変更されたため、反映しませんでした")
    })?;
    step(|app| -> Check {
        check!(
            app.editor().text() == "利用者の編集",
            "古い結果で編集を上書きした"
        );
        app.worker_search_bytes = threshold;
        app.editor().set_text("a\nb\n");
        Ok(())
    })??;
    let version = step(|app| app.state(id).unwrap().version())?;
    cmd("edit.sort_lines_asc", Value::Null)?;
    step(|app| -> Check {
        check!(
            app.state(id).unwrap().version() == version,
            "同じ並びで版が変わった"
        );
        app.editor().set_text("one\ntwo\nthree\nfour\n");
        app.editor().set_selection(Selection::caret_at(4));
        Ok(())
    })??;
    let version = step(|app| app.state(id).unwrap().version())?;
    cmd("search.toggle_bookmark", Value::Null)?;
    step(|app| app.editor().set_selection(Selection::caret_at(14)))?;
    cmd("search.toggle_bookmark", Value::Null)?;
    step(|app| -> Check {
        check!(
            app.editor().has_bookmark(1) && app.editor().has_bookmark(3),
            "行の印"
        );
        check!(
            app.state(id).unwrap().version() == version,
            "印で文書の版が変わった"
        );
        Ok(())
    })??;
    cmd("search.next_bookmark", Value::Null)?;
    step(|app| -> Check {
        check!(app.editor().selection().caret == 4, "次は先頭へ折り返す");
        Ok(())
    })??;
    cmd("search.prev_bookmark", Value::Null)?;
    step(|app| -> Check {
        check!(app.editor().selection().caret == 14, "前は末尾へ折り返す");
        Ok(())
    })??;
    // タブを切り替えると他の文書の印と混ざらない。
    cmd("file.new", Value::Null)?;
    let second = step(|app| app.active_id().unwrap())?;
    step(|app| -> Check {
        check!(!app.editor().has_bookmark(0), "新しい文書に前の印が残った");
        app.activate(id).map_err(super::e2s)?;
        check!(
            app.editor().has_bookmark(1) && app.editor().has_bookmark(3),
            "タブ切替で印が消えた"
        );
        Ok(())
    })??;
    cmd("search.toggle_bookmark", Value::Null)?;
    step(|app| -> Check {
        check!(!app.editor().has_bookmark(3), "印の解除");
        Ok(())
    })??;
    cmd("search.clear_bookmarks", Value::Null)?;
    cmd("search.next_bookmark", Value::Null)?;
    step(|app| -> Check {
        check!(!app.editor().has_bookmark(1), "全解除");
        check!(
            app.status_message() == Some("ブックマークがありません"),
            "印がないときの通知"
        );
        let _ = app.close_document(second, true);
        let _ = app.close_document(id, true);
        Ok(())
    })??;
    Ok(())
}
