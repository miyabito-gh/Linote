//! 改善 V4-1 の自己診断: 設定の再読み込み（テーマ・表示・キーとメニューの表示・グローバル
//! ショートカット・そのほかの項目）、壊れた設定ファイルの扱いと復旧、自分の書き込みでは再読み込み
//! しないこと、外部の変更の自動検出、Linote のタブで保存したときの再読み込み、設定ファイルを開く、
//! 設定画面（台本つきの操作と、実際のダイアログの操作）での保存と反映。V4-2 は「キー」ページ
//! （一覧、絞り込み、割り当て、付け替え、外す、既定に戻す、保存とアクセラレーター・メニュー表示への反映）。
//! 実際のウィンドウの見た目・前面に戻ったときの自動検出・高 DPI は Windows 実機で確認する
//! （`docs/verification/V.md`）。

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use serde_json::{json, Value};

use super::integration::{cmd, step, wait};
use super::{Check, Ctx};
use crate::appdata::settings::{stamp_changed, SettingsStamp, WrapMode};
use crate::appdata::{Settings, ThemeMode};
use crate::command::{CommandId, KeyChord};
use crate::platform::hotkey::HotkeyStatus;
use crate::ui::app::fileio::SettingsState;
use crate::ui::dialogs::SettingsSession;
use crate::ui::settings_dialog as dialog;

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!()));
        }
    };
}

pub(super) fn run(ctx: &Ctx) -> Check {
    let file = ctx.paths.settings_file();
    commands_and_menu()?;
    broken_then_fixed(&file)?;
    reload_applies_everything(&file)?;
    own_write_is_not_reloaded(&file)?;
    external_change_is_detected(&file)?;
    open_and_save_settings_file(&file)?;
    scripted_dialog(ctx, &file)?;
    real_dialog(&file)?;
    keys_page(ctx, &file)?;
    // 後始末: 既定の設定に戻す。
    write(&file, "{}")?;
    cmd("app.reload_settings", Value::Null)?;
    Ok(())
}

fn write(file: &Path, text: &str) -> Check {
    std::fs::write(file, text).map_err(|e| e.to_string())
}

fn settle() -> Check {
    wait("設定の書き出しが終わる", |app| {
        app.settings_writes_pending == 0
    })
}

/// コマンドの登録、既定のキー（Ctrl+, が衝突しない）、メニューの項目。
fn commands_and_menu() -> Check {
    step(|app| -> Check {
        for id in [
            "app.settings",
            "app.reload_settings",
            "app.open_settings_file",
        ] {
            check!(app.registry().find(id).is_some(), "{id} が登録されている");
            check!(
                !app.needs_document(CommandId::new(id)),
                "{id} は文書がなくても使える"
            );
        }
        let chord = KeyChord::parse("Ctrl+Comma").unwrap();
        check!(
            app.keymap().command_for(&chord) == Some(CommandId::new("app.settings")),
            "Ctrl+, は設定画面"
        );
        let entries = app.menu().ok_or("メニューがない")?.entries().to_vec();
        for (id, label) in [
            ("app.settings", "設定..."),
            ("app.open_settings_file", "設定ファイルを開く"),
            ("app.reload_settings", "設定を再読み込み"),
        ] {
            let e = entries
                .iter()
                .find(|e| e.command == CommandId::new(id))
                .ok_or_else(|| format!("メニューに {id} がない"))?;
            let text = app.menu().unwrap().label_of(e.id).unwrap_or_default();
            check!(text.starts_with(label), "メニューの表示 {text}");
        }
        let e = entries
            .iter()
            .find(|e| e.command == CommandId::new("app.settings"))
            .unwrap();
        let text = app.menu().unwrap().label_of(e.id).unwrap_or_default();
        check!(text.contains("Ctrl+Comma"), "メニューのキー表示 {text}");
        Ok(())
    })?
}

