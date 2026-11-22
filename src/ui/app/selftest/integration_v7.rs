//! V7: 外部変更の非同期読み直しと編集の保護、最近使ったファイルの消去。
use super::integration::{cmd, open_and_wait, step, wait};
use super::{text_of, Check, Ctx};
use crate::appdata::settings::ExternalChangeMode;
use crate::command::Value;
use crate::document::Selection;
use crate::text::Encoding;

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond { return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!())); }
    };
}

pub(super) fn run(ctx: &Ctx) -> Check {
    automatic_reload(ctx)?;
    recent_and_settings()?;
    Ok(())
}

fn automatic_reload(ctx: &Ctx) -> Check {
    let path = ctx.dir.join("v7-auto.txt");
    // CP932 の「あ」で文字コードを確定。読み直しでは、ASCII だけになっても保持する。
    std::fs::write(&path, b"\x82\xa0\r\nlong line\r\nlast\r\n").map_err(|e| e.to_string())?;
    let id = open_and_wait(&path)?;
    let other = step(|app| app.new_untitled())?.map_err(super::e2s)?;
    step(|app| -> Check {
        app.settings.external_change = ExternalChangeMode::ReloadIfClean;
        app.activate(id).map_err(super::e2s)?;
        app.editor().set_selection(Selection::caret_at(8));
        Ok(())
    })??;
    let order = step(|app| app.documents().iter().map(|d| d.id()).collect::<Vec<_>>())?;
    std::fs::write(&path, b"x\r\ny\r\n").map_err(|e| e.to_string())?;
    step(|app| app.check_external_change_of(id))?;
    wait("V7 自動の読み直し", |app| {
        !app.state(id).unwrap().has_external_change()
    })?;
    step(|app| -> Check {
        check!(text_of(app, id) == "x\r\ny\r\n", "新しい本文");
        check!(
            app.documents().iter().map(|d| d.id()).collect::<Vec<_>>() == order,
            "タブ順または ID が変わった"
        );
        check!(
            app.active_id() == Some(id),
            "読み直しでアクティブタブが変わった"
        );
        check!(
            app.state(id).unwrap().format.encoding == Encoding::Cp932,
            "文字コードが自動判定に変わった"
        );
        check!(!app.state(id).unwrap().is_dirty(), "読み直し後は保存済み");
        check!(
            app.editor().selection().caret == 4,
            "短くなった行への位置補正"
        );
        check!(
            app.status_message()
                .is_some_and(|s| s.contains("ファイルの変更を反映しました")),
            "読み直しの通知"
        );
        Ok(())
    })??;
    // 同じ UI 借用内で読み直しを開始して編集すれば、worker が速くても反映前の版違いを作れる。
    std::fs::write(&path, b"changed again\r\n").map_err(|e| e.to_string())?;
    step(|app| -> Check {
        app.check_external_change_of(id);
        app.editor().replace_selection_text("editing");
        Ok(())
    })??;
    wait("V7 編集と読み直しの競合", |app| {
        app.opening.is_empty()
    })?;
    step(|app| -> Check {
        check!(
            text_of(app, id).contains("editing"),
            "読み込み中の編集が失われた"
        );
        check!(
            app.state(id).unwrap().is_dirty() && app.state(id).unwrap().has_external_change(),
            "競合が通知に切り替わらなかった"
        );
        Ok(())
    })??;
    // 初めから未保存なら自動では読み直さない。
    let dirty_path = ctx.dir.join("v7-dirty.txt");
    std::fs::write(&dirty_path, "original").map_err(|e| e.to_string())?;
    let dirty = open_and_wait(&dirty_path)?;
    step(|app| app.editor().replace_selection_text("mine"))?;
    std::fs::write(&dirty_path, "changed on disk").map_err(|e| e.to_string())?;
    step(|app| -> Check {
        app.check_external_change_of(dirty);
        check!(
            text_of(app, dirty).contains("mine"),
            "未保存の文書が置き換えられた"
        );
        check!(
            app.state(dirty).unwrap().has_external_change(),
            "未保存の外部変更の印"
        );
        check!(app.opening.is_empty(), "未保存なのに読み込みが始まった");
        Ok(())
    })??;
    // 削除・移動されたときは、保存済みでも通知する。
    let removed_path = ctx.dir.join("v7-removed.txt");
    std::fs::write(&removed_path, "keep me").map_err(|e| e.to_string())?;
    let removed = open_and_wait(&removed_path)?;
    std::fs::remove_file(&removed_path).map_err(|e| e.to_string())?;
    step(|app| -> Check {
        app.check_external_change_of(removed);
        check!(text_of(app, removed) == "keep me", "削除で本文が失われた");
        check!(
            app.state(removed).unwrap().has_external_change(),
            "削除の通知"
        );
        app.settings.external_change = ExternalChangeMode::Notify;
        for doc in [id, other, dirty, removed] {
            let _ = app.close_document(doc, true);
        }
        Ok(())
    })??;
    Ok(())
}

