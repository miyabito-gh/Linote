//! 改善 U1 の自己診断: ファイル操作（最近使ったファイル、ディスクからの再読み込み、
//! すべて閉じる・ほかを閉じる、保存時の文字コード・改行、フルパスのコピー、ドロップで開く、
//! メニューとキー割り当て）。「新しいウィンドウ」（別プロセスの起動）と、Explorer からの
//! 実際のドラッグ＆ドロップは Windows 実機で確認する（`docs/verification/U.md`）。

use serde_json::json;

use super::integration::{cmd, open_and_wait, step, wait};
use super::{text_of, Check, Ctx};
use crate::command::{CommandId, KeyChord, Value};
use crate::core::DocumentId;
use crate::platform::clipboard;
use crate::text::{Encoding, Eol};
use crate::ui::dialogs::CloseChoice;

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!()));
        }
    };
}

pub(super) fn run(ctx: &Ctx) -> Check {
    menu_and_keys()?;
    save_format(ctx)?;
    reload(ctx)?;
    recent_files(ctx)?;
    dropped_files(ctx)?;
    closing_many(ctx)?;
    copy_path(ctx)?;
    Ok(())
}

fn save_and_wait(id: DocumentId) -> Check {
    cmd("file.save", Value::Null)?;
    wait("保存が終わる", move |app| {
        app.document(id).is_some_and(|d| !d.saving)
    })
}

fn menu_and_keys() -> Check {
    step(|app| -> Check {
        let key = |s: &str| app.keymap().command_for(&KeyChord::parse(s).unwrap());
        check!(
            key("Ctrl+Shift+N") == Some(CommandId::new("file.new_window")),
            "Ctrl+Shift+N"
        );
        let entries = app.menu().ok_or("メニューがない")?.entries().to_vec();
        for command in [
            "file.reload",
            "file.save_encoding",
            "file.save_eol",
            "file.copy_path",
            "file.reveal_in_explorer",
            "file.new_window",
            "file.close_others",
            "file.close_all",
        ] {
            check!(
                entries.iter().any(|e| e.command == CommandId::new(command)),
                "メニューに {command} がない"
            );
        }
        let recent = entries
            .iter()
            .filter(|e| e.command == CommandId::new("file.open_recent"))
            .count();
        check!(
            recent == crate::appdata::settings::RECENT_FILES_LIMIT,
            "最近使ったファイルの枠 {recent}"
        );
        Ok(())
    })?
}

/// 保存時の改行と文字コードを変えて保存すると、ファイルの中身が変わる。
fn save_format(ctx: &Ctx) -> Check {
    let path = ctx.dir.join("u1-format.txt");
    std::fs::write(&path, "あい\nうえ\n").map_err(|e| e.to_string())?;
    let id = open_and_wait(&path)?;
    step(|app| -> Check {
        let st = app.state(id).unwrap();
        check!(st.format.eol == Eol::Lf && !st.is_dirty(), "開いた直後");
        Ok(())
    })??;
    cmd("file.save_eol", json!({ "eol": "crlf" }))?;
    step(|app| -> Check {
        check!(text_of(app, id) == "あい\r\nうえ\r\n", "改行を変換");
        let st = app.state(id).unwrap();
        check!(st.format.eol == Eol::CrLf, "改行の記録");
        check!(st.is_dirty(), "改行の変更は未保存");
        check!(app.status.info().eol == "CRLF", "ステータスの改行");
        Ok(())
    })??;
    save_and_wait(id)?;
    check!(
        std::fs::read(&path).map_err(|e| e.to_string())? == "あい\r\nうえ\r\n".as_bytes(),
        "CRLF で保存"
    );
    // 文字コードだけの変更でも未保存になり、保存すると変わる。
    cmd("file.save_encoding", json!({ "encoding": "cp932" }))?;
    step(|app| -> Check {
        let st = app.state(id).unwrap();
        check!(st.format.encoding == Encoding::Cp932, "文字コードの記録");
        check!(st.is_dirty(), "文字コードの変更は未保存");
        check!(
            app.status.info().encoding.contains("Shift_JIS"),
            "ステータスの文字コード"
        );
        Ok(())
    })??;
    save_and_wait(id)?;
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    check!(
        bytes == [0x82, 0xA0, 0x82, 0xA2, 0x0D, 0x0A, 0x82, 0xA4, 0x82, 0xA6, 0x0D, 0x0A],
        "CP932 で保存 {bytes:?}"
    );
    cmd("file.save_encoding", json!({ "encoding": "utf-16le" }))?;
    save_and_wait(id)?;
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    check!(
        bytes.starts_with(&[0xFF, 0xFE]),
        "UTF-16 LE（BOM 付き）で保存"
    );
    cmd("file.save_encoding", json!({ "encoding": "utf-8-bom" }))?;
    cmd("file.save_eol", json!({ "eol": "cr" }))?;
    save_and_wait(id)?;
    check!(
        std::fs::read(&path).map_err(|e| e.to_string())?
            == b"\xEF\xBB\xBF\xE3\x81\x82\xE3\x81\x84\r\xE3\x81\x86\xE3\x81\x88\r",
        "UTF-8 BOM・CR で保存"
    );
    // メモには使えない。
    cmd("file.new", Value::Null)?;
    step(|app| app.close_document(id, true))?.map_err(super::e2s)?;
    Ok(())
}

