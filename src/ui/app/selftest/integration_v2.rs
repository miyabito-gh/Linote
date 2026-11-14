//! 改善 V2 の自己診断: すべて保存（順序・取り消しで止まる）、閉じたタブの開き直し
//! （新しい順・存在しないファイルを飛ばす・カーソル・メモ）、メモの削除（確認・版の不一致）、
//! 言語の手動選択（文書ごと・保存しても保つ・自動に戻す）。

use serde_json::{json, Value};

use super::integration::{cmd, open_and_wait, step, wait};
use super::{Check, Ctx};
use crate::core::DocumentId;
use crate::document::{DocumentKind, EditTarget, Selection};
use crate::ui::editor::Language;

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!()));
        }
    };
}

pub(super) fn run(ctx: &Ctx) -> Check {
    zero_documents()?;
    save_all(ctx)?;
    reopen_closed(ctx)?;
    delete_memo(ctx)?;
    language_choice(ctx)?;
    Ok(())
}

fn close_all() -> Check {
    step(|app| {
        let ids: Vec<DocumentId> = app.documents().iter().map(|d| d.id()).collect();
        for id in ids {
            let _ = app.close_document(id, true);
        }
    })
}

fn set_text(id: DocumentId, text: &'static str) -> Check {
    step(|app| app.with_text(id, |ed| ed.set_text(text)))?.map_err(super::e2s)
}

fn is_dirty(id: DocumentId) -> Result<bool, String> {
    step(|app| app.state(id).is_some_and(|s| s.is_dirty()))
}

/// 文書 0 件でも、新しいコマンドは安全（すべて保存・言語は何もしない、開き直しは案内だけ）。
fn zero_documents() -> Check {
    close_all()?;
    step(|app| app.closed_tabs_clear_for_test())?;
    cmd("file.save_all", Value::Null)?;
    cmd("view.language", json!({ "language": "rust" }))?;
    cmd("file.reopen_closed", Value::Null)?;
    step(|app| -> Check {
        check!(app.documents().is_empty(), "0 件のまま");
        check!(
            app.needs_document(crate::command::CommandId::new("file.save_all"))
                && app.needs_document(crate::command::CommandId::new("view.language"))
                && !app.needs_document(crate::command::CommandId::new("file.reopen_closed")),
            "needs_document の判定"
        );
        Ok(())
    })?
}

fn save_all(ctx: &Ctx) -> Check {
    close_all()?;
    let pa = ctx.dir.join("v2-a.txt");
    let pb = ctx.dir.join("v2-b.txt");
    std::fs::write(&pa, "a\n").map_err(|e| e.to_string())?;
    std::fs::write(&pb, "b\n").map_err(|e| e.to_string())?;
    let a = open_and_wait(&pa)?;
    cmd("file.new", Value::Null)?;
    let u = step(|app| app.active_id().unwrap())?;
    let b = open_and_wait(&pb)?;
    set_text(a, "a2")?;
    set_text(u, "untitled")?;
    set_text(b, "b2")?;

    // 名前のない文書で保存を取り消すと、そこで止まる（右の b は保存しない）。
    ctx.prompts.save.borrow_mut().push_back(None);
    cmd("file.save_all", Value::Null)?;
    wait("a の保存", move |app| {
        app.state(a).is_some_and(|s| !s.is_dirty()) && app.document(a).is_some_and(|d| !d.saving)
    })?;
    check!(is_dirty(u)?, "取り消した無題は未保存のまま");
    check!(is_dirty(b)?, "取り消した後ろの文書は保存しない");
    check!(
        std::fs::read_to_string(&pa).map_err(|e| e.to_string())? == "a2",
        "a の内容"
    );
    check!(
        std::fs::read_to_string(&pb).map_err(|e| e.to_string())? == "b\n",
        "b は変わらない"
    );

    // 保存先を選ぶと全部保存される。
    let pu = ctx.dir.join("v2-u.txt");
    ctx.prompts.save.borrow_mut().push_back(Some(pu.clone()));
    cmd("file.save_all", Value::Null)?;
    wait("すべて保存", move |app| {
        [a, u, b].iter().all(|&id| {
            app.state(id).is_some_and(|s| !s.is_dirty())
                && app.document(id).is_some_and(|d| !d.saving)
        })
    })?;
    check!(
        std::fs::read_to_string(&pu).map_err(|e| e.to_string())? == "untitled",
        "無題の保存先"
    );
    check!(
        std::fs::read_to_string(&pb).map_err(|e| e.to_string())? == "b2",
        "b の内容"
    );
    close_all()
}

