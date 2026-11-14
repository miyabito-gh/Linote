//! 改善 U4 の自己診断: タブの並べ替え（コマンド相当の `move_tab` と、マウスメッセージによる
//! ドラッグ）、右クリックメニューのコマンド（右側を閉じる ほか）、別ウィンドウへの移動の
//! 受け渡し（送り側が受け渡しファイルを書き、受け側の処理を同じプロセスで実行して、
//! 受け取りの確認で元のタブが閉じること。失敗・時間切れ・編集の競合では元のタブが残ること）。
//! 新しいプロセスの起動と、画面をまたぐドラッグ、置き場所は Windows 実機で確認する
//! （`docs/verification/U.md`）。

use std::time::{Duration, Instant};

use serde_json::json;
use windows::Win32::Foundation::{LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageW, SetCursorPos, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};

use super::integration::{cmd, step, wait};
use super::{text_of, Check, Ctx};
use crate::appdata::handoff::{self, Handoff, HandoffDoc, TextDoc, TextKind};
use crate::command::{CommandId, Invocation, Value};
use crate::core::{DocumentId, TargetRef};
use crate::document::DocumentKind;
use crate::memo::MemoStore;
use crate::text::{Encoding, Eol, TextFormat};
use crate::ui::dialogs::CloseChoice;
use crate::ui::tabs::TAB_MENU;

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!()));
        }
    };
}

pub(super) fn run(ctx: &Ctx) -> Check {
    step(|app| app.handoff_spawn = false)?;
    let result = (|| {
        menu_and_commands()?;
        reorder()?;
        drag_with_mouse_messages()?;
        close_right(ctx)?;
        handoff_unsaved_text(ctx)?;
        handoff_file_and_memo(ctx)?;
        handoff_failures(ctx)?;
        adopt_failures(ctx)?;
        Ok(())
    })();
    step(|app| app.handoff_spawn = true)?;
    result
}

fn close_everything() -> Check {
    step(|app| {
        let ids: Vec<_> = app.documents().iter().map(|d| d.id()).collect();
        for id in ids {
            let _ = app.close_document(id, true);
        }
        // 前の試験の受け渡しが残っていたら捨てる。
        app.handoffs.clear();
    })
}

fn new_docs(n: usize, texts: &[&str]) -> Result<Vec<DocumentId>, String> {
    close_everything()?;
    // 最後の 1 つを閉じると空の文書が残るので、足りない分だけ作る。
    let have = step(|app| app.documents().len())?;
    for _ in have..n {
        cmd("file.new", Value::Null)?;
    }
    let ids: Vec<DocumentId> = step(|app| app.documents().iter().map(|d| d.id()).collect())?;
    for (id, text) in ids.iter().zip(texts) {
        step(|app| app.with_text(*id, |ed| ed.set_text(text)))?.map_err(super::e2s)?;
    }
    Ok(ids)
}

fn order() -> Result<Vec<DocumentId>, String> {
    step(|app| app.documents().iter().map(|d| d.id()).collect())
}

fn menu_and_commands() -> Check {
    step(|app| -> Check {
        let entries = app.menu().ok_or("メニューがない")?.entries().to_vec();
        for command in ["file.close_right", "file.move_to_new_window"] {
            check!(
                entries.iter().any(|e| e.command == CommandId::new(command)),
                "メニューに {command} がない"
            );
        }
        // 右クリックメニューのコマンドはすべて登録されている。
        for (command, _) in TAB_MENU.iter().flatten() {
            check!(
                app.registry().find(command).is_some(),
                "右クリックメニューの {command} が未登録"
            );
        }
        Ok(())
    })?
}