fn recent_and_settings() -> Check {
    wait("設定の書き出し", |app| {
        app.settings_writes_pending == 0
    })?;
    step(|app| {
        app.settings.recent_files_max = 0;
        app.settings.push_recent("C:\\not-recorded.txt".into());
    })?;
    step(|app| -> Check {
        check!(
            app.settings.recent_files.is_empty(),
            "件数 0 でも記録された"
        );
        app.settings.recent_files_max = 30;
        for n in 0..40 {
            app.settings.push_recent(format!("C:\\v7-{n}.txt"));
        }
        check!(app.settings.recent_files.len() == 30, "30 件の上限");
        check_recent_menu(app, 30)?;
        app.persist_settings_now();
        Ok(())
    })??;
    cmd("file.clear_recent", Value::Null)?;
    wait("最近の一覧の消去を保存", |app| {
        app.settings_writes_pending == 0
    })?;
    step(|app| -> Check {
        check!(app.settings.recent_files.is_empty(), "一覧を消去できない");
        check_recent_menu(app, 0)?;
        let saved = super::super::fileio::load_settings(app.paths.as_ref()).0;
        check!(saved.recent_files.is_empty(), "消去が保存されない");
        app.settings.recent_files_max = 10;
        let items = crate::appdata::settings::pages()
            .iter()
            .flat_map(|p| p.items)
            .collect::<Vec<_>>();
        check!(
            items.iter().any(|i| i.id == "external_change")
                && items.iter().any(|i| i.id == "recent_files_max"),
            "設定画面の項目"
        );
        Ok(())
    })??;
    Ok(())
}

/// 履歴の実件数だけが並び、空なら消去も無効になることを実メニューで確認する。
fn check_recent_menu(app: &mut super::super::App, count: i32) -> Check {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetMenuItemCount, GetMenuItemID, GetMenuState, MF_BYCOMMAND, MF_GRAYED,
    };
    let menu = app.menu().ok_or("メニューがない")?;
    let id = menu
        .entries()
        .iter()
        .find(|e| e.command.as_str() == "file.clear_recent")
        .ok_or("履歴消去の項目がない")?
        .id;
    let popups = menu.popups();
    for popup in popups {
        // SAFETY: 直接の子項目を調べる。GetMenuState は子メニューも再帰的に探すため使わない。
        let owns_command = unsafe {
            (0..GetMenuItemCount(Some(popup))).any(|n| GetMenuItemID(popup, n) == id as u32)
        };
        if !owns_command {
            continue;
        }
        app.on_init_menu_popup(popup);
        // SAFETY: 自分のメニューの項目数と有効状態を読む。
        unsafe {
            check!(
                GetMenuItemCount(Some(popup)) == count.max(1) + 2,
                "履歴の実件数とメニューが違う: expected {}, actual {}",
                count.max(1) + 2,
                GetMenuItemCount(Some(popup))
            );
            let disabled = GetMenuState(popup, id as u32, MF_BYCOMMAND) & MF_GRAYED.0 != 0;
            check!(disabled == (count == 0), "履歴消去の有効状態");
        }
        return Ok(());
    }
    Err("履歴メニューがない".into())
}
