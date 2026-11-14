//! 統合 I1 の自己診断: 開く→編集→保存、文字コードと改行の保持、変換できない文字、外部変更、
//! 閉じる前の確認、復元、メモ、クリップボード、メニューとキー割り当て、設定。
//!
//! 開く・保存・復元の書き出しはワーカーで行うので、各段階を「始める（アプリを借りる）→
//! 待つ（借りずにメッセージを回す）→確かめる（借りる）」に分けて進める。
//! 確認ダイアログは [`ScriptedPrompts`](crate::ui::dialogs::ScriptedPrompts) の台本で答える。

use std::path::{Path, PathBuf};

use serde_json::json;

use super::{pump_until, text_of, Check, Ctx};
use crate::command::CommandHost;
use crate::command::{CommandId, KeyChord, Value};
use crate::core::{DocumentId, TargetRef, TaskContext, Version};
use crate::document::DocumentKind;
use crate::platform::clipboard;
use crate::recovery::{RecoveryKind, RecoveryStore, Snapshot};
use crate::text::{self, Encoding, Eol, OnUnmappable, TextFormat};
use crate::ui::app::{with_app, App};
use crate::ui::dialogs::{CloseChoice, RestoreChoice};

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!()));
        }
    };
}

pub(super) fn step<R>(f: impl FnOnce(&mut App) -> R) -> std::result::Result<R, String> {
    with_app(f).ok_or_else(|| "アプリを借りられません".to_owned())
}

/// 条件が成り立つまで（アプリを借りずに）メッセージを回して待つ。
pub(super) fn wait(what: &str, mut cond: impl FnMut(&mut App) -> bool) -> Check {
    if pump_until(|| with_app(&mut cond).unwrap_or(false)) {
        Ok(())
    } else {
        let status = with_app(|app| app.status_message().map(str::to_owned)).flatten();
        Err(format!(
            "待ち合わせがタイムアウトしました: {what}（状態表示: {status:?}）"
        ))
    }
}

pub(super) fn cmd(id: &'static str, args: Value) -> Result<(), String> {
    step(|app| app.run_command(CommandId::new(id), args))
}

fn encode(text: &str, encoding: Encoding, bom: bool, on: OnUnmappable) -> Vec<u8> {
    let format = TextFormat {
        encoding,
        bom,
        eol: Eol::CrLf,
    };
    text::encode(text, format, on, &TaskContext::detached()).expect("変換できる")
}

pub(super) fn open_and_wait(path: &Path) -> Result<DocumentId, String> {
    let p = path.to_path_buf();
    cmd("file.open", json!({ "path": path.to_string_lossy() }))?;
    wait("ファイルが開く", |app| {
        app.documents()
            .iter()
            .any(|d| d.state.path() == Some(p.as_path()))
    })?;
    step(|app| {
        app.documents()
            .iter()
            .find(|d| d.state.path() == Some(path))
            .map(|d| d.id())
    })?
    .ok_or_else(|| "開いた文書がない".to_owned())
}

fn save_and_wait(id: DocumentId) -> Check {
    cmd("file.save", Value::Null)?;
    wait("保存が終わる", move |app| {
        app.document(id).is_some_and(|d| !d.saving)
    })
}

pub(super) fn run(ctx: &Ctx) -> Check {
    super::integration_i2::startup_is_light()?;
    file_round_trips(ctx)?;
    unmappable_and_external_change(ctx)?;
    closing(ctx)?;
    reopen_with_encoding(ctx)?;
    recovery(ctx)?;
    memos(ctx)?;
    editing_commands(ctx)?;
    clipboard_tab()?;
    menu_keys_and_settings(ctx)?;
    super::integration_i2::run(ctx)?;
    super::integration_u1::run(ctx)?;
    super::integration_u2::run(ctx)?;
    super::integration_u3::run(ctx)?;
    super::integration_u4::run(ctx)?;
    super::integration_u5::run(ctx)?;
    super::integration_v1::run(ctx)?;
    super::integration_v2::run(ctx)?;
    Ok(())
}

