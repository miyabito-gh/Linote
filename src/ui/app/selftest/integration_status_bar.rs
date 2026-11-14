//! Status visibility is global, persists, and releases layout space without losing information.
use super::integration::{cmd, step, wait};
use super::Check;
use crate::command::Value;
use crate::ui::statusbar::MessageKind;

pub(super) fn run() -> Check {
    // The command must remain available with no documents.
    step(|app| {
        let ids = app.documents().iter().map(|d| d.id()).collect::<Vec<_>>();
        for id in ids {
            let _ = app.close_document(id, true);
        }
    })?;
    cmd("view.status_bar", serde_json::json!({ "enabled": true }))?;
    let (bottom, height, info) = step(|app| {
        (
            app.layout.editor.bottom,
            app.layout.status.bottom - app.layout.status.top,
            app.status.info().clone(),
        )
    })?;
    if height <= 0 {
        return Err("表示中のステータスバーに高さがない".into());
    }
    cmd("view.status_bar", Value::Null)?;
    step(|app| -> Check {
        if app.settings.view.status_bar || app.layout.status.top != app.layout.status.bottom {
            return Err("ステータスバーが非表示にならない".into());
        }
        if app.layout.editor.bottom != bottom + height || *app.status.info() != info {
            return Err("非表示時の本文領域または保持した情報が違う".into());
        }
        app.set_status("非表示中の通知", MessageKind::Warning);
        Ok(())
    })??;
    wait("ステータス表示設定の保存", |app| {
        app.settings_writes_pending == 0
    })?;
    step(|app| -> Check {
        let saved = super::super::fileio::load_settings(app.paths.as_ref()).0;
        if saved.view.status_bar {
            return Err("非表示設定が保存されない".into());
        }
        // Modify the instance without saving; reload must recover the stored flag.
        app.settings.view.status_bar = true;
        app.apply_view_settings();
        Ok(())
    })??;
    cmd("app.reload_settings", Value::Null)?;
    step(|app| -> Check {
        if app.settings.view.status_bar || app.layout.status.top != app.layout.status.bottom {
            return Err("再読み込みでステータスバー非表示が復元されない".into());
        }
        app.set_status("再表示する通知", MessageKind::Warning);
        Ok(())
    })??;
    cmd("view.status_bar", Value::Null)?;
    step(|app| -> Check {
        if !app.settings.view.status_bar || app.layout.editor.bottom != bottom {
            return Err("ステータスバーの再表示".into());
        }
        if app.status_message() != Some("再表示する通知") {
            return Err("再表示で非表示中の通知を失った".into());
        }
        if app.view_menu_checked(
            crate::command::CommandId::new("view.status_bar"),
            &Value::Null,
        ) != Some(true)
        {
            return Err("表示メニューのステータスバーのチェック".into());
        }
        Ok(())
    })??;
    Ok(())
}