fn reorder() -> Check {
    let ids = new_docs(4, &["a", "b", "c", "d"])?;
    // 先頭のタブをアクティブにして、2 番目へ動かす。
    step(|app| app.activate(ids[0]))?.map_err(super::e2s)?;
    step(|app| -> Check {
        check!(app.move_tab(ids[0], 2), "動かせない");
        Ok(())
    })??;
    check!(
        order()? == [ids[1], ids[2], ids[0], ids[3]],
        "並び {:?}",
        order()?
    );
    step(|app| -> Check {
        check!(
            app.active_id() == Some(ids[0]),
            "アクティブなタブが変わった"
        );
        check!(app.active == Some(2), "アクティブな番号 {:?}", app.active);
        let titles: Vec<_> = app.tabs.items().iter().map(|t| t.id).collect();
        check!(titles == order_now(app), "タブ表示の並び");
        check!(text_of(app, ids[0]) == "a", "本文が変わった");
        // アクティブでないタブを動かしても、アクティブなタブは変わらない。
        check!(app.move_tab(ids[3], 0), "動かせない 2");
        check!(app.active_id() == Some(ids[0]), "アクティブなタブ 2");
        check!(app.active == Some(3), "アクティブな番号 2 {:?}", app.active);
        // 同じ場所・範囲外は何も起きない。
        check!(!app.move_tab(ids[3], 0), "同じ場所");
        check!(!app.move_tab(ids[3], 9), "範囲外");
        Ok(())
    })??;
    check!(
        order()? == [ids[3], ids[1], ids[2], ids[0]],
        "並び 2 {:?}",
        order()?
    );
    // 保存の状態（未保存の丸）は文書に付いてくる。
    step(|app| -> Check {
        let dirty: Vec<_> = app.tabs.items().iter().map(|t| t.dirty).collect();
        check!(dirty.iter().all(|&d| d), "未保存の表示 {dirty:?}");
        Ok(())
    })?
}

fn order_now(app: &crate::ui::app::App) -> Vec<DocumentId> {
    app.documents().iter().map(|d| d.id()).collect()
}