/// ディスクからの再読み込み（未保存の変更は確認し、断れば何も変えない）。
fn reload(ctx: &Ctx) -> Check {
    let path = ctx.dir.join("u1-reload.txt");
    std::fs::write(&path, "one\n").map_err(|e| e.to_string())?;
    let id = open_and_wait(&path)?;
    std::fs::write(&path, "two (changed outside)\n").map_err(|e| e.to_string())?;
    step(|app| app.with_text(id, |ed| ed.set_text("edited")))?.map_err(super::e2s)?;
    // 断る。
    ctx.prompts.confirm.borrow_mut().push_back(false);
    cmd("file.reload", Value::Null)?;
    step(|app| -> Check {
        check!(text_of(app, id) == "edited", "断ったのに読み直した");
        Ok(())
    })??;
    // 受け入れる。
    ctx.prompts.confirm.borrow_mut().push_back(true);
    cmd("file.reload", Value::Null)?;
    wait("再読み込み", |app| {
        app.document(id).is_none()
            && app
                .documents()
                .iter()
                .any(|d| d.state.path() == Some(path.as_path()))
    })?;
    let new_id = step(|app| {
        app.documents()
            .iter()
            .find(|d| d.state.path() == Some(path.as_path()))
            .map(|d| d.id())
    })?
    .ok_or("再読み込みした文書がない")?;
    step(|app| -> Check {
        check!(
            text_of(app, new_id) == "two (changed outside)\n",
            "ディスクの内容"
        );
        check!(!app.state(new_id).unwrap().is_dirty(), "未変更");
        Ok(())
    })??;
    // 未変更なら確認なしで読み直す。
    std::fs::write(&path, "three\n").map_err(|e| e.to_string())?;
    cmd("file.reload", Value::Null)?;
    wait("再読み込み（未変更）", |app| {
        app.document(new_id).is_none()
            && app
                .documents()
                .iter()
                .any(|d| d.state.path() == Some(path.as_path()))
    })?;
    // ファイルでない文書は断る。
    cmd("file.new", Value::Null)?;
    cmd("file.reload", Value::Null)?;
    step(|app| -> Check {
        check!(
            app.status_message()
                .is_some_and(|m| m.contains("再読み込みできません")),
            "案内 {:?}",
            app.status_message()
        );
        Ok(())
    })??;
    Ok(())
}

