//! 改善 V1 の自己診断: 無題を作らない起動・終了（開く・受け渡しの途中、開くのに失敗したとき）、
//! 前回のタブの記録と復元（順序・アクティブ・カーソル・存在しないファイルを飛ばす・設定で無効）、
//! ウィンドウの位置の取得と戻し、矩形選択の切り替え。
//! メニューバーのフォーカス（Alt を離したとき）と実際のモニターへの配置は Windows 実機で確認する
//! （`docs/verification/V.md`）。

use std::path::PathBuf;

use serde_json::Value;

use super::integration::{cmd, step, wait};
use super::{Check, Ctx};
use crate::appdata::handoff::{self, Handoff, HandoffDoc};
use crate::appdata::session::Session;
use crate::appdata::WindowSettings;
use crate::core::DocumentId;

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!()));
        }
    };
}

pub(super) fn run(ctx: &Ctx) -> Check {
    no_untitled_when_opening_or_adopting(ctx)?;
    session_round_trip(ctx)?;
    window_placement()?;
    rectangular_select_toggle()?;
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

fn make_file(ctx: &Ctx, name: &str, body: &str) -> Result<PathBuf, String> {
    let p = ctx.dir.join(name);
    std::fs::write(&p, body).map_err(|e| e.to_string())?;
    Ok(p)
}

/// 開く・受け渡しの読み込み中や失敗したときに、無題の文書ができない。
fn no_untitled_when_opening_or_adopting(ctx: &Ctx) -> Check {
    close_all()?;
    // 開くのに失敗（存在しない）: 文書 0 件のまま。
    let missing = ctx.dir.join("v1-missing.txt");
    step(|app| app.open_path(&missing))?;
    step(|app| -> Check {
        check!(app.documents().is_empty(), "失敗した後に文書ができた");
        Ok(())
    })??;

    // 受け渡し（--adopt）: 読み込みは非同期。完了までも、完了後も無題はできない。
    let file = make_file(ctx, "v1-adopt.txt", "adopted\n")?;
    let h = handoff::write(
        &ctx.paths,
        &Handoff::new(HandoffDoc::File { path: file.clone() }),
        701,
    )
    .map_err(super::e2s)?;
    step(|app| app.adopt_handoff(&h))?.map_err(super::e2s)?;
    step(|app| -> Check {
        check!(
            app.documents().iter().all(|d| d.state.path().is_some()),
            "読み込み中に無題ができた"
        );
        Ok(())
    })??;
    wait("受け渡しの文書が開く", |app| {
        app.documents().len() == 1
    })?;
    step(|app| -> Check {
        check!(
            app.documents().len() == 1,
            "文書の数 {}",
            app.documents().len()
        );
        check!(
            app.documents()[0].state.path().is_some(),
            "無題ができている"
        );
        Ok(())
    })??;
    // 閉じると 0 件。
    close_all()?;
    step(|app| -> Check {
        check!(app.documents().is_empty(), "閉じた後に文書がある");
        check!(app.active_id().is_none(), "アクティブな文書");
        Ok(())
    })??;
    Ok(())
}

/// 前回のタブを記録して、開き直す（順序・アクティブ・カーソル）。
fn session_round_trip(ctx: &Ctx) -> Check {
    close_all()?;
    let a = make_file(ctx, "v1-a.txt", "aaaa\nbbbb\ncccc\n")?;
    let b = make_file(ctx, "v1-b.txt", "one\ntwo\nthree\nfour\n")?;
    let c = make_file(ctx, "v1-c.txt", "gone soon\n")?;
    let d = make_file(ctx, "v1-d.txt", "dddd\n")?;
    for p in [&a, &b, &c, &d] {
        let p = p.clone();
        step(|app| app.open_path(&p))?;
        // 順に開く（順序をはっきりさせる）。
        wait("開く", |app| app.opening.is_empty())?;
    }
    wait("4 つ開く", |app| app.documents().len() == 4)?;
    // 無題とクリップボード由来は記録しない。
    step(|app| app.new_untitled().map(|_| ()))?.map_err(super::e2s)?;
    let ids: Vec<DocumentId> = step(|app| app.documents().iter().map(|d| d.id()).collect())?;
    // b にカーソル（3 行目の先頭 = バイト 8）を置き、b をアクティブに。
    let b_id = ids[1];
    step(|app| -> Check {
        app.activate(b_id).map_err(super::e2s)?;
        app.editor()
            .set_selection(crate::document::Selection::caret_at(8));
        Ok(())
    })??;
    step(|app| -> Check {
        let s = app.current_session();
        check!(s.docs.len() == 4, "記録の数 {}", s.docs.len());
        check!(s.active == Some(1), "アクティブ {:?}", s.active);
        check!(s.docs[1].caret == 8, "カーソル {}", s.docs[1].caret);
        check!(
            s.docs[0].path == a || s.docs[0].path.ends_with("v1-a.txt"),
            "順序"
        );
        Ok(())
    })??;
    step(|app| app.save_session_and_window())?;
    let text = std::fs::read_to_string(ctx.paths.session_file()).map_err(|e| e.to_string())?;
    let written = Session::from_json(&text).ok_or("session.json を読めない")?;
    check!(
        written.docs.len() == 4 && written.active == Some(1),
        "書いた記録"
    );

    // 存在しなくなったファイルは黙って飛ばす。
    std::fs::remove_file(&c).map_err(|e| e.to_string())?;
    close_all()?;
    check!(
        step(|app| app.begin_session_restore())? == 3,
        "存在するファイルだけ開く"
    );
    // 起動の経路と同じく、復元の間に無題はできない。
    wait("3 つ開く", |app| app.documents().len() == 3)?;
    step(|app| -> Check {
        let paths: Vec<_> = app
            .documents()
            .iter()
            .map(|d| {
                d.state
                    .path()
                    .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            })
            .collect();
        check!(
            paths
                == [
                    Some("v1-a.txt".into()),
                    Some("v1-b.txt".into()),
                    Some("v1-d.txt".into())
                ],
            "タブの順序 {paths:?}"
        );
        let active = app.active_id().ok_or("アクティブな文書がない")?;
        check!(
            app.document(active).unwrap().title == "v1-b.txt",
            "アクティブなタブ"
        );
        check!(
            app.editor().selection().caret == 8,
            "カーソルの位置 {}",
            app.editor().selection().caret
        );
        Ok(())
    })??;
    // 別のタブへ移っても、そのタブのカーソルは先頭。
    step(|app| -> Check {
        let first = app.documents()[0].id();
        app.activate(first).map_err(super::e2s)?;
        check!(app.editor().selection().caret == 0, "ほかのタブのカーソル");
        Ok(())
    })??;

    // 設定で無効にできる。
    close_all()?;
    step(|app| app.settings.restore_session = false)?;
    check!(
        step(|app| app.begin_session_restore())? == 0,
        "無効のときは開かない"
    );
    step(|app| app.settings.restore_session = true)?;
    // 記録が壊れていても何もしない（起動は 0 件のまま）。
    std::fs::write(ctx.paths.session_file(), "{ broken").map_err(|e| e.to_string())?;
    check!(step(|app| app.begin_session_restore())? == 0, "壊れた記録");
    step(|app| -> Check {
        check!(app.documents().is_empty(), "壊れた記録で文書ができた");
        Ok(())
    })??;
    let _ = std::fs::remove_file(ctx.paths.session_file());
    // 以降の試験のために文書を 1 つ開く。
    cmd("file.new", Value::Null)?;
    Ok(())
}

/// ウィンドウの位置の取得と戻し（画面外の記録は作業領域に収まる）。
fn window_placement() -> Check {
    step(|app| -> Check {
        let now = app.window_settings().ok_or("位置を取れない")?;
        check!(now.width > 0 && now.height > 0, "位置 {now:?}");
        // 保存した位置を戻して、取り直すと同じ大きさになる（作業領域に収まる範囲）。
        let target = WindowSettings {
            x: now.x,
            y: now.y,
            width: now.width.min(900),
            height: now.height.min(600),
            maximized: false,
        };
        app.settings.window = Some(target);
        check!(app.apply_saved_window(), "位置を戻せない");
        let back = app.window_settings().ok_or("位置を取れない 2")?;
        check!(
            back.width == target.width && back.height == target.height && !back.maximized,
            "戻した位置 {back:?} / {target:?}"
        );
        // 画面外の記録でもウィンドウは作業領域の内側に来る。
        app.settings.window = Some(WindowSettings {
            x: 90_000,
            y: 90_000,
            width: 700,
            height: 500,
            maximized: false,
        });
        check!(app.apply_saved_window(), "画面外の位置を戻せない");
        let moved = app.window_settings().ok_or("位置を取れない 3")?;
        check!(
            moved.x < 90_000 && moved.y < 90_000,
            "画面外のまま {moved:?}"
        );
        app.settings.window = None;
        Ok(())
    })?
}

/// 矩形選択の切り替え（編集 > 矩形選択）。
fn rectangular_select_toggle() -> Check {
    cmd("edit.rectangular_select", Value::Null)?;
    step(|app| -> Check {
        check!(
            app.editor().rectangular_select_active(),
            "矩形選択がオンにならない"
        );
        check!(
            app.view_menu_checked(
                crate::command::CommandId::new("edit.rectangular_select"),
                &Value::Null
            ) == Some(true),
            "メニューのチェック"
        );
        Ok(())
    })??;
    cmd("edit.rectangular_select", Value::Null)?;
    step(|app| -> Check {
        check!(
            !app.editor().rectangular_select_active(),
            "矩形選択がオフにならない"
        );
        check!(
            !app.editor().is_rectangular_selection(),
            "通常の選択に戻らない"
        );
        Ok(())
    })??;
    Ok(())
}