/// CP932・CRLF のファイルを開いて編集して保存しても、文字コードと改行が保たれる。
fn file_round_trips(ctx: &Ctx) -> Check {
    let cp932 = ctx.dir.join("cp.txt");
    std::fs::write(
        &cp932,
        encode(
            "日本語\r\nテスト\r\n",
            Encoding::Cp932,
            false,
            OnUnmappable::Fail,
        ),
    )
    .map_err(|e| e.to_string())?;
    let id = open_and_wait(&cp932)?;
    step(|app| -> Check {
        let st = app.state(id).unwrap();
        check!(
            st.format.encoding == Encoding::Cp932,
            "文字コード {:?}",
            st.format
        );
        check!(st.format.eol == Eol::CrLf && !st.format.bom, "改行・BOM");
        check!(!st.is_dirty(), "開いた直後は未変更");
        check!(
            app.document(id).unwrap().title == "cp.txt",
            "題名 {}",
            app.document(id).unwrap().title
        );
        check!(text_of(app, id) == "日本語\r\nテスト\r\n", "本文");
        check!(
            app.status.info().encoding.contains("Shift_JIS"),
            "ステータスの文字コード"
        );
        app.with_text(id, |ed| ed.set_text("日本語\r\nテスト\r\n追加\r\n"))
            .map_err(super::e2s)?;
        check!(app.state(id).unwrap().is_dirty(), "編集で未保存");
        Ok(())
    })??;
    save_and_wait(id)?;
    step(|app| -> Check {
        check!(!app.state(id).unwrap().is_dirty(), "保存後は未変更");
        Ok(())
    })??;
    let saved = std::fs::read(&cp932).map_err(|e| e.to_string())?;
    check!(
        saved
            == encode(
                "日本語\r\nテスト\r\n追加\r\n",
                Encoding::Cp932,
                false,
                OnUnmappable::Fail
            ),
        "CP932 のまま保存される: {saved:?}"
    );

    // UTF-8 の BOM と LF も保たれる。
    let bom = ctx.dir.join("bom.md");
    std::fs::write(&bom, b"\xEF\xBB\xBFa\nb\n").map_err(|e| e.to_string())?;
    let id = open_and_wait(&bom)?;
    step(|app| -> Check {
        let st = app.state(id).unwrap();
        check!(
            st.format.encoding == Encoding::Utf8 && st.format.bom && st.format.eol == Eol::Lf,
            "BOM 付き LF {:?}",
            st.format
        );
        check!(text_of(app, id) == "a\nb\n", "BOM は本文に入らない");
        check!(
            app.document(id).unwrap().language == crate::ui::editor::Language::Markdown,
            "言語"
        );
        app.with_text(id, |ed| ed.set_text("a\nb\nc\n"))
            .map_err(super::e2s)?;
        Ok(())
    })??;
    save_and_wait(id)?;
    check!(
        std::fs::read(&bom).map_err(|e| e.to_string())? == b"\xEF\xBB\xBFa\nb\nc\n",
        "BOM と LF のまま保存される"
    );
    // 同じファイルは同じタブで開く。
    let again = open_and_wait(&bom)?;
    check!(again == id, "同じファイルは同じタブ");
    // 一時ファイルが残らない。
    let leftovers = std::fs::read_dir(&ctx.dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter(|e| crate::file::is_temp_file_name(&e.file_name()))
        .count();
    check!(leftovers == 0, "一時ファイルが残っている");
    Ok(())
}

/// 変換できない文字の確認と、外部変更の検知（別名保存の案内）。
fn unmappable_and_external_change(ctx: &Ctx) -> Check {
    let path = ctx.dir.join("cp.txt");
    let id = step(|app| {
        app.documents()
            .iter()
            .find(|d| d.state.path() == Some(path.as_path()))
            .map(|d| d.id())
    })?
    .ok_or("cp.txt が開いていない")?;
    let before = std::fs::read(&path).map_err(|e| e.to_string())?;
    step(|app| {
        app.activate(id).ok();
        app.with_text(id, |ed| ed.set_text("日本語\r\n😀\r\n"))
    })?
    .map_err(super::e2s)?;
    // 断ると、何も書かれず、未保存のまま。
    ctx.prompts.confirm.borrow_mut().push_back(false);
    save_and_wait(id)?;
    check!(
        std::fs::read(&path).map_err(|e| e.to_string())? == before,
        "断ったのにファイルが変わった"
    );
    check!(
        ctx.prompts
            .shown
            .borrow()
            .iter()
            .any(|s| s.starts_with("confirm:") && s.contains("😀")),
        "変換できない文字の確認が出ない: {:?}",
        ctx.prompts.shown.borrow()
    );
    check!(
        step(|app| app.state(id).unwrap().is_dirty())?,
        "断ったら未保存のまま"
    );
    // 承知すると「?」に置き換えて保存する。
    ctx.prompts.confirm.borrow_mut().push_back(true);
    save_and_wait(id)?;
    // 確認のあとの再保存もワーカーなので、保存が済んで未保存でなくなるまで待つ。
    wait("置き換えて保存", |app| {
        !app.state(id).unwrap().is_dirty()
    })?;
    check!(
        std::fs::read(&path).map_err(|e| e.to_string())?
            == encode(
                "日本語\r\n😀\r\n",
                Encoding::Cp932,
                false,
                OnUnmappable::Replace
            ),
        "? に置き換えた内容"
    );

    // 外部で書き換えられると、上書きせず、別名保存を案内する。
    std::fs::write(&path, b"changed outside, longer than before").map_err(|e| e.to_string())?;
    let outside = std::fs::read(&path).map_err(|e| e.to_string())?;
    step(|app| app.with_text(id, |ed| ed.set_text("編集した内容\r\n")))?.map_err(super::e2s)?;
    ctx.prompts.confirm.borrow_mut().push_back(false);
    cmd("file.save", Value::Null)?;
    check!(
        step(|app| app.state(id).unwrap().has_external_change())?,
        "外部変更を検知しない"
    );
    check!(
        std::fs::read(&path).map_err(|e| e.to_string())? == outside,
        "外部変更を上書きした"
    );
    // 上書き保存はできず、別名では保存できる。
    let other = ctx.dir.join("other.txt");
    ctx.prompts.save.borrow_mut().push_back(Some(other.clone()));
    cmd("file.save_as", Value::Null)?;
    wait("別名で保存", |app| {
        app.state(id)
            .is_some_and(|s| s.path() == Some(other.as_path()) && !s.is_dirty())
    })?;
    check!(
        std::fs::read(&other).map_err(|e| e.to_string())?
            == encode(
                "編集した内容\r\n",
                Encoding::Cp932,
                false,
                OnUnmappable::Fail
            ),
        "別名で保存した内容"
    );
    check!(
        !step(|app| app.state(id).unwrap().has_external_change())?,
        "別名保存で外部変更が解消する"
    );
    check!(
        std::fs::read(&path).map_err(|e| e.to_string())? == outside,
        "元のファイルはそのまま"
    );
    Ok(())
}

/// 未保存のタブを閉じるときの確認（取り消し／保存しない／保存）。
fn closing(ctx: &Ctx) -> Check {
    cmd("file.new", Value::Null)?;
    let id = step(|app| app.active_id().unwrap())?;
    step(|app| app.with_text(id, |ed| ed.set_text("閉じる前")))?.map_err(super::e2s)?;
    // 取り消し。
    ctx.prompts
        .close
        .borrow_mut()
        .push_back(CloseChoice::Cancel);
    let closed = step(|app| app.close_document(id, false))?.map_err(super::e2s)?;
    check!(
        !closed && step(|app| app.state(id).is_some())?,
        "取り消しで閉じた"
    );
    // 保存せずに閉じる。
    ctx.prompts
        .close
        .borrow_mut()
        .push_back(CloseChoice::Discard);
    let closed = step(|app| app.close_document(id, false))?.map_err(super::e2s)?;
    check!(
        closed && step(|app| app.state(id).is_none())?,
        "保存しないで閉じられない"
    );

    // 保存して閉じる（保存先を尋ね、保存が終わってから閉じる）。
    cmd("file.new", Value::Null)?;
    let id = step(|app| app.active_id().unwrap())?;
    step(|app| app.with_text(id, |ed| ed.set_text("保存して閉じる\r\n")))?.map_err(super::e2s)?;
    let target = ctx.dir.join("closing.txt");
    ctx.prompts.close.borrow_mut().push_back(CloseChoice::Save);
    ctx.prompts
        .save
        .borrow_mut()
        .push_back(Some(target.clone()));
    let closed = step(|app| app.close_document(id, false))?.map_err(super::e2s)?;
    check!(!closed, "保存が終わるまで閉じない");
    wait("保存して閉じる", |app| app.state(id).is_none())?;
    check!(
        std::fs::read(&target).map_err(|e| e.to_string())? == "保存して閉じる\r\n".as_bytes(),
        "保存した内容"
    );

    // 終了の確認: 取り消しなら終了せず、「保存しない」なら終了してよい（確認は 1 回だけ）。
    cmd("file.new", Value::Null)?;
    let id = step(|app| app.active_id().unwrap())?;
    step(|app| app.with_text(id, |ed| ed.set_text("終了前")))?.map_err(super::e2s)?;
    ctx.prompts
        .close
        .borrow_mut()
        .push_back(CloseChoice::Cancel);
    check!(!step(|app| app.can_exit())?, "取り消しで終了した");
    ctx.prompts
        .close
        .borrow_mut()
        .push_back(CloseChoice::Discard);
    check!(step(|app| app.can_exit())?, "保存しないなら終了できる");
    check!(step(|app| app.can_exit())?, "確認は繰り返さない");
    // 後始末（破棄の確認なしで閉じる）。
    step(|app| app.close_document(id, true))?.map_err(super::e2s)?;
    Ok(())
}

/// 文字コードを指定して開き直す。
fn reopen_with_encoding(ctx: &Ctx) -> Check {
    let path = ctx.dir.join("reopen.txt");
    std::fs::write(
        &path,
        encode(
            "漢字のテスト\r\n",
            Encoding::Cp932,
            false,
            OnUnmappable::Fail,
        ),
    )
    .map_err(|e| e.to_string())?;
    let first = open_and_wait(&path)?;
    // UTF-8 として読み直すと、読めないバイトが置き換わり、警告する。
    cmd("file.reopen_with_encoding", json!({ "encoding": "utf-8" }))?;
    wait("UTF-8 で開き直す", |app| {
        app.document(first).is_none()
            && app
                .documents()
                .iter()
                .any(|d| d.state.path() == Some(path.as_path()))
    })?;
    let id = step(|app| {
        app.documents()
            .iter()
            .find(|d| d.state.path() == Some(path.as_path()))
            .map(|d| d.id())
    })?
    .unwrap();
    step(|app| -> Check {
        check!(
            app.document(id).unwrap().lossy,
            "置き換えた文書は警告の対象"
        );
        check!(
            app.state(id).unwrap().format.encoding == Encoding::Utf8,
            "UTF-8"
        );
        check!(text_of(app, id).contains('\u{fffd}'), "置き換えた文字");
        check!(
            app.status_message()
                .is_some_and(|m| m.contains("置き換えて")),
            "警告 {:?}",
            app.status_message()
        );
        Ok(())
    })??;
    // 置き換えたまま上書きするときは確認し、断れば書かない。
    let before = std::fs::read(&path).map_err(|e| e.to_string())?;
    step(|app| app.with_text(id, |ed| ed.set_text("x")))?.map_err(super::e2s)?;
    ctx.prompts.confirm.borrow_mut().push_back(false);
    save_and_wait(id)?;
    check!(
        std::fs::read(&path).map_err(|e| e.to_string())? == before,
        "確認を断ったのに保存した"
    );
    // CP932 で開き直すと元に戻る（未保存の変更は、確認のうえ破棄される）。
    ctx.prompts.confirm.borrow_mut().push_back(true);
    cmd("file.reopen_with_encoding", json!({ "encoding": "cp932" }))?;
    wait("CP932 で開き直す", |app| {
        app.document(id).is_none()
            && app
                .documents()
                .iter()
                .any(|d| d.state.path() == Some(path.as_path()))
    })?;
    let id = step(|app| {
        app.documents()
            .iter()
            .find(|d| d.state.path() == Some(path.as_path()))
            .map(|d| d.id())
    })?
    .unwrap();
    step(|app| -> Check {
        check!(text_of(app, id) == "漢字のテスト\r\n", "元の本文");
        check!(!app.document(id).unwrap().lossy, "警告が消える");
        check!(!app.state(id).unwrap().is_dirty(), "未変更");
        Ok(())
    })??;
    Ok(())
}

fn find_title(app: &App, title: &str) -> Option<DocumentId> {
    app.documents()
        .iter()
        .find(|d| d.title == title)
        .map(|d| d.id())
}

/// 復元データの書き出し・走査・復元・破棄。
fn recovery(ctx: &Ctx) -> Check {
    let store = RecoveryStore::new(&ctx.paths);
    // 編集した文書の復元データが、書き出しの時刻が来ると書かれる。
    cmd("file.new", Value::Null)?;
    let id = step(|app| app.active_id().unwrap())?;
    step(|app| {
        let _ = app.set_title(id, "復元の元".into());
        app.with_text(id, |ed| ed.set_text("復元される本文\r\n"))
    })?
    .map_err(super::e2s)?;
    check!(
        step(|app| app.document(id).unwrap().pending.is_some())?,
        "書き出しの待ちになっていない"
    );
    step(|app| app.flush_pending_writes())?;
    wait("復元データが書かれる", |_| {
        store
            .scan()
            .map(|r| {
                r.live
                    .iter()
                    .any(|e| e.meta.as_ref().is_some_and(|m| m.title == "復元の元"))
            })
            .unwrap_or(false)
    })?;
    // 保存すると、その版までの復元データは消える。
    let target = ctx.dir.join("recovered-save.txt");
    ctx.prompts
        .save
        .borrow_mut()
        .push_back(Some(target.clone()));
    cmd("file.save", Value::Null)?;
    wait("保存で復元データが消える", |_| {
        store
            .scan()
            .map(|r| {
                !r.live
                    .iter()
                    .any(|e| e.meta.as_ref().is_some_and(|m| m.title == "復元の元"))
            })
            .unwrap_or(false)
    })?;
    step(|app| app.close_document(id, true))?.map_err(super::e2s)?;

    // 持ち主のいない復元データ（前回の異常終了を模す）を、尋ねて復元する。
    let make_orphan = |title: &str, text: &str| -> Result<(), String> {
        let slot = store.open_slot().map_err(|e| e.to_string())?;
        slot.write(
            &Snapshot {
                kind: RecoveryKind::Untitled,
                source_path: None,
                format: TextFormat::DEFAULT,
                file_stamp: None,
                title: title.to_owned(),
                version: Version::INITIAL.next(),
                text: text.to_owned(),
            },
            &TaskContext::detached(),
        )
        .map_err(|e| e.to_string())?;
        slot.release_keeping_data();
        Ok(())
    };
    make_orphan("孤児", "復元された\r\n本文")?;
    let report = store.scan().map_err(|e| e.to_string())?;
    check!(
        report.candidates.len() == 1,
        "候補の数 {}",
        report.candidates.len()
    );
    ctx.prompts
        .restore
        .borrow_mut()
        .push_back(RestoreChoice::Restore);
    step(|app| app.offer_restore(report))?;
    let restored = step(|app| find_title(app, "孤児"))?.ok_or("復元された文書がない")?;
    step(|app| -> Check {
        let d = app.document(restored).unwrap();
        check!(d.state.is_dirty(), "復元した文書は未保存");
        check!(*d.state.kind() == DocumentKind::Untitled, "種類");
        check!(d.slot.is_some(), "復元データを引き継ぐ");
        check!(text_of(app, restored) == "復元された\r\n本文", "本文");
        Ok(())
    })??;
    // 復元した文書を保存すると、復元データがなくなり、次の走査で候補にならない。
    let saved = ctx.dir.join("restored.txt");
    ctx.prompts.save.borrow_mut().push_back(Some(saved.clone()));
    step(|app| app.activate(restored))?.map_err(super::e2s)?;
    cmd("file.save", Value::Null)?;
    wait("復元した文書を保存", |app| {
        app.state(restored).is_some_and(|s| !s.is_dirty())
    })?;
    step(|app| app.close_document(restored, true))?.map_err(super::e2s)?;

    // 断ると破棄される。「あとで」なら残る。
    make_orphan("破棄する", "不要")?;
    let report = store.scan().map_err(|e| e.to_string())?;
    ctx.prompts
        .restore
        .borrow_mut()
        .push_back(RestoreChoice::Later);
    step(|app| app.offer_restore(report))?;
    let report = store.scan().map_err(|e| e.to_string())?;
    check!(report.candidates.len() == 1, "あとでなら残る");
    ctx.prompts
        .restore
        .borrow_mut()
        .push_back(RestoreChoice::Discard);
    step(|app| app.offer_restore(report))?;
    let report = store.scan().map_err(|e| e.to_string())?;
    check!(report.candidates.is_empty(), "破棄したのに残っている");
    check!(
        step(|app| find_title(app, "破棄する").is_none())?,
        "破棄したものは開かない"
    );
    Ok(())
}

/// メモのタブ: 開く、自動保存、閉じるときの保存、版、小窓。
fn memos(ctx: &Ctx) -> Check {
    let store = crate::memo::MemoStore::new(&ctx.paths);
    let memo = store
        .create("テストメモ", "hello")
        .map_err(|e| e.to_string())?;
    let id = step(|app| app.open_memo(memo.clone()))?.map_err(super::e2s)?;
    step(|app| -> Check {
        let d = app.document(id).unwrap();
        check!(
            matches!(d.state.kind(), DocumentKind::Memo(m) if *m == memo.id),
            "メモの種類"
        );
        check!(d.title == "テストメモ", "題名");
        check!(!d.state.is_dirty(), "開いた直後は未変更");
        check!(text_of(app, id) == "hello", "本文");
        Ok(())
    })??;
    // 開いているメモをもう一度開いても、同じタブ。
    check!(
        step(|app| app.open_memo(memo.clone()))?.map_err(super::e2s)? == id,
        "同じメモは同じタブ"
    );
    // 編集すると自動保存の待ちになり、時刻が来ると保存される。
    step(|app| app.with_text(id, |ed| ed.set_text("hello world")))?.map_err(super::e2s)?;
    check!(
        step(|app| app.state(id).unwrap().is_dirty())?,
        "編集で未保存"
    );
    step(|app| app.flush_pending_writes())?;
    wait("メモの自動保存", |app| {
        app.document(id)
            .is_some_and(|d| !d.state.is_dirty() && !d.write_in_flight)
    })?;
    let loaded = store.load(&memo.id).map_err(|e| e.to_string())?;
    check!(
        loaded.body == "hello world",
        "保存した本文 {:?}",
        loaded.body
    );
    check!(loaded.version > memo.version, "版が進む");
    let version =
        step(|app| app.target_version(&TargetRef::Memo(memo.id.clone())))?.map_err(super::e2s)?;
    check!(version == loaded.version, "TargetRef::Memo の版");
    // 開いていないメモの版は保存領域から読む。
    let other = store.create("別のメモ", "x").map_err(|e| e.to_string())?;
    check!(
        step(|app| app.target_version(&TargetRef::Memo(other.id.clone())))?.map_err(super::e2s)?
            == other.version,
        "開いていないメモの版"
    );
    // 「上書き保存」はメモの保存。名前の変更。
    step(|app| app.with_text(id, |ed| ed.set_text("hello again")))?.map_err(super::e2s)?;
    cmd("file.save", Value::Null)?;
    check!(
        store.load(&memo.id).map_err(|e| e.to_string())?.body == "hello again",
        "保存コマンドでメモが保存される"
    );
    cmd("memo.rename", json!({ "name": "改名したメモ" }))?;
    check!(
        step(|app| app.document(id).unwrap().title.clone())? == "改名したメモ"
            && store.load(&memo.id).map_err(|e| e.to_string())?.name == "改名したメモ",
        "メモの名前の変更"
    );
    // 未保存のまま閉じても、尋ねずに保存する。
    step(|app| app.with_text(id, |ed| ed.set_text("closing body")))?.map_err(super::e2s)?;
    check!(
        step(|app| app.close_document(id, false))?.map_err(super::e2s)?,
        "メモは尋ねずに閉じる"
    );
    check!(
        store.load(&memo.id).map_err(|e| e.to_string())?.body == "closing body",
        "閉じる前に保存される"
    );
    // 小窓は初めて開くときに作り、もう一度で閉じる。
    cmd("memo.open_panel", Value::Null)?;
    check!(step(|app| app.memo_panel_visible())?, "小窓が開く");
    cmd("memo.open_panel", Value::Null)?;
    check!(!step(|app| app.memo_panel_visible())?, "小窓が閉じる");
    Ok(())
}

/// 編集コマンド（すべて選択、切り取り、貼り付け、行へ移動）。
fn editing_commands(ctx: &Ctx) -> Check {
    cmd("file.new", Value::Null)?;
    let id = step(|app| app.active_id().unwrap())?;
    step(|app| app.with_text(id, |ed| ed.set_text("a\nb\nc\n")))?.map_err(super::e2s)?;
    cmd("edit.select_all", Value::Null)?;
    check!(
        step(|app| {
            let r = app.editor().selection().range();
            r.end - r.start
        })? == 6,
        "すべて選択"
    );
    cmd("edit.cut", Value::Null)?;
    check!(step(|app| text_of(app, id))?.is_empty(), "切り取り");
    cmd("edit.paste", Value::Null)?;
    // 貼り付けは、文書の改行（CRLF）にそろえられる。
    check!(
        step(|app| text_of(app, id))? == "a\r\nb\r\nc\r\n",
        "貼り付け"
    );
    cmd("edit.undo", Value::Null)?;
    check!(step(|app| text_of(app, id))?.is_empty(), "元に戻す");
    cmd("edit.redo", Value::Null)?;
    check!(
        step(|app| text_of(app, id))? == "a\r\nb\r\nc\r\n",
        "やり直し"
    );
    // 行へ移動: 引数で、入力欄で、範囲外は最終行へ。
    cmd("edit.goto_line", json!({ "line": 2 }))?;
    let line = |app: &mut App| {
        let caret = app.editor().selection().caret;
        app.editor().line_from_position(caret) + 1
    };
    check!(step(line)? == 2, "行 2 へ");
    ctx.prompts.input.borrow_mut().push_back(Some("３".into()));
    cmd("edit.goto_line", Value::Null)?;
    check!(step(line)? == 3, "入力欄で行 3 へ（全角数字）");
    cmd("edit.goto_line", json!({ "line": 999 }))?;
    check!(step(line)? == 4, "範囲外は最終行");
    ctx.prompts.input.borrow_mut().push_back(Some("abc".into()));
    cmd("edit.goto_line", Value::Null)?;
    check!(step(line)? == 4, "不正な入力では動かない");
    step(|app| app.close_document(id, true))?.map_err(super::e2s)?;
    Ok(())
}

/// クリップボードから未保存のタブを作る。
fn clipboard_tab() -> Check {
    step(|app| clipboard::write_text(Some(app.hwnd()), "クリップボード\r\nの内容\ttab"))?
        .map_err(super::e2s)?;
    cmd("clipboard.new_tab", Value::Null)?;
    step(|app| -> Check {
        let id = app.active_id().unwrap();
        let st = app.state(id).unwrap();
        check!(*st.kind() == DocumentKind::Clipboard, "種類");
        check!(st.is_dirty(), "未保存のタブ");
        check!(st.format.eol == Eol::CrLf, "改行");
        check!(
            text_of(app, id) == "クリップボード\r\nの内容\ttab",
            "本文 {:?}",
            text_of(app, id)
        );
        check!(
            app.document(id)
                .unwrap()
                .title
                .starts_with("クリップボード"),
            "題名"
        );
        check!(
            app.status_message()
                .is_some_and(|m| m.contains("新しいタブ")),
            "状態表示"
        );
        Ok(())
    })??;
    // グローバルホットキーの登録の状態を持つ（競合しても動作は続く）。
    check!(
        step(|app| app.hotkey_status().is_some())?,
        "ホットキーを登録する"
    );
    Ok(())
}

/// メニュー、キー割り当て、テーマの切り替えと設定の保存。
fn menu_keys_and_settings(ctx: &Ctx) -> Check {
    step(|app| -> Check {
        check!(
            app.menu().is_some_and(|m| !m.entries().is_empty()),
            "メニューがある"
        );
        let key = |s: &str| app.keymap().command_for(&KeyChord::parse(s).unwrap());
        check!(key("Ctrl+S") == Some(CommandId::new("file.save")), "Ctrl+S");
        check!(key("Ctrl+O") == Some(CommandId::new("file.open")), "Ctrl+O");
        check!(
            key("Ctrl+G") == Some(CommandId::new("edit.goto_line")),
            "Ctrl+G"
        );
        check!(
            key("Ctrl+Shift+M") == Some(CommandId::new("memo.open_panel")),
            "Ctrl+Shift+M"
        );
        check!(
            key("Ctrl+Alt+V") == Some(CommandId::new("clipboard.new_tab")),
            "Ctrl+Alt+V"
        );
        // メニューから選んだのと同じ経路（登録表）で実行できる。
        let entries = app.menu().unwrap().entries().to_vec();
        let item = entries
            .iter()
            .find(|e| e.command == CommandId::new("file.new"))
            .ok_or("新規の項目がない")?;
        let n = app.documents().len();
        check!(app.on_menu_command(item.id), "メニュー項目の実行");
        check!(app.documents().len() == n + 1, "メニューの「新規」");
        Ok(())
    })??;

    // テーマを切り替えると設定ファイルに保存される（Dark → System）。
    let settings = ctx.paths.settings_file();
    cmd("view.toggle_theme", Value::Null)?;
    check!(
        step(|app| app.theme_mode())? == crate::appdata::ThemeMode::System,
        "テーマの切り替え"
    );
    wait("設定の保存", |_| settings.exists())?;
    let text = std::fs::read_to_string(&settings).map_err(|e| e.to_string())?;
    check!(
        text.contains("\"theme\": \"system\""),
        "保存された設定 {text}"
    );
    step(|app| app.set_theme_mode(crate::appdata::ThemeMode::Dark))?;

    // 壊れた設定ファイルは上書きしない。
    // 進行中の設定の書き出し（最近使ったファイルの記録など）が終わるのを待つ。
    std::thread::sleep(std::time::Duration::from_millis(400));
    let broken: PathBuf = settings.clone();
    std::fs::write(&broken, "{ broken").map_err(|e| e.to_string())?;
    step(|app| {
        app.settings_state = super::super::fileio::SettingsState::Broken("試験".into());
        app.persist_settings();
    })?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    check!(
        std::fs::read_to_string(&broken).map_err(|e| e.to_string())? == "{ broken",
        "壊れた設定ファイルを上書きした"
    );
    Ok(())
}