fn reopen_closed(ctx: &Ctx) -> Check {
    close_all()?;
    step(|app| app.closed_tabs_clear_for_test())?;
    let pa = ctx.dir.join("v2-ra.txt");
    let pb = ctx.dir.join("v2-rb.txt");
    let pc = ctx.dir.join("v2-rc.txt");
    for p in [&pa, &pb, &pc] {
        std::fs::write(p, "0123456789\nabcdef\n").map_err(|e| e.to_string())?;
    }
    let a = open_and_wait(&pa)?;
    let b = open_and_wait(&pb)?;
    let c = open_and_wait(&pc)?;
    step(|app| app.activate(a))?.map_err(super::e2s)?;
    step(|app| {
        app.with_text(a, |ed| {
            EditTarget::set_selection(ed, Selection::caret_at(5))
        })
    })?
    .map_err(super::e2s)?;
    let _ = (b, c);
    // 閉じる順: a, b, c。開き直しは c, b, a の順。
    for id in [a, b, c] {
        step(|app| app.close_document(id, false))?.map_err(super::e2s)?;
    }
    check!(step(|app| app.documents().is_empty())?, "全部閉じた");
    // b のファイルは消す（飛ばして次へ）。
    std::fs::remove_file(&pb).map_err(|e| e.to_string())?;
    cmd("file.reopen_closed", Value::Null)?;
    wait("c が開き直る", |app| {
        app.documents()
            .iter()
            .any(|d| d.state.path().is_some_and(|p| p.ends_with("v2-rc.txt")))
    })?;
    cmd("file.reopen_closed", Value::Null)?;
    wait("a が開き直る（b は飛ばす）", |app| {
        app.documents()
            .iter()
            .any(|d| d.state.path().is_some_and(|p| p.ends_with("v2-ra.txt")))
    })?;
    step(|app| -> Check {
        check!(app.documents().len() == 2, "b は開かない");
        let id = app
            .documents()
            .iter()
            .find(|d| d.state.path().is_some_and(|p| p.ends_with("v2-ra.txt")))
            .map(|d| d.id())
            .unwrap();
        app.activate(id).map_err(super::e2s)?;
        check!(app.editor().selection().caret == 5, "カーソル位置を戻す");
        Ok(())
    })??;

    // 履歴が空なら何も開かない。
    step(|app| app.closed_tabs_clear_for_test())?;
    cmd("file.reopen_closed", Value::Null)?;
    check!(step(|app| app.documents().len())? == 2, "履歴が空");

    // メモは ID で開き直す。
    let store = crate::memo::MemoStore::new(&ctx.paths);
    let memo = store
        .create("v2 reopen", "body")
        .map_err(|e| e.to_string())?;
    let mid = step(|app| app.open_memo(memo.clone()))?.map_err(super::e2s)?;
    step(|app| app.close_document(mid, false))?.map_err(super::e2s)?;
    cmd("file.reopen_closed", Value::Null)?;
    step(|app| -> Check {
        check!(
            app.documents()
                .iter()
                .any(|d| matches!(d.state.kind(), DocumentKind::Memo(m) if *m == memo.id)),
            "メモを開き直した"
        );
        Ok(())
    })??;
    close_all()
}