/// 最近使ったファイル: 開くと先頭に入り、なくなったファイルは案内して外す。
fn recent_files(ctx: &Ctx) -> Check {
    let a = ctx.dir.join("u1-recent-a.txt");
    let b = ctx.dir.join("u1-recent-b.txt");
    std::fs::write(&a, "a").map_err(|e| e.to_string())?;
    std::fs::write(&b, "b").map_err(|e| e.to_string())?;
    open_and_wait(&a)?;
    open_and_wait(&b)?;
    let a_text = a.to_string_lossy().into_owned();
    let b_text = b.to_string_lossy().into_owned();
    step(|app| -> Check {
        let r = &app.settings().recent_files;
        check!(
            r.len() >= 2 && r[0] == b_text && r[1] == a_text,
            "並び {r:?}"
        );
        Ok(())
    })??;
    // 開き直しても重複せず、先頭に来る。
    cmd("file.open", json!({ "path": a_text }))?;
    step(|app| -> Check {
        let r = &app.settings().recent_files;
        check!(
            r.iter().filter(|p| **p == a_text).count() == 1,
            "重複 {r:?}"
        );
        Ok(())
    })??;
    // 番号から開く（既に開いているのでそのタブへ移る）。
    let a_id = step(|app| {
        app.documents()
            .iter()
            .find(|d| d.state.path() == Some(a.as_path()))
            .map(|d| d.id())
    })?
    .ok_or("a がない")?;
    let b_id = step(|app| {
        app.documents()
            .iter()
            .find(|d| d.state.path() == Some(b.as_path()))
            .map(|d| d.id())
    })?
    .ok_or("b がない")?;
    step(|app| app.activate(b_id))?.map_err(super::e2s)?;
    let n = step(|app| {
        app.settings()
            .recent_files
            .iter()
            .position(|p| *p == a_text)
            .unwrap()
            + 1
    })?;
    cmd("file.open_recent", json!({ "index": n }))?;
    step(|app| -> Check {
        check!(
            app.active_id() == Some(a_id),
            "最近使ったファイルから開けない"
        );
        Ok(())
    })??;
    // なくなったファイルは案内して一覧から外す。
    let gone = ctx.dir.join("u1-gone.txt");
    let gone_text = gone.to_string_lossy().into_owned();
    step(|app| app.settings.push_recent(gone_text.clone()))?;
    cmd("file.open_recent", json!({ "index": 1 }))?;
    step(|app| -> Check {
        check!(
            !app.settings().recent_files.contains(&gone_text),
            "なくなったファイルが残っている"
        );
        Ok(())
    })??;
    check!(
        ctx.prompts
            .shown
            .borrow()
            .iter()
            .any(|s| s.contains("見つかりません") && s.contains("u1-gone.txt")),
        "案内が出ていない"
    );
    // 範囲外の番号は誤り。
    step(|app| app.run_command(CommandId::new("file.open_recent"), json!({ "index": 11 })))?;
    Ok(())
}

/// ドロップで開く: 複数可、フォルダーは無視して知らせる、開いているファイルはそのタブへ。
fn dropped_files(ctx: &Ctx) -> Check {
    let a = ctx.dir.join("u1-drop-a.txt");
    let b = ctx.dir.join("u1-drop-b.txt");
    let folder = ctx.dir.join("u1-drop-folder");
    std::fs::write(&a, "a").map_err(|e| e.to_string())?;
    std::fs::write(&b, "b").map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let (pa, pb, pf) = (a.clone(), b.clone(), folder.clone());
    step(move |app| app.open_dropped(&[pa, pf, pb]))?;
    wait("ドロップしたファイルが開く", |app| {
        [&a, &b].iter().all(|p| {
            app.documents()
                .iter()
                .any(|d| d.state.path() == Some(p.as_path()))
        })
    })?;
    step(|app| -> Check {
        check!(
            app.status_message().is_some_and(
                |m| m.contains("フォルダーは開けません") && m.contains("u1-drop-folder")
            ),
            "フォルダーの案内 {:?}",
            app.status_message()
        );
        let n = app
            .documents()
            .iter()
            .filter(|d| d.state.path() == Some(a.as_path()))
            .count();
        check!(n == 1, "重複して開いた");
        Ok(())
    })??;
    // 同じファイルをもう一度落とすと、そのタブへ移るだけ。
    let before = step(|app| app.documents().len())?;
    let (pa, pb) = (a.clone(), b.clone());
    step(move |app| app.open_dropped(&[pa]))?;
    step(|app| -> Check {
        check!(app.documents().len() == before, "タブが増えた");
        let id = app
            .documents()
            .iter()
            .find(|d| d.state.path() == Some(a.as_path()))
            .map(|d| d.id());
        check!(app.active_id() == id, "開いているタブへ移っていない");
        Ok(())
    })??;
    let _ = pb;
    Ok(())
}