fn lparam(x: i32, y: i32) -> LPARAM {
    LPARAM((((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF)) as isize)
}

/// マウスのメッセージでタブをドラッグして並べ替える。離した位置（カーソルの実位置）が
/// ウィンドウの中なら、外へ出したことにはならない。
fn drag_with_mouse_messages() -> Check {
    let ids = new_docs(3, &["x", "y", "z"])?;
    let (hwnd, from, to, screen) = step(|app| -> Result<_, String> {
        app.relayout();
        let r0 = app.tabs.tab_rect(0).ok_or("タブ 0 の矩形がない")?;
        let r2 = app.tabs.tab_rect(2).ok_or("タブ 2 の矩形がない")?;
        let y = (r0.top + r0.bottom) / 2;
        let from = ((r0.left + r0.right) / 2, y);
        let to = (r2.right - 5, y);
        let mut p = POINT { x: to.0, y: to.1 };
        // SAFETY: 出力先は POINT 1 つ。
        unsafe {
            let _ = ClientToScreen(app.hwnd(), &mut p);
        }
        Ok((app.hwnd(), from, to, (p.x, p.y)))
    })??;
    // SAFETY: 自分のウィンドウへの同期メッセージ。カーソルの実位置はウィンドウの中に置く。
    unsafe {
        let _ = SetCursorPos(screen.0, screen.1);
        SendMessageW(
            hwnd,
            WM_LBUTTONDOWN,
            Some(WPARAM(1)),
            Some(lparam(from.0, from.1)),
        );
    }
    // 少し動いただけ（しきい値の内側）では動かない。
    unsafe {
        SendMessageW(
            hwnd,
            WM_MOUSEMOVE,
            Some(WPARAM(1)),
            Some(lparam(from.0 + 2, from.1)),
        );
    }
    check!(order()? == ids, "しきい値の内側で動いた");
    unsafe {
        SendMessageW(
            hwnd,
            WM_MOUSEMOVE,
            Some(WPARAM(1)),
            Some(lparam(to.0, to.1)),
        );
        SendMessageW(
            hwnd,
            WM_LBUTTONUP,
            Some(WPARAM(0)),
            Some(lparam(to.0, to.1)),
        );
    }
    check!(
        order()? == [ids[1], ids[2], ids[0]],
        "ドラッグ後の並び {:?}",
        order()?
    );
    step(|app| -> Check {
        check!(
            app.active_id() == Some(ids[0]),
            "ドラッグしたタブがアクティブ"
        );
        check!(app.tab_drag.is_none(), "ドラッグの状態が残った");
        check!(
            app.handoffs.is_empty(),
            "ウィンドウの中なのに移動が始まった"
        );
        Ok(())
    })??;
    // 単なるクリックでは並びは変わらず、アクティブになるだけ。
    let click = step(|app| {
        let r = app.tabs.tab_rect(0).unwrap();
        ((r.left + r.right) / 2, (r.top + r.bottom) / 2)
    })?;
    unsafe {
        SendMessageW(
            hwnd,
            WM_LBUTTONDOWN,
            Some(WPARAM(1)),
            Some(lparam(click.0, click.1)),
        );
        SendMessageW(
            hwnd,
            WM_LBUTTONUP,
            Some(WPARAM(0)),
            Some(lparam(click.0, click.1)),
        );
    }
    check!(
        order()? == [ids[1], ids[2], ids[0]],
        "クリックで並びが変わった"
    );
    step(|app| -> Check {
        check!(app.active_id() == Some(ids[1]), "クリックでアクティブ");
        Ok(())
    })??;
    Ok(())
}

/// 右側のタブを閉じる（未保存は確認し、取り消すとそこで止まる）。
fn close_right(ctx: &Ctx) -> Check {
    let ids = new_docs(4, &["a", "b", "c", "d"])?;
    ctx.prompts.close.borrow_mut().clear();
    step(|app| app.activate(ids[1]))?.map_err(super::e2s)?;
    // 取り消す: 最初の確認（ids[2]）で止まる。
    ctx.prompts
        .close
        .borrow_mut()
        .push_back(CloseChoice::Cancel);
    cmd("file.close_right", Value::Null)?;
    check!(order()? == ids, "取り消したのに閉じた {:?}", order()?);
    step(|app| -> Check {
        check!(app.active_id() == Some(ids[1]), "アクティブなタブ");
        Ok(())
    })??;
    // 保存しない: 右側がすべて閉じ、対象のタブは残ってアクティブ。
    ctx.prompts
        .close
        .borrow_mut()
        .push_back(CloseChoice::Discard);
    ctx.prompts
        .close
        .borrow_mut()
        .push_back(CloseChoice::Discard);
    cmd("file.close_right", Value::Null)?;
    check!(
        order()? == [ids[0], ids[1]],
        "右側を閉じた後 {:?}",
        order()?
    );
    step(|app| -> Check {
        check!(app.active_id() == Some(ids[1]), "アクティブなタブ 2");
        Ok(())
    })??;
    // 一番右では何も閉じない。
    cmd("file.close_right", Value::Null)?;
    check!(order()?.len() == 2, "一番右で閉じた");
    Ok(())
}

fn move_to_new_window(id: DocumentId) -> Result<(), String> {
    step(|app| {
        let inv = Invocation::user(CommandId::new("file.move_to_new_window"))
            .with_target(TargetRef::Document(id), None)
            .with_args(json!({ "position": "-1500,40" }));
        app.execute(&inv).map(|_| ()).map_err(super::e2s)
    })?
}

fn pending_file() -> Result<std::path::PathBuf, String> {
    step(|app| app.handoffs.last().map(|h| h.file.clone()))?.ok_or("移動の待ちがない".to_owned())
}

fn wait_recovery_written() -> Check {
    wait("復元データの書き出し", |app| {
        app.documents().iter().all(|d| !d.write_in_flight)
    })
}

/// 未保存の本文を渡す: 文字コード・改行・未保存の状態が引き継がれ、受け取りの確認の後で
/// 元のタブが閉じる。受け側は復元データを書く。
fn handoff_unsaved_text(ctx: &Ctx) -> Check {
    let ids = new_docs(2, &["あいう\r\nえお\r\n", "other"])?;
    let a = ids[0];
    step(|app| {
        app.update_state(a, |s| {
            s.format = TextFormat {
                encoding: Encoding::Cp932,
                bom: false,
                eol: Eol::CrLf,
            };
        })
    })?
    .map_err(super::e2s)?;
    move_to_new_window(a)?;
    let file = pending_file()?;
    check!(
        file.starts_with(ctx.paths.handoff_dir()),
        "置き場所 {file:?}"
    );
    let written = handoff::read(&file, handoff::now_ms()).map_err(super::e2s)?;
    let HandoffDoc::Text(t) = &written.doc else {
        return Err(format!("本文つきの受け渡しではない: {written:?}"));
    };
    check!(
        t.kind == TextKind::Untitled && t.dirty && t.path.is_none(),
        "種類 {t:?}"
    );
    check!(t.text == "あいう\r\nえお\r\n", "本文 {:?}", t.text);
    check!(t.format.encoding == Encoding::Cp932, "文字コード");
    // 受け取り前は、元のタブは残る。
    step(|app| app.handoff_tick())?;
    check!(order()?.contains(&a), "確認の前に閉じた");
    step(|app| -> Check {
        check!(app.handoffs.len() == 1, "待ちが消えた");
        Ok(())
    })??;
    // 同じ文書をもう一度移そうとすると断る。
    step(|app| {
        let _ = app.activate(a);
        app.run_command(CommandId::new("file.move_to_new_window"), Value::Null);
        check!(app.handoffs.len() == 1, "二重に移動を始めた");
        Ok::<(), String>(())
    })??;
    // 受け側（同じプロセスで実行する）。
    let before = order()?;
    step(|app| app.adopt_handoff(&file))?.map_err(super::e2s)?;
    check!(!file.exists(), "受け取り後にファイルが残った");
    let adopted = order()?
        .into_iter()
        .find(|id| !before.contains(id))
        .ok_or("受け取った文書がない")?;
    step(|app| -> Check {
        let st = app.state(adopted).unwrap();
        check!(*st.kind() == DocumentKind::Untitled, "種類");
        check!(st.is_dirty(), "未保存として開いていない");
        check!(
            st.format.encoding == Encoding::Cp932,
            "受け取った文字コード"
        );
        check!(st.format.eol == Eol::CrLf, "受け取った改行");
        check!(
            text_of(app, adopted) == "あいう\r\nえお\r\n",
            "受け取った本文"
        );
        let i = app.index_of(adopted).unwrap();
        check!(app.docs[i].slot.is_some(), "復元データの枠がない");
        Ok(())
    })??;
    wait_recovery_written()?;
    let recovered = std::fs::read_dir(ctx.paths.recovery_dir())
        .map(|d| d.count())
        .unwrap_or(0);
    check!(recovered > 0, "復元データが書かれていない");
    // 見張りが、受け取りを確認して元のタブを閉じる。
    step(|app| app.handoff_tick())?;
    check!(!order()?.contains(&a), "確認できたのに元のタブが残った");
    step(|app| -> Check {
        check!(app.handoffs.is_empty(), "待ちが残った");
        check!(!app.handoff_timer, "タイマーが残った");
        Ok(())
    })??;
    // 取り込んだ文書は、閉じるときに未保存の確認が出る（未保存として扱っている）。
    Ok(())
}

/// 保存済みのファイルはパスだけ、メモは保存領域から。ファイルの有無で確認する。
fn handoff_file_and_memo(ctx: &Ctx) -> Check {
    // 保存済みのファイル。
    let path = ctx.dir.join("u4-saved.txt");
    std::fs::write(&path, "saved\n").map_err(|e| e.to_string())?;
    close_everything()?;
    cmd("file.new", Value::Null)?;
    let id = super::integration::open_and_wait(&path)?;
    move_to_new_window(id)?;
    let file = pending_file()?;
    let written = handoff::read(&file, handoff::now_ms()).map_err(super::e2s)?;
    check!(
        written.doc == HandoffDoc::File { path: path.clone() },
        "ファイルの受け渡し {written:?}"
    );
    // 受け側が先に claim した（ファイルが消えた）ので、元のタブが閉じる。
    check!(handoff::claim(&file), "claim できない");
    step(|app| app.handoff_tick())?;
    check!(!order()?.contains(&id), "元のタブが残った");

    // 受け側: パスから開く。
    close_everything()?;
    cmd("file.new", Value::Null)?;
    let paths = ctx.paths.clone();
    let file = handoff::write(
        &paths,
        &Handoff::new(HandoffDoc::File { path: path.clone() }),
        90,
    )
    .map_err(super::e2s)?;
    step(|app| app.adopt_handoff(&file))?.map_err(super::e2s)?;
    check!(!file.exists(), "ファイルが残った");
    wait("受け取ったファイルが開く", |app| {
        app.documents()
            .iter()
            .any(|d| d.state.path() == Some(path.as_path()))
    })?;

    // メモ。
    close_everything()?;
    cmd("file.new", Value::Null)?;
    let store = MemoStore::new(&ctx.paths);
    let memo = store.create("u4 メモ", "memo body\n").map_err(super::e2s)?;
    let mid = step(|app| app.open_memo(memo.clone()))?.map_err(super::e2s)?;
    step(|app| app.with_text(mid, |ed| ed.set_text("memo body\nedited\n")))?.map_err(super::e2s)?;
    move_to_new_window(mid)?;
    let file = pending_file()?;
    let written = handoff::read(&file, handoff::now_ms()).map_err(super::e2s)?;
    check!(
        matches!(&written.doc, HandoffDoc::Memo { id } if *id == memo.id),
        "メモの受け渡し {written:?}"
    );
    // 編集中の内容も、渡す前に保存領域へ書かれている。
    check!(
        store.load(&memo.id).map_err(super::e2s)?.body == "memo body\nedited\n",
        "メモが保存されていない"
    );
    check!(handoff::claim(&file), "claim できない 2");
    step(|app| app.handoff_tick())?;
    check!(!order()?.contains(&mid), "メモのタブが残った");
    // 受け側: 同じメモを開く。
    let file = handoff::write(
        &paths,
        &Handoff::new(HandoffDoc::Memo {
            id: memo.id.clone(),
        }),
        91,
    )
    .map_err(super::e2s)?;
    step(|app| app.adopt_handoff(&file))?.map_err(super::e2s)?;
    step(|app| -> Check {
        let opened = app
            .docs
            .iter()
            .find(|d| matches!(d.state.kind(), DocumentKind::Memo(m) if *m == memo.id));
        let d = opened.ok_or("メモが開いていない")?;
        check!(!d.state.is_dirty(), "メモが未保存になっている");
        let id = d.id();
        check!(text_of(app, id) == "memo body\nedited\n", "メモの本文");
        Ok(())
    })??;
    store
        .delete(&memo.id, store.load(&memo.id).map_err(super::e2s)?.version)
        .ok();
    Ok(())
}

/// 送り側の失敗: 時間切れ、編集の競合、1 つだけのタブ、起動の失敗では元のタブを残す。
fn handoff_failures(ctx: &Ctx) -> Check {
    // 1 つだけのタブは移せない。
    close_everything()?;
    let only = order()?[0];
    check!(order()?.len() == 1, "文書の数");
    check!(move_to_new_window(only).is_err(), "1 つだけのタブを移せた");
    step(|app| -> Check {
        check!(app.handoffs.is_empty(), "待ちができた");
        Ok(())
    })??;

    // 時間切れ: 元のタブとその内容が残り、ファイルは取り消され、遅れて起動した受け側は
    // 取り込めない。
    let ids = new_docs(2, &["keep me", "x"])?;
    ctx.prompts.shown.borrow_mut().clear();
    move_to_new_window(ids[0])?;
    let file = pending_file()?;
    step(|app| {
        app.handoffs[0].deadline = Instant::now() - Duration::from_secs(1);
        app.handoff_tick();
    })?;
    check!(order()? == ids, "時間切れで閉じた");
    check!(!file.exists(), "取り消したのにファイルが残った");
    check!(
        ctx.prompts
            .shown
            .borrow()
            .iter()
            .any(|s| s.contains("移せませんでした")),
        "失敗を知らせていない {:?}",
        ctx.prompts.shown.borrow()
    );
    let late = step(|app| app.adopt_handoff(&file))?;
    check!(late.is_err(), "取り消し後に受け取れた");
    step(|app| -> Check {
        check!(app.handoffs.is_empty(), "待ちが残った");
        Ok(())
    })??;
    step(|app| -> Check {
        check!(!app.handoff_timer, "タイマーが残った");
        Ok(())
    })??;
    step(|app| -> Check {
        check!(text_of(app, ids[0]) == "keep me", "本文が変わった");
        Ok(())
    })??;

    // 受け側が先に取った後で時間切れになっても、取り消せず受け取り済みとして扱う。
    move_to_new_window(ids[0])?;
    let file = pending_file()?;
    check!(handoff::claim(&file), "claim できない");
    step(|app| {
        app.handoffs[0].deadline = Instant::now() - Duration::from_secs(1);
        app.handoff_tick();
    })?;
    check!(!order()?.contains(&ids[0]), "受け取り済みなのに残った");

    // 待っている間に元の文書を編集したら、閉じずに残す。
    let ids = new_docs(2, &["v1", "x"])?;
    ctx.prompts.shown.borrow_mut().clear();
    move_to_new_window(ids[0])?;
    let file = pending_file()?;
    step(|app| app.with_text(ids[0], |ed| ed.set_text("v2 edited after handoff")))?
        .map_err(super::e2s)?;
    check!(handoff::claim(&file), "claim できない");
    step(|app| app.handoff_tick())?;
    check!(order()?.contains(&ids[0]), "編集した元のタブが閉じた");
    step(|app| -> Check {
        check!(
            text_of(app, ids[0]) == "v2 edited after handoff",
            "編集内容"
        );
        Ok(())
    })??;
    check!(
        ctx.prompts
            .shown
            .borrow()
            .iter()
            .any(|s| s.contains("残しました")),
        "知らせていない"
    );

    // 読み込みで不正なバイトを置き換えた文書は移さない。
    let bad = ctx.dir.join("u4-lossy.txt");
    std::fs::write(&bad, b"ok \xFF\xFE\xFD bytes\n").map_err(|e| e.to_string())?;
    close_everything()?;
    cmd("file.new", Value::Null)?;
    let _ = super::integration::open_and_wait(&bad)?;
    let lossy = step(|app| app.docs.iter().find(|d| d.lossy).map(|d| d.id()))?;
    if let Some(l) = lossy {
        step(|app| app.with_text(l, |ed| ed.set_text("changed")))?.map_err(super::e2s)?;
        check!(
            move_to_new_window(l).is_err(),
            "不正なバイトを含む文書を移せた"
        );
    }
    Ok(())
}

/// 受け側の失敗: 壊れた・古い・未対応の版のファイルは何も取り込まず、ファイルにも触れない。
fn adopt_failures(ctx: &Ctx) -> Check {
    let _ = new_docs(1, &["base"])?;
    let dir = ctx.paths.handoff_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let before = order()?;
    let try_adopt = |name: &str, bytes: &[u8]| -> Result<(), String> {
        let f = dir.join(name);
        std::fs::write(&f, bytes).map_err(|e| e.to_string())?;
        let r = step(|app| app.adopt_handoff(&f))?;
        check!(r.is_err(), "{name} を受け取れた");
        check!(f.exists(), "{name} を消した（受け取っていないのに）");
        check!(order()? == before, "{name} で文書が増えた");
        let _ = std::fs::remove_file(&f);
        Ok(())
    };
    try_adopt("handoff-0-1-1.json", b"{broken")?;
    try_adopt("handoff-0-2-1.json", b"")?;
    try_adopt(
        "handoff-0-3-1.json",
        br#"{"version":99,"created_ms":1,"kind":"file","path":"x"}"#,
    )?;
    let mut stale = Handoff::new(HandoffDoc::Text(TextDoc {
        kind: TextKind::Untitled,
        path: None,
        title: "古い".into(),
        format: TextFormat::DEFAULT,
        dirty: true,
        disk: None,
        text: "stale".into(),
    }));
    stale.created_ms = 1;
    try_adopt("handoff-0-4-1.json", &stale.encode())?;
    // 存在しないファイル・メモを指す受け渡し。
    let gone = Handoff::new(HandoffDoc::File {
        path: ctx.dir.join("no-such-u4.txt"),
    });
    try_adopt("handoff-0-5-1.json", &gone.encode())?;
    let nomemo = Handoff::new(HandoffDoc::Memo {
        id: crate::core::MemoId::parse("no-such-memo").unwrap(),
    });
    try_adopt("handoff-0-6-1.json", &nomemo.encode())?;
    Ok(())
}