fn delete_memo(ctx: &Ctx) -> Check {
    close_all()?;
    let store = crate::memo::MemoStore::new(&ctx.paths);

    // 確認で取り消せば消さない。
    let m1 = store.create("v2 del1", "x").map_err(|e| e.to_string())?;
    ctx.prompts.confirm.borrow_mut().push_back(false);
    step(|app| app.delete_memo_confirmed(&m1.id, &m1.name))?;
    check!(store.load(&m1.id).is_ok(), "取り消したのに消えた");

    // 開いているタブがあれば閉じて消す。
    let id = step(|app| app.open_memo(m1.clone()))?.map_err(super::e2s)?;
    ctx.prompts.confirm.borrow_mut().push_back(true);
    step(|app| app.delete_memo_confirmed(&m1.id, &m1.name))?;
    check!(store.load(&m1.id).is_err(), "消えていない");
    check!(
        step(|app| app.document(id).is_none())?,
        "タブが閉じていない"
    );

    // 版が合わなければ消さない。
    let m2 = store.create("v2 del2", "y").map_err(|e| e.to_string())?;
    step(|app| app.open_memo(m2.clone()))?.map_err(super::e2s)?;
    store
        .update(&m2.id, m2.version, "changed elsewhere")
        .map_err(|e| e.to_string())?;
    ctx.prompts.confirm.borrow_mut().push_back(true);
    step(|app| app.delete_memo_confirmed(&m2.id, &m2.name))?;
    check!(store.load(&m2.id).is_ok(), "版の不一致なのに消えた");
    close_all()?;
    store
        .delete(
            &m2.id,
            store.load(&m2.id).map_err(|e| e.to_string())?.version,
        )
        .map_err(|e| e.to_string())
}

fn language_choice(ctx: &Ctx) -> Check {
    close_all()?;
    let p = ctx.dir.join("v2-lang.txt");
    std::fs::write(&p, "fn main() {}\n").map_err(|e| e.to_string())?;
    let other = ctx.dir.join("v2-lang2.txt");
    std::fs::write(&other, "x\n").map_err(|e| e.to_string())?;
    let id = open_and_wait(&p)?;
    let id2 = open_and_wait(&other)?;
    step(|app| app.activate(id))
        .map_err(|e| e.to_string())?
        .map_err(super::e2s)?;
    cmd("view.language", json!({ "language": "rust" }))?;
    step(|app| -> Check {
        check!(
            app.document(id).unwrap().language == Language::Rust,
            "Rust にならない"
        );
        check!(
            app.document(id2).unwrap().language == Language::PlainText,
            "他の文書は変わらない"
        );
        check!(
            app.status.info().language == "Rust",
            "ステータスバー {}",
            app.status.info().language
        );
        check!(
            app.view_menu_checked(
                crate::command::CommandId::new("view.language"),
                &json!({ "language": "rust" })
            ) == Some(true)
                && app.view_menu_checked(
                    crate::command::CommandId::new("view.language"),
                    &json!({ "language": "auto" })
                ) == Some(false),
            "メニューの印"
        );
        Ok(())
    })??;
    // 保存しても手動の選択を保つ。
    set_text(id, "fn main() {}\n!")?;
    cmd("file.save", Value::Null)?;
    wait("保存", move |app| {
        app.document(id)
            .is_some_and(|d| !d.saving && !d.state.is_dirty())
    })?;
    check!(
        step(|app| app.document(id).unwrap().language)? == Language::Rust,
        "保存で言語が戻った"
    );
    cmd("view.language", json!({ "language": "none" }))?;
    check!(
        step(|app| app.document(id).unwrap().language)? == Language::PlainText,
        "なし"
    );
    cmd("view.language", json!({ "language": "auto" }))?;
    check!(
        step(|app| app.document(id).unwrap().language)? == Language::PlainText,
        "拡張子 .txt から自動"
    );
    close_all()
}