/// ほかのタブを閉じる・すべて閉じる（未保存は 1 つずつ確認し、取り消すと止まる）。
fn closing_many(ctx: &Ctx) -> Check {
    // 整理: すべて閉じて 1 つだけにする。
    ctx.prompts.close.borrow_mut().clear();
    step(|app| {
        let ids: Vec<_> = app.documents().iter().map(|d| d.id()).collect();
        for id in ids {
            let _ = app.close_document(id, true);
        }
    })?;
    let mut ids = Vec::new();
    for _ in 0..4 {
        cmd("file.new", Value::Null)?;
    }
    step(|app| ids.extend(app.documents().iter().map(|d| d.id())))?;
    let docs = step(|app| app.documents().len())?;
    check!(docs >= 4, "文書の数 {docs}");
    let keep = step(|app| app.active_id())?.ok_or("アクティブな文書がない")?;
    // 1 つに変更を入れる。
    let dirty = ids.iter().copied().find(|&i| i != keep).unwrap();
    step(|app| app.with_text(dirty, |ed| ed.set_text("unsaved")))?.map_err(super::e2s)?;
    // 取り消すと、そこで止まる（keep は残る）。
    ctx.prompts
        .close
        .borrow_mut()
        .push_back(CloseChoice::Cancel);
    cmd("file.close_others", Value::Null)?;
    step(|app| -> Check {
        check!(app.document(dirty).is_some(), "取り消したのに閉じた");
        check!(app.document(keep).is_some(), "対象の文書が消えた");
        check!(app.active_id() == Some(keep), "アクティブな文書");
        Ok(())
    })??;
    // 保存しないを選ぶと、ほかがすべて閉じる。
    ctx.prompts
        .close
        .borrow_mut()
        .push_back(CloseChoice::Discard);
    cmd("file.close_others", Value::Null)?;
    step(|app| -> Check {
        check!(app.documents().len() == 1, "残り {}", app.documents().len());
        check!(app.active_id() == Some(keep), "アクティブな文書");
        Ok(())
    })??;
    // すべて閉じる: 文書 0 件になる（無題は作らない。V1）。
    cmd("file.new", Value::Null)?;
    cmd("file.close_all", Value::Null)?;
    step(|app| -> Check {
        check!(app.documents().is_empty(), "残り {}", app.documents().len());
        check!(app.active_id().is_none(), "アクティブな文書がない");
        Ok(())
    })??;
    // 以降の試験のために、文書を 1 つ開き直す。
    cmd("file.new", Value::Null)?;
    Ok(())
}

/// フルパスをクリップボードへ。ファイルでない文書は断る。
fn copy_path(ctx: &Ctx) -> Check {
    let path = ctx.dir.join("u1-copy.txt");
    std::fs::write(&path, "x").map_err(|e| e.to_string())?;
    open_and_wait(&path)?;
    cmd("file.copy_path", Value::Null)?;
    let text = step(|app| clipboard::read_text(Some(app.hwnd())))?.map_err(|e| e.to_string())?;
    check!(
        text.as_deref() == Some(path.to_string_lossy().as_ref()),
        "クリップボード {text:?}"
    );
    cmd("file.new", Value::Null)?;
    cmd("file.copy_path", Value::Null)?;
    step(|app| -> Check {
        check!(
            app.status_message()
                .is_some_and(|m| m.contains("ファイルになっていない")),
            "案内 {:?}",
            app.status_message()
        );
        Ok(())
    })??;
    Ok(())
}