/// 壊れている間は今の設定を保って知らせ、書き込まない。直したら読めて、書き込みを再開する。
fn broken_then_fixed(file: &Path) -> Check {
    settle()?;
    write(file, "{ broken")?;
    let before = step(|app| {
        app.settings_state = SettingsState::Loaded;
        app.settings.clone()
    })?;
    cmd("app.reload_settings", Value::Null)?;
    step(|app| -> Check {
        check!(
            matches!(app.settings_state, SettingsState::Broken(_)),
            "壊れた設定ファイルを検出する"
        );
        check!(app.settings == before, "壊れている間は今の設定を保つ");
        let s = app.status_message().unwrap_or_default();
        check!(s.contains("読めません"), "状態欄の知らせ {s}");
        Ok(())
    })??;
    // 書き込みは止まる（ファイルは壊れたまま）。
    step(|app| app.persist_settings())?;
    std::thread::sleep(std::time::Duration::from_millis(150));
    check!(
        std::fs::read_to_string(file).map_err(|e| e.to_string())? == "{ broken",
        "壊れた設定ファイルを上書きした"
    );
    // 直ると読めて、書き込みを再開する。
    write(file, r#"{ "theme": "dark" }"#)?;
    cmd("app.reload_settings", Value::Null)?;
    step(|app| -> Check {
        check!(app.settings_state.writable(), "直したら書き込みを再開する");
        check!(
            matches!(app.settings_state, SettingsState::Loaded),
            "読めた状態"
        );
        let s = app.status_message().unwrap_or_default();
        check!(s.contains("保存を再開"), "状態欄の知らせ {s}");
        Ok(())
    })??;
    Ok(())
}

/// 再読み込みで、テーマ・表示・キー（アクセラレーターとメニュー）・グローバルショートカット・
/// ほかの項目がすべて反映される。キーの誤りは起動時と同じ文で知らせる。
fn reload_applies_everything(file: &Path) -> Check {
    cmd("file.new", Value::Null)?;
    write(
        file,
        &json!({
            "version": 1,
            "theme": "light",
            "keys": { "file.save": "Ctrl+Alt+Q", "clipboard.new_tab": "Ctrl+Alt+B" },
            "recent_files": ["C:\\work\\reloaded.txt"],
            "restore_session": false,
            "search_history": { "find": ["reloaded"], "replace": [], "grep_dir": [] },
            "view": {
                "wrap": "word", "wrap_column": 100, "font_size": 14, "zoom": 2,
                "show_whitespace": true, "show_eol": true, "line_numbers": false,
                "highlight_current_line": false, "highlight_braces": false,
                "tab_width": 8, "use_spaces": true, "auto_indent": false
            },
            "print": { "landscape": true, "color": false },
            "future": { "keep": 1 }
        })
        .to_string(),
    )?;
    cmd("app.reload_settings", Value::Null)?;
    step(|app| -> Check {
        let s = app.settings.clone();
        check!(app.theme_mode() == ThemeMode::Light, "テーマのモード");
        check!(!app.theme().dark, "ライトテーマの配色");
        check!(
            s.view.wrap == WrapMode::Word && s.view.tab_width == 8,
            "表示の設定"
        );
        check!(
            app.editor().view_settings() == s.view,
            "編集欄に表示の設定が反映された {:?}",
            app.editor().view_settings()
        );
        check!(app.editor().zoom() == 2, "拡大率");
        check!(!s.restore_session, "前回のタブの復元");
        check!(s.print.landscape && !s.print.color, "印刷の設定");
        check!(
            s.recent_files == vec!["C:\\work\\reloaded.txt".to_owned()],
            "最近使ったファイル"
        );
        check!(
            s.search_history.find == vec!["reloaded".to_owned()],
            "検索履歴"
        );
        // キー: 新しい割り当てがアクセラレーターとメニューに出て、古い既定は外れる。
        let key = |t: &str| app.keymap().command_for(&KeyChord::parse(t).unwrap());
        check!(
            key("Ctrl+Alt+Q") == Some(CommandId::new("file.save")),
            "新しいキー"
        );
        check!(key("Ctrl+S").is_none(), "古い既定のキーが残っている");
        let save = app
            .menu()
            .unwrap()
            .entries()
            .iter()
            .find(|e| e.command == CommandId::new("file.save"))
            .map(|e| e.id)
            .ok_or("上書き保存の項目がない")?;
        let text = app.menu().unwrap().label_of(save).unwrap_or_default();
        check!(
            text.contains("Ctrl+Alt+Q") && !text.contains("Ctrl+S"),
            "メニューのキー表示 {text}"
        );
        // グローバルショートカットを登録し直した（競合していても、割り当ては新しいもの）。
        match app.hotkey_status() {
            Some(HotkeyStatus::Registered(c) | HotkeyStatus::Conflict(c)) => {
                check!(c.to_string() == "Ctrl+Alt+B", "ホットキー {c}")
            }
            other => return Err(format!("ホットキーの状態 {other:?}")),
        }
        Ok(())
    })??;

    // キーの設定に問題があるときの通知は起動時と同じ文。
    write(
        file,
        r#"{ "theme": "light", "keys": { "file.open": "Ctrl+Nope" } }"#,
    )?;
    cmd("app.reload_settings", Value::Null)?;
    step(|app| -> Check {
        let s = app.status_message().unwrap_or_default();
        check!(
            s.contains("キーの設定に問題があります"),
            "キーの問題の通知 {s}"
        );
        // 誤った割り当ては既定値で補う。
        check!(
            app.keymap()
                .command_for(&KeyChord::parse("Ctrl+O").unwrap())
                == Some(CommandId::new("file.open")),
            "誤りは既定で補う"
        );
        // 外した項目は元に戻る（ホットキーは既定に）。
        check!(
            matches!(app.hotkey_status(), Some(HotkeyStatus::Registered(c) | HotkeyStatus::Conflict(c)) if c.to_string() == "Ctrl+Alt+V"),
            "ホットキーが既定に戻る"
        );
        Ok(())
    })??;

    // 項目を消す（ファイルを最小に）と、既定に戻る。知らない項目は保たれたまま書き戻せる。
    write(file, "{}")?;
    cmd("app.reload_settings", Value::Null)?;
    step(|app| -> Check {
        check!(app.settings == Settings::default(), "空のファイルは既定");
        check!(
            app.editor().view_settings() == app.settings.view,
            "表示も既定"
        );
        check!(app.editor().zoom() == 0, "拡大率も既定");
        Ok(())
    })?
}

/// 自分が書いた直後は、設定ファイルの変更として検出しない。
fn own_write_is_not_reloaded(file: &Path) -> Check {
    settle()?;
    cmd("view.toggle_eol", Value::Null)?;
    // 書き出しの最中は自動検出を止める。
    step(|app| -> Check {
        check!(app.settings_writes_pending > 0, "書き出しが進行中");
        let before = app.settings.clone();
        app.set_status("マーカー", crate::ui::statusbar::MessageKind::Info);
        app.check_settings_file();
        check!(app.settings == before, "書き出し中に再読み込みした");
        check!(
            app.status_message() == Some("マーカー"),
            "書き出し中に何かした"
        );
        Ok(())
    })??;
    settle()?;
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    check!(text.contains("\"show_eol\": true"), "保存された設定 {text}");
    step(|app| -> Check {
        check!(
            !stamp_changed(
                app.settings_stamp,
                SettingsStamp::of(&app.paths.as_ref().unwrap().settings_file())
            ),
            "自分の書き込みの印を覚えている"
        );
        app.set_status("マーカー", crate::ui::statusbar::MessageKind::Info);
        app.check_settings_file();
        check!(
            app.status_message() == Some("マーカー"),
            "自分の書き込みで再読み込みした"
        );
        check!(app.settings.view.show_eol, "設定が変わった");
        Ok(())
    })??;
    Ok(())
}

/// 外部（手や別のウィンドウ）の変更は、前面に戻ったとき（更新時刻と長さの違い）に拾う。
fn external_change_is_detected(file: &Path) -> Check {
    write(
        file,
        r#"{ "theme": "dark", "view": { "tab_width": 2, "show_eol": true }, "extra_item": [1, 2, 3] }"#,
    )?;
    step(|app| -> Check {
        app.check_settings_file();
        check!(app.theme_mode() == ThemeMode::Dark, "テーマが反映された");
        check!(app.settings.view.tab_width == 2, "タブ幅が反映された");
        check!(
            app.editor().view_settings().tab_width == 2,
            "編集欄に反映された"
        );
        let s = app.status_message().unwrap_or_default();
        check!(s.contains("変更を反映しました"), "知らせ {s}");
        // 続けて調べても何も起きない（印を更新済み）。
        app.set_status("マーカー", crate::ui::statusbar::MessageKind::Info);
        app.check_settings_file();
        check!(
            app.status_message() == Some("マーカー"),
            "二重に再読み込みした"
        );
        Ok(())
    })??;
    // 壊れた内容に変わっても、今の設定を保って知らせる。
    write(file, "{ broken again")?;
    step(|app| -> Check {
        app.check_settings_file();
        check!(
            matches!(app.settings_state, SettingsState::Broken(_)),
            "壊れた状態"
        );
        check!(app.theme_mode() == ThemeMode::Dark, "今の設定を保つ");
        Ok(())
    })??;
    // ファイルが消えても今の設定を保つ。
    std::fs::remove_file(file).map_err(|e| e.to_string())?;
    step(|app| -> Check {
        app.check_settings_file();
        check!(app.theme_mode() == ThemeMode::Dark, "消えても設定を保つ");
        check!(
            app.settings_state.writable(),
            "ファイルがなければ書ける状態"
        );
        Ok(())
    })?
}

/// 設定ファイルを開く（なければ今の設定で作る）。Linote のタブで保存すると再読み込みする。
fn open_and_save_settings_file(file: &Path) -> Check {
    settle()?;
    let _ = std::fs::remove_file(file);
    cmd("app.open_settings_file", Value::Null)?;
    wait("設定ファイルが開く", |app| {
        app.documents()
            .iter()
            .any(|d| d.state.path().is_some_and(|p| p == file))
    })?;
    check!(file.exists(), "なければ作ってから開く");
    let created = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let parsed = Settings::from_json(&created).map_err(|e| e.to_string())?;
    step(|app| -> Check {
        check!(parsed == app.settings, "今の設定で作る");
        check!(app.settings_state.writable(), "作った後は書ける状態");
        Ok(())
    })??;
    // タブで編集して保存すると、設定が読み直される。
    let id = step(|app| {
        app.documents()
            .iter()
            .find(|d| d.state.path().is_some_and(|p| p == file))
            .map(|d| d.id())
    })?
    .ok_or("設定ファイルのタブがない")?;
    step(|app| {
        app.with_text(id, |ed| {
            ed.set_text(r#"{ "theme": "light", "view": { "wrap": "char" } }"#)
        })
    })?
    .map_err(super::e2s)?;
    step(|app| app.activate(id))?.map_err(super::e2s)?;
    check!(
        step(|app| app
            .settings
            .recent_files
            .iter()
            .all(|p| !p.ends_with("settings.json")))?,
        "設定ファイルを最近使ったファイルに載せない"
    );
    cmd("file.save", Value::Null)?;
    wait("保存して再読み込みする", |app| {
        app.theme_mode() == ThemeMode::Light
    })?;
    step(|app| -> Check {
        check!(app.settings.view.wrap == WrapMode::Char, "表示も反映");
        let s = app.status_message().unwrap_or_default();
        check!(s.contains("再読み込み"), "知らせ {s}");
        Ok(())
    })??;
    // 開いたままの設定ファイルの文書を閉じる。
    step(|app| app.close_document(id, true).map_err(super::e2s))??;
    Ok(())
}

/// 台本つきの設定画面: OK・適用で保存と反映が行われ、画面で触らなかった項目（ほかのウィンドウが
/// 書いたもの、知らない項目）は失われない。壊れているときは保存せず、この起動の間だけ反映する。
fn scripted_dialog(ctx: &Ctx, file: &Path) -> Check {
    settle()?;
    write(
        file,
        r#"{ "theme": "system", "future": { "keep": 1 }, "view": { "tab_width": 4 } }"#,
    )?;
    cmd("app.reload_settings", Value::Null)?;
    // 画面を開く前に、別のウィンドウが タブ幅 と 最近使ったファイル を書いた（まだ読み直していない）。
    write(
        file,
        r#"{ "theme": "system", "future": { "keep": 1 }, "view": { "tab_width": 2 }, "recent_files": ["C:\\other.txt"] }"#,
    )?;
    scripted_dialog_run(ctx, file)
}

fn scripted_dialog_run(ctx: &Ctx, file: &Path) -> Check {
    let result: Rc<RefCell<Option<Settings>>> = Rc::new(RefCell::new(None));
    let r = result.clone();
    ctx.prompts.settings.borrow_mut().push_back(Box::new(
        move |session: &mut dyn SettingsSession| {
            let base = session.current();
            let mut edited = base.clone();
            edited.theme = ThemeMode::Dark;
            edited.view.show_whitespace = true;
            edited.view.wrap = WrapMode::Column;
            edited.view.wrap_column = 60;
            edited.restore_session = false;
            *r.borrow_mut() = Some(session.apply(&base, &edited));
        },
    ));
    cmd("app.settings", Value::Null)?;
    let applied = result
        .borrow()
        .clone()
        .ok_or("台本の操作が呼ばれていない")?;
    check!(applied.theme == ThemeMode::Dark, "適用後の設定のテーマ");
    step(|app| -> Check {
        check!(app.theme_mode() == ThemeMode::Dark, "テーマが反映された");
        check!(app.theme().dark, "ダークの配色");
        let v = &app.settings.view;
        check!(
            v.show_whitespace && v.wrap == WrapMode::Column && v.wrap_column == 60,
            "表示が反映された"
        );
        check!(
            app.editor().view_settings() == app.settings.view,
            "編集欄に反映された"
        );
        check!(!app.settings.restore_session, "前回のタブの復元");
        // 画面で触らなかった項目は、最新のファイルの内容のまま。
        check!(
            app.settings.view.tab_width == 2,
            "別のウィンドウのタブ幅を保つ"
        );
        check!(
            app.settings.recent_files == vec!["C:\\other.txt".to_owned()],
            "別のウィンドウの最近使ったファイルを保つ"
        );
        let s = app.status_message().unwrap_or_default();
        check!(s.contains("保存して反映"), "知らせ {s}");
        Ok(())
    })??;
    settle()?;
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let saved = Settings::from_json(&text).map_err(|e| e.to_string())?;
    check!(saved.theme == ThemeMode::Dark, "保存されたテーマ {text}");
    check!(
        saved.view.show_whitespace && saved.view.wrap_column == 60 && saved.view.tab_width == 2,
        "保存された表示 {text}"
    );
    check!(!saved.restore_session, "保存された restore_session");
    check!(text.contains("\"future\""), "知らない項目が消えた {text}");
    check!(
        text.contains("other.txt"),
        "最近使ったファイルが消えた {text}"
    );

    // キャンセル（台本が何も適用しない）では何も変わらない。
    ctx.prompts
        .settings
        .borrow_mut()
        .push_back(Box::new(|_| {}));
    let before = step(|app| app.settings.clone())?;
    cmd("app.settings", Value::Null)?;
    step(|app| -> Check {
        check!(app.settings == before, "キャンセルで設定が変わった");
        Ok(())
    })??;

    // 壊れているとき: 画面に理由を出し、OK・適用は「この起動の間だけ」反映して保存しない。
    write(file, "{ nope")?;
    cmd("app.reload_settings", Value::Null)?;
    let warning: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let w = warning.clone();
    ctx.prompts.settings.borrow_mut().push_back(Box::new(
        move |session: &mut dyn SettingsSession| {
            *w.borrow_mut() = session.warning();
            let base = session.current();
            let mut edited = base.clone();
            edited.view.show_eol = !base.view.show_eol;
            session.apply(&base, &edited);
        },
    ));
    let eol_before = step(|app| app.settings.view.show_eol)?;
    cmd("app.settings", Value::Null)?;
    let w = warning.borrow().clone().unwrap_or_default();
    check!(
        w.contains("読めません") && w.contains("この起動の間だけ"),
        "画面の上部の理由 {w}"
    );
    step(|app| -> Check {
        check!(
            app.settings.view.show_eol != eol_before,
            "壊れていても、この起動の間は反映する"
        );
        let s = app.status_message().unwrap_or_default();
        check!(s.contains("この起動の間だけ"), "知らせ {s}");
        Ok(())
    })??;
    std::thread::sleep(std::time::Duration::from_millis(150));
    check!(
        std::fs::read_to_string(file).map_err(|e| e.to_string())? == "{ nope",
        "壊れた設定ファイルを上書きした"
    );
    // 直して読み直す。
    write(file, "{}")?;
    cmd("app.reload_settings", Value::Null)?;
    step(|app| -> Check {
        check!(app.settings_state.writable(), "直ったら保存できる");
        Ok(())
    })?
}

/// 実際のダイアログ（Wine 上）: 部品の初期値、ページの切り替え、適用、OK、キャンセル。
fn real_dialog(file: &Path) -> Check {
    settle()?;
    write(
        file,
        r#"{ "theme": "light", "restore_session": true, "view": { "line_numbers": true, "wrap": "none", "tab_width": 4, "font_size": 12 } }"#,
    )?;
    cmd("app.reload_settings", Value::Null)?;
    let problems: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let p = problems.clone();
    let mut driver = move |dlg: windows::Win32::Foundation::HWND| {
        let bad = |what: &str, ok: bool| {
            if !ok {
                p.borrow_mut().push(what.to_owned());
            }
        };
        // 最初は「一般」ページ。
        bad("一般ページが見える", dialog::item_visible(dlg, "theme"));
        bad(
            "表示ページは隠れている",
            !dialog::item_visible(dlg, "view.wrap"),
        );
        bad(
            "テーマの初期値",
            dialog::get_choice(dlg, "theme").as_deref() == Some("light"),
        );
        bad(
            "前回のタブの初期値",
            dialog::get_check(dlg, "restore_session") == Some(true),
        );
        bad("警告がない", dialog::banner_text(dlg).is_none());
        // ページを切り替える。
        bad("表示ページを選べる", dialog::select_page(dlg, "view"));
        bad("表示ページが見える", dialog::item_visible(dlg, "view.wrap"));
        bad("一般ページは隠れる", !dialog::item_visible(dlg, "theme"));
        bad(
            "折り返しの初期値",
            dialog::get_choice(dlg, "view.wrap").as_deref() == Some("none"),
        );
        bad(
            "行番号の初期値",
            dialog::get_check(dlg, "view.line_numbers") == Some(true),
        );
        bad(
            "文字サイズの初期値",
            dialog::control_text(dlg, "view.font_size").as_deref() == Some("12"),
        );
        bad(
            "書体の表示",
            dialog::control_text(dlg, "view.font")
                .is_some_and(|t| t.contains("12 pt") && t.contains("既定")),
        );
        // 変更して「適用」。
        dialog::set_check(dlg, "view.show_eol", true);
        dialog::select_choice(dlg, "view.tab_width", "8");
        dialog::select_choice(dlg, "view.wrap", "column");
        dialog::set_number_text(dlg, "view.wrap_column", "120");
        dialog::press(dlg, dialog::ID_APPLY);
        // 「一般」に戻ってテーマを変え、OK。
        dialog::select_page(dlg, "general");
        dialog::select_choice(dlg, "theme", "dark");
        dialog::set_check(dlg, "restore_session", false);
        dialog::press(dlg, windows::Win32::UI::WindowsAndMessaging::IDOK.0);
    };
    step(|app| app.run_real_settings_dialog_for_test(&mut driver))?;
    check!(
        problems.borrow().is_empty(),
        "ダイアログの確認に失敗: {:?}",
        problems.borrow()
    );
    step(|app| -> Check {
        let s = &app.settings;
        check!(app.theme_mode() == ThemeMode::Dark, "テーマが反映された");
        check!(!s.restore_session, "前回のタブの復元");
        check!(
            s.view.show_eol
                && s.view.tab_width == 8
                && s.view.wrap == WrapMode::Column
                && s.view.wrap_column == 120,
            "表示が反映された {:?}",
            s.view
        );
        check!(app.editor().view_settings() == s.view, "編集欄に反映された");
        Ok(())
    })??;
    settle()?;
    let saved = Settings::from_json(&std::fs::read_to_string(file).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    check!(
        saved.theme == ThemeMode::Dark && saved.view.wrap_column == 120,
        "保存された"
    );
    // 変更を入れたあとの OK で保存された値は、閉じて開き直した画面の初期値になる。
    // キャンセルでは何も変わらない。
    let problems2: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let p2 = problems2.clone();
    let mut driver2 = move |dlg: windows::Win32::Foundation::HWND| {
        if dialog::get_choice(dlg, "theme").as_deref() != Some("dark") {
            p2.borrow_mut().push("開き直した画面のテーマ".into());
        }
        dialog::set_check(dlg, "restore_session", true);
        dialog::select_choice(dlg, "theme", "light");
        dialog::press(dlg, windows::Win32::UI::WindowsAndMessaging::IDCANCEL.0);
    };
    let before = step(|app| app.settings.clone())?;
    step(|app| app.run_real_settings_dialog_for_test(&mut driver2))?;
    check!(problems2.borrow().is_empty(), "{:?}", problems2.borrow());
    step(|app| -> Check {
        check!(app.settings == before, "キャンセルで設定が変わった");
        Ok(())
    })?
}

/// 「キー」ページ（実際のダイアログ）: 一覧と絞り込み、割り当て、重複の確認（付け替える・やめる）、
/// 外す、既定に戻す、OK での保存とアクセラレーター・メニューの表示への反映。
fn keys_page(ctx: &Ctx, file: &Path) -> Check {
    use windows::Win32::UI::WindowsAndMessaging::IDOK;
    settle()?;
    write(file, "{}")?;
    cmd("app.reload_settings", Value::Null)?;
    // 確認の答え: 1 つめ「やめる」、2 つめ「付け替える」、3 つめ「すべて既定に戻す」は はい。
    ctx.prompts.confirm.borrow_mut().extend([false, true, true]);
    ctx.prompts.shown.borrow_mut().clear();
    let problems: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let p = problems.clone();
    let mut driver = move |dlg: windows::Win32::Foundation::HWND| {
        let bad = |what: &str, ok: bool| {
            if !ok {
                p.borrow_mut().push(what.to_owned());
            }
        };
        let key_of = |dlg, command: &str| -> Option<[String; 4]> {
            let rows = dialog::key_list_texts(dlg);
            // 一覧の「キー」列は、コマンド名ではなく表示名で探す。
            let title = match command {
                "file.save" => "上書き保存",
                "file.open" => "開く...",
                "file.new" => "新規",
                _ => return None,
            };
            rows.into_iter().find(|r| r[1] == title)
        };
        bad("キーのページを選べる", dialog::select_page(dlg, "keys"));
        let rows = dialog::key_list_texts(dlg);
        bad("一覧にコマンドが並ぶ", rows.len() > 50);
        bad(
            "分類が出る",
            rows.iter().any(|r| r[0] == "ファイル") && rows.iter().any(|r| r[0] == "編集"),
        );
        bad(
            "グローバルショートカットも一覧にある",
            rows.iter()
                .any(|r| r[1].contains("（グローバル）") && r[2] == "Ctrl+Alt+V"),
        );
        bad("最初は変更済がない", rows.iter().all(|r| r[3].is_empty()));
        // 絞り込み。
        let all = rows.len();
        dialog::key_filter(dlg, "ctrl+s");
        let some = dialog::key_list_texts(dlg);
        bad(
            "キーで絞り込める",
            !some.is_empty() && some.len() < all && some.iter().any(|r| r[2] == "Ctrl+S"),
        );
        dialog::key_filter(dlg, "存在しない語");
        bad("該当なしは空", dialog::key_list_texts(dlg).is_empty());
        dialog::key_filter(dlg, "");
        bad(
            "絞り込みを消すと全部",
            dialog::key_list_texts(dlg).len() == all,
        );

        // 1. 割り当て: 上書き保存を Ctrl+Alt+K に。
        bad(
            "上書き保存の既定のキー",
            key_of(dlg, "file.save").is_some_and(|r| r[2] == "Ctrl+S"),
        );
        bad("上書き保存を選べる", dialog::key_select(dlg, "file.save"));
        dialog::key_capture(dlg, "ctrl+alt+k");
        dialog::press(dlg, dialog::ID_KEY_ASSIGN);
        bad(
            "割り当てが一覧に出る",
            key_of(dlg, "file.save").is_some_and(|r| {
                r[2] == "Ctrl+Alt+K" && r[3].contains("変更済") && r[3].contains("Ctrl+S")
            }),
        );
        bad("入力欄が空になる", dialog::key_capture_text(dlg).is_empty());
        // 2. 重複して「やめる」: 開く を Ctrl+Alt+K に → 確認は いいえ → 変わらない。
        bad("開くを選べる", dialog::key_select(dlg, "file.open"));
        dialog::key_capture(dlg, "Ctrl+Alt+K");
        dialog::press(dlg, dialog::ID_KEY_ASSIGN);
        bad(
            "やめたら変わらない",
            key_of(dlg, "file.open").is_some_and(|r| r[2] == "Ctrl+O")
                && key_of(dlg, "file.save").is_some_and(|r| r[2] == "Ctrl+Alt+K"),
        );
        // 3. 重複して「付け替える」: 確認は はい → 相手は割り当てなし。
        dialog::press(dlg, dialog::ID_KEY_ASSIGN);
        bad(
            "付け替えた",
            key_of(dlg, "file.open").is_some_and(|r| r[2] == "Ctrl+Alt+K")
                && key_of(dlg, "file.save")
                    .is_some_and(|r| r[2].is_empty() && r[3].contains("変更済")),
        );
        // 4. 割り当てられないキー（文字入力を奪う）は知らせて変えない。
        bad("新規を選べる", dialog::key_select(dlg, "file.new"));
        dialog::key_capture(dlg, "A");
        dialog::press(dlg, dialog::ID_KEY_ASSIGN);
        bad(
            "文字のキーは断る",
            key_of(dlg, "file.new").is_some_and(|r| r[2] == "Ctrl+N"),
        );
        // 5. 外す。
        dialog::press(dlg, dialog::ID_KEY_CLEAR);
        bad(
            "外せる",
            key_of(dlg, "file.new").is_some_and(|r| r[2].is_empty()),
        );
        // 6. 既定に戻す。
        dialog::press(dlg, dialog::ID_KEY_RESET);
        bad(
            "既定に戻せる",
            key_of(dlg, "file.new").is_some_and(|r| r[2] == "Ctrl+N" && r[3].is_empty()),
        );
        // 7. キー入力欄に押下を送ると表記になる（修飾キーなしの F5）。
        dialog::key_capture_press(dlg, 0x74);
        bad("F5 を受け取る", dialog::key_capture_text(dlg) == "F5");
        dialog::key_capture(dlg, "");
        // OK で保存と反映。
        dialog::press(dlg, IDOK.0);
    };
    step(|app| app.run_real_settings_dialog_for_test(&mut driver))?;
    check!(
        problems.borrow().is_empty(),
        "キーのページの確認に失敗: {:?}",
        problems.borrow()
    );
    let shown = ctx.prompts.shown.borrow().join("\n");
    check!(
        shown.contains("付け替え") || shown.contains("使っています"),
        "付け替えの確認の文面 {shown}"
    );
    check!(
        shown.contains("文字の入力に使う"),
        "割り当てられないキーの知らせ {shown}"
    );
    step(|app| -> Check {
        let key = |t: &str| app.keymap().command_for(&KeyChord::parse(t).unwrap());
        check!(
            key("Ctrl+Alt+K") == Some(CommandId::new("file.open")),
            "新しい割り当てがアクセラレーターに"
        );
        check!(key("Ctrl+S").is_none(), "外れた既定のキーが残っている");
        check!(key("Ctrl+O").is_none(), "付け替えた元の既定が残っている");
        let label = |command: &'static str| -> Result<String, String> {
            let id = app
                .menu()
                .unwrap()
                .entries()
                .iter()
                .find(|e| e.command == CommandId::new(command))
                .map(|e| e.id)
                .ok_or(format!("{command} の項目がない"))?;
            Ok(app.menu().unwrap().label_of(id).unwrap_or_default())
        };
        let open = label("file.open")?;
        check!(open.contains("Ctrl+Alt+K"), "開くのメニュー表示 {open}");
        let save = label("file.save")?;
        check!(!save.contains("Ctrl+"), "上書き保存のメニュー表示 {save}");
        check!(
            app.settings.keys.len() == 2
                && app.settings.keys.get("file.open").map(String::as_str) == Some("Ctrl+Alt+K")
                && app.settings.keys.get("file.save").map(String::as_str) == Some(""),
            "既定と違うものだけが keys に残る {:?}",
            app.settings.keys
        );
        Ok(())
    })??;
    settle()?;
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    check!(
        text.contains("Ctrl+Alt+K") && !text.contains("file.new"),
        "保存された keys {text}"
    );

    // 開き直すと、保存した割り当てが一覧に出る。「すべて既定に戻す」（はい）で全部戻り、
    // キャンセルするので設定は変わらない。続けて OK で戻す。
    let problems2: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let p2 = problems2.clone();
    let mut driver2 = move |dlg: windows::Win32::Foundation::HWND| {
        dialog::select_page(dlg, "keys");
        let rows = dialog::key_list_texts(dlg);
        let changed = rows.iter().filter(|r| !r[3].is_empty()).count();
        if changed != 2 {
            p2.borrow_mut()
                .push(format!("開き直した一覧の変更済は 2 件: {changed}"));
        }
        dialog::press(dlg, dialog::ID_KEY_RESET_ALL);
        if dialog::key_list_texts(dlg).iter().any(|r| !r[3].is_empty()) {
            p2.borrow_mut().push("すべて既定に戻す".into());
        }
        dialog::press(dlg, windows::Win32::UI::WindowsAndMessaging::IDOK.0);
    };
    step(|app| app.run_real_settings_dialog_for_test(&mut driver2))?;
    check!(problems2.borrow().is_empty(), "{:?}", problems2.borrow());
    step(|app| -> Check {
        check!(
            app.settings.keys.is_empty(),
            "すべて既定に戻した {:?}",
            app.settings.keys
        );
        check!(
            app.keymap()
                .command_for(&KeyChord::parse("Ctrl+S").unwrap())
                == Some(CommandId::new("file.save")),
            "既定のキーが戻った"
        );
        Ok(())
    })??;
    settle()?;
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    check!(!text.contains("Ctrl+Alt+K"), "keys が消えた {text}");
    Ok(())
}
