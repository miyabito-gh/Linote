//! 改善 V5 の自己診断: 新規文書の既定の形式（無題・クリップボード由来）、保存時の整形
//! （行末の空白の除去、最終行の改行、Markdown の扱い、1 回の Undo、カーソルの保持、
//! 変化がないときは版を動かさない）、設定画面の項目。

use serde_json::json;

use super::integration::{cmd, open_and_wait, step, wait};
use super::{text_of, Check, Ctx};
use crate::command::Value;
use crate::core::DocumentId;
use crate::document::Selection;
use crate::platform::clipboard;
use crate::text::{Encoding, Eol};

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!()));
        }
    };
}

pub(super) fn run(ctx: &Ctx) -> Check {
    new_file_defaults()?;
    save_tidy(ctx)?;
    settings_items()?;
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

fn save_and_wait(id: DocumentId) -> Check {
    cmd("file.save", Value::Null)?;
    wait("保存が終わる", move |app| {
        app.document(id).is_some_and(|d| !d.saving)
    })
}

/// 無題・クリップボード由来の文書が、設定の文字コードと改行で作られる。
fn new_file_defaults() -> Check {
    close_all()?;
    step(|app| {
        app.settings.new_file.format.encoding = Encoding::Cp932;
        app.settings.new_file.format.bom = false;
        app.settings.new_file.format.eol = Eol::Lf;
    })?;
    cmd("file.new", Value::Null)?;
    step(|app| -> Check {
        let id = app.active_id().unwrap();
        let f = app.state(id).unwrap().format;
        check!(
            f.encoding == Encoding::Cp932 && !f.bom && f.eol == Eol::Lf,
            "無題の形式 {f:?}"
        );
        Ok(())
    })??;
    // クリップボード由来: 内容に改行があればそれを優先し、文字コードは設定に従う。
    step(|app| clipboard::write_text(Some(app.hwnd()), "a\nb"))?.map_err(super::e2s)?;
    cmd("clipboard.new_tab", Value::Null)?;
    step(|app| -> Check {
        let id = app.active_id().unwrap();
        let f = app.state(id).unwrap().format;
        check!(
            f.encoding == Encoding::Cp932 && f.eol == Eol::Lf,
            "形式 {f:?}"
        );
        Ok(())
    })??;
    step(|app| {
        app.settings.new_file.format.encoding = Encoding::Utf8;
        app.settings.new_file.format.bom = true;
        app.settings.new_file.format.eol = Eol::CrLf;
    })?;
    cmd("file.new", Value::Null)?;
    step(|app| -> Check {
        let id = app.active_id().unwrap();
        let f = app.state(id).unwrap().format;
        check!(
            f.encoding == Encoding::Utf8 && f.bom && f.eol == Eol::CrLf,
            "BOM 付き {f:?}"
        );
        Ok(())
    })??;
    // 既定に戻す。
    step(|app| app.settings.new_file = Default::default())?;
    close_all()
}

fn set_save(trim: bool, newline: bool) -> Check {
    step(|app| {
        app.settings.save.trim_trailing_whitespace = trim;
        app.settings.save.ensure_final_newline = newline;
    })
}

fn save_tidy(ctx: &Ctx) -> Check {
    close_all()?;
    // 設定が無効なら整形しない。
    let p = ctx.dir.join("v5-a.txt");
    std::fs::write(&p, "x\r\n").map_err(|e| e.to_string())?;
    let id = open_and_wait(&p)?;
    let before = "a  \r\nb\t\r\n   \r\nc  ";
    step(|app| app.with_text(id, |ed| ed.set_text(before)))?.map_err(super::e2s)?;
    set_save(false, false)?;
    save_and_wait(id)?;
    check!(
        std::fs::read(&p).map_err(|e| e.to_string())? == before.as_bytes(),
        "無効なのに整形された"
    );

    // 両方有効: 行末の空白が除かれ、最終行に（文書の）CRLF が付く。
    set_save(true, true)?;
    // カーソルを置く。
    step(|app| app.with_text(id, |ed| ed.set_text(before)))?.map_err(super::e2s)?;
    step(|app| app.editor().set_selection(Selection::caret_at(1)))?;
    save_and_wait(id)?;
    let saved = std::fs::read(&p).map_err(|e| e.to_string())?;
    check!(
        saved == b"a\r\nb\r\n\r\nc\r\n",
        "整形の結果 {:?}",
        String::from_utf8_lossy(&saved)
    );
    step(|app| -> Check {
        check!(text_of(app, id) == "a\r\nb\r\n\r\nc\r\n", "編集欄の本文");
        check!(
            app.editor().selection().caret == 1,
            "カーソル {:?}",
            app.editor().selection()
        );
        check!(!app.state(id).unwrap().is_dirty(), "保存後は未保存でない");
        Ok(())
    })??;
    // 1 回の Undo で整形前に戻る。
    step(|app| -> Check {
        app.editor().undo();
        check!(text_of(app, id) == before, "Undo 後 {:?}", text_of(app, id));
        check!(app.state(id).unwrap().is_dirty(), "戻すと未保存");
        Ok(())
    })??;

    // 変化がなければ版も未保存の印も動かさない。
    step(|app| app.with_text(id, |ed| ed.set_text("a\r\nb\r\n")))?.map_err(super::e2s)?;
    save_and_wait(id)?;
    let v1 = step(|app| app.state(id).unwrap().version())?;
    save_and_wait(id)?;
    step(|app| -> Check {
        check!(app.state(id).unwrap().version() == v1, "版が動いた");
        check!(!app.state(id).unwrap().is_dirty(), "未保存になった");
        Ok(())
    })??;

    // 空の本文には改行を足さない。LF の文書には LF を足す。
    step(|app| app.with_text(id, |ed| ed.set_text("")))?.map_err(super::e2s)?;
    save_and_wait(id)?;
    check!(
        std::fs::read(&p).map_err(|e| e.to_string())?.is_empty(),
        "空の本文に改行が付いた"
    );
    let q = ctx.dir.join("v5-lf.txt");
    std::fs::write(&q, "x\n").map_err(|e| e.to_string())?;
    let lf = open_and_wait(&q)?;
    step(|app| app.with_text(lf, |ed| ed.set_text("l1 \nl2")))?.map_err(super::e2s)?;
    save_and_wait(lf)?;
    check!(
        std::fs::read(&q).map_err(|e| e.to_string())? == b"l1\nl2\n",
        "LF の整形"
    );

    // Markdown では行末の空白を除かない（最終行の改行は補う）。
    let m = ctx.dir.join("v5-md.md");
    std::fs::write(&m, "x\n").map_err(|e| e.to_string())?;
    let md = open_and_wait(&m)?;
    step(|app| app.with_text(md, |ed| ed.set_text("line  \nnext  ")))?.map_err(super::e2s)?;
    save_and_wait(md)?;
    let saved = std::fs::read(&m).map_err(|e| e.to_string())?;
    check!(
        saved == b"line  \nnext  \n",
        "Markdown の整形 {:?}",
        String::from_utf8_lossy(&saved)
    );

    // 名前を付けて保存でも整形する（無題 → 保存先を指定）。
    cmd("file.new", Value::Null)?;
    let u = step(|app| app.active_id().unwrap())?;
    step(|app| app.with_text(u, |ed| ed.set_text("new  ")))?.map_err(super::e2s)?;
    let dest = ctx.dir.join("v5-as.txt");
    cmd("file.save_as", json!({ "path": dest.to_string_lossy() }))?;
    wait("名前を付けて保存", move |app| {
        app.document(u).is_some_and(|d| !d.saving) && dest_exists(&dest)
    })?;
    let saved = std::fs::read(ctx.dir.join("v5-as.txt")).map_err(|e| e.to_string())?;
    check!(
        saved == b"new\r\n",
        "名前を付けて保存の整形 {:?}",
        String::from_utf8_lossy(&saved)
    );
    set_save(false, false)?;
    close_all()
}

fn dest_exists(p: &std::path::Path) -> bool {
    p.exists()
}

/// 設定画面の「ファイル」ページと項目。
fn settings_items() -> Check {
    use crate::appdata::settings::{pages, ItemValue};
    let page = pages()
        .iter()
        .find(|p| p.id == "file")
        .ok_or("「ファイル」ページがない")?;
    for id in [
        "new_file.encoding",
        "new_file.eol",
        "save.trim_trailing_whitespace",
        "save.ensure_final_newline",
    ] {
        check!(page.items.iter().any(|i| i.id == id), "項目 {id} がない");
    }
    let mut s = crate::appdata::Settings::default();
    let enc = page
        .items
        .iter()
        .find(|i| i.id == "new_file.encoding")
        .unwrap();
    enc.set(&mut s, &ItemValue::Text("shift_jis".into()));
    check!(
        s.new_file.format.encoding == Encoding::Cp932,
        "設定への書き込み"
    );
    check!(
        enc.get(&s) == ItemValue::Text("shift_jis".into()),
        "設定からの読み出し"
    );
    Ok(())
}
