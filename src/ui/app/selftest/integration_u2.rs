//! 改善 U2 の自己診断: 表示の設定（折り返し、桁指定の折り返し、書体、拡大・縮小、空白・
//! 改行・行番号の表示、現在行、タブ幅、タブをスペースで、自動インデント）が編集欄と
//! 巨大ファイルのビューに反映され、設定ファイルに保存されること。メニューの印、キー割り当て。
//! 見た目（折り返しの位置、ガイド線、強調の色など）は Windows 実機で確認する
//! （`docs/verification/U.md`）。

use serde_json::json;
use windows::Win32::Foundation::{LPARAM, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, SendMessageW};

use super::integration::{cmd, step, wait};
use super::integration_i2::{find_big, write_numbered_lines};
use super::{text_of, Check, Ctx};
use crate::appdata::settings::{Settings, WrapMode};
use crate::bigfile::LargeModeThresholds;
use crate::command::{CommandId, KeyChord, Value};
use crate::ui::app::App;

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!()));
        }
    };
}

// Scintilla のメッセージ（読み出し用。sci.rs にない値は試験の中だけで使う）。
const SCI_GETVIEWWS: u32 = 2020;
const SCI_GETCARETLINEVISIBLE: u32 = 2095;
const SCI_GETUSETABS: u32 = 2125;
const SCI_GETTABWIDTH: u32 = 2121;
const SCI_GETVIEWEOL: u32 = 2355;
const SCI_GETEDGECOLUMN: u32 = 2360;
const SCI_GETEDGEMODE: u32 = 2362;
const SCI_GETWRAPMODE: u32 = 2269;
const SCI_GETMARGINRIGHT: u32 = 2158;
const SCI_WRAPCOUNT: u32 = 2235;
const SCI_GETZOOM: u32 = 2374;
const SCI_SETZOOM: u32 = 2373;
const SCI_NEWLINE: u32 = 2329;
const SCI_STYLEGETSIZEFRACTIONAL: u32 = 2062;
const STYLE_DEFAULT: usize = 32;

fn sci(app: &App, msg: u32, w: usize, l: isize) -> isize {
    // SAFETY: 自分の編集欄への同期メッセージ（ポインタを渡さない読み出し）。
    unsafe { SendMessageW(app.editor.hwnd(), msg, Some(WPARAM(w)), Some(LPARAM(l))).0 }
}

pub(super) fn run(ctx: &Ctx) -> Check {
    // 前の試験が壊した設定ファイルを捨てる（壊れていると保存しない）。
    let _ = std::fs::remove_file(ctx.paths.settings_file());
    step(|app| app.settings_state = super::super::fileio::SettingsState::Missing)?;
    menu_and_keys()?;
    wrapping(ctx)?;
    toggles_and_tabs()?;
    zoom_and_font(ctx)?;
    auto_indent()?;
    big_view_line_numbers(ctx)?;
    persistence(ctx)?;
    // 後の試験に影響しないよう、既定の表示に戻す。
    step(|app| {
        app.update_view(|v| *v = Default::default());
    })?;
    Ok(())
}

fn menu_and_keys() -> Check {
    step(|app| -> Check {
        let key = |s: &str| app.keymap().command_for(&KeyChord::parse(s).unwrap());
        check!(
            key("Ctrl+Plus") == Some(CommandId::new("view.zoom_in")),
            "Ctrl+Plus"
        );
        check!(
            key("Ctrl+Minus") == Some(CommandId::new("view.zoom_out")),
            "Ctrl+Minus"
        );
        check!(
            key("Ctrl+0") == Some(CommandId::new("view.zoom_reset")),
            "Ctrl+0"
        );
        let entries = app.menu().ok_or("メニューがない")?.entries().to_vec();
        for command in [
            "view.font",
            "view.zoom_in",
            "view.zoom_out",
            "view.zoom_reset",
            "view.toggle_whitespace",
            "view.toggle_eol",
            "view.toggle_line_numbers",
            "view.toggle_current_line",
            "view.toggle_braces",
            "view.toggle_edge_guide",
            "view.toggle_tab_spaces",
            "view.toggle_auto_indent",
        ] {
            check!(
                entries.iter().any(|e| e.command == CommandId::new(command)),
                "メニューに {command} がない"
            );
        }
        let wraps = entries
            .iter()
            .filter(|e| e.command == CommandId::new("view.wrap"))
            .count();
        check!(wraps == 4, "折り返しの項目 {wraps}");
        let tabs = entries
            .iter()
            .filter(|e| e.command == CommandId::new("view.tab_width"))
            .count();
        check!(tabs == 3, "タブ幅の項目 {tabs}");
        // 印の状態（既定: 折り返さない、4、行番号あり）。
        let checked = |command: &'static str, args: Value| {
            app.view_menu_checked(CommandId::new(command), &args)
        };
        check!(
            checked("view.wrap", json!({ "mode": "none" })) == Some(true),
            "折り返さないに印"
        );
        check!(
            checked("view.wrap", json!({ "mode": "word" })) == Some(false),
            "単語単位に印なし"
        );
        check!(
            checked("view.tab_width", json!({ "width": "4" })) == Some(true),
            "タブ幅 4 に印"
        );
        check!(
            checked("view.toggle_line_numbers", Value::Null) == Some(true),
            "行番号に印"
        );
        check!(
            checked("file.save", Value::Null).is_none(),
            "対象外は印なし"
        );
        Ok(())
    })?
}

/// 折り返し（なし／単語／文字／桁指定）と桁指定の入力。
fn wrapping(ctx: &Ctx) -> Check {
    cmd("file.new", Value::Null)?;
    let id = step(|app| app.active_id().unwrap())?;
    step(|app| app.with_text(id, |ed| ed.set_text(&"0".repeat(41))))?.map_err(super::e2s)?;

    step(|app| -> Check {
        check!(sci(app, SCI_GETWRAPMODE, 0, 0) == 0, "既定は折り返さない");
        check!(sci(app, SCI_WRAPCOUNT, 0, 0) == 1, "折り返さない");
        Ok(())
    })??;
    cmd("view.wrap", json!({ "mode": "char" }))?;
    step(|app| -> Check {
        check!(app.settings().view.wrap == WrapMode::Char, "設定: 文字単位");
        check!(sci(app, SCI_GETWRAPMODE, 0, 0) == 2, "文字単位");
        Ok(())
    })??;
    cmd("view.wrap", json!({ "mode": "word" }))?;
    step(|app| -> Check {
        check!(sci(app, SCI_GETWRAPMODE, 0, 0) == 1, "単語単位");
        check!(
            sci(app, SCI_GETMARGINRIGHT, 0, 0) == 0,
            "桁指定でなければ右余白なし"
        );
        Ok(())
    })??;

    // 桁指定: 入力欄で 40 桁。40 文字は 1 行、41 文字は 2 行。
    let client_width = step(|app| {
        let mut r = RECT::default();
        // SAFETY: 自分の編集欄の領域を読む。
        unsafe {
            let _ = GetClientRect(app.editor.hwnd(), &mut r);
        }
        r.right - r.left
    })?;
    ctx.prompts.input.borrow_mut().push_back(Some("40".into()));
    cmd("view.wrap", json!({ "mode": "column" }))?;
    step(|app| -> Check {
        let v = &app.settings().view;
        check!(
            v.wrap == WrapMode::Column && v.wrap_column == 40,
            "設定: 40 桁 {v:?}"
        );
        check!(
            sci(app, SCI_GETWRAPMODE, 0, 0) == 1,
            "桁指定は単語単位で折る"
        );
        check!(sci(app, SCI_GETEDGEMODE, 0, 0) == 1, "ガイド線");
        check!(sci(app, SCI_GETEDGECOLUMN, 0, 0) == 40, "ガイド線の桁");
        if client_width > 900 {
            check!(
                sci(app, SCI_GETMARGINRIGHT, 0, 0) > 0,
                "右余白（編集欄の幅 {client_width}）"
            );
        }
        Ok(())
    })??;
    if client_width > 900 {
        // 折り返しの計算は描画のときに行われるので、メッセージを回して待つ。
        wait("41 文字が 2 行に折り返される", |app| {
            sci(app, SCI_WRAPCOUNT, 0, 0) == 2
        })?;
        step(|app| app.with_text(id, |ed| ed.set_text(&"0".repeat(40))))?.map_err(super::e2s)?;
        wait("40 文字は 1 行に収まる", |app| {
            sci(app, SCI_WRAPCOUNT, 0, 0) == 1
        })?;
        // 余裕の確認: 40 文字の行が折り返されない状態が、少し待っても続く。
        step(|_| std::thread::sleep(std::time::Duration::from_millis(100)))?;
        step(|app| -> Check {
            check!(sci(app, SCI_WRAPCOUNT, 0, 0) == 1, "40 文字は 1 行のまま");
            Ok(())
        })??;
    }
    // ガイド線の切り替え。
    cmd("view.toggle_edge_guide", Value::Null)?;
    step(|app| -> Check {
        check!(sci(app, SCI_GETEDGEMODE, 0, 0) == 0, "ガイド線を消す");
        Ok(())
    })??;
    cmd("view.toggle_edge_guide", json!({ "enabled": true }))?;

    // 入力の取り消し・範囲外・引数での指定。
    cmd("view.wrap", json!({ "mode": "column" }))?; // 台本が尽きている（取り消し）
    ctx.prompts.input.borrow_mut().push_back(Some("5".into()));
    cmd("view.wrap", json!({ "mode": "column" }))?;
    ctx.prompts.input.borrow_mut().push_back(Some("abc".into()));
    cmd("view.wrap", json!({ "mode": "column" }))?;
    step(|app| -> Check {
        check!(
            app.settings().view.wrap_column == 40,
            "取り消し・範囲外・不正な入力では変えない"
        );
        Ok(())
    })??;
    cmd("view.wrap", json!({ "mode": "column", "column": 100 }))?;
    step(|app| -> Check {
        check!(app.settings().view.wrap_column == 100, "引数で桁数を指定");
        check!(sci(app, SCI_GETEDGECOLUMN, 0, 0) == 100, "ガイド線の桁 100");
        Ok(())
    })??;
    cmd("view.wrap", json!({ "mode": "none" }))?;
    step(|app| -> Check {
        check!(sci(app, SCI_GETWRAPMODE, 0, 0) == 0, "折り返しを戻す");
        check!(sci(app, SCI_GETMARGINRIGHT, 0, 0) == 0, "右余白を戻す");
        check!(
            sci(app, SCI_GETEDGEMODE, 0, 0) == 0,
            "桁指定でなければガイド線なし"
        );
        Ok(())
    })??;
    step(move |app| app.close_document(id, true))?.map_err(super::e2s)?;
    Ok(())
}

/// 空白・改行・現在行の切り替え、タブ幅、タブをスペースで（新しいタブにも適用）。
fn toggles_and_tabs() -> Check {
    cmd("view.toggle_whitespace", Value::Null)?;
    cmd("view.toggle_eol", Value::Null)?;
    cmd("view.toggle_current_line", json!({ "enabled": false }))?;
    step(|app| -> Check {
        check!(sci(app, SCI_GETVIEWWS, 0, 0) == 1, "空白を表示");
        check!(sci(app, SCI_GETVIEWEOL, 0, 0) == 1, "改行を表示");
        check!(
            sci(app, SCI_GETCARETLINEVISIBLE, 0, 0) == 0,
            "現在行の強調を切る"
        );
        let v = &app.settings().view;
        check!(
            v.show_whitespace && v.show_eol && !v.highlight_current_line,
            "設定"
        );
        Ok(())
    })??;
    cmd("view.toggle_whitespace", Value::Null)?;
    cmd("view.toggle_eol", Value::Null)?;
    cmd("view.toggle_current_line", Value::Null)?;
    step(|app| -> Check {
        check!(sci(app, SCI_GETVIEWWS, 0, 0) == 0, "空白を隠す");
        check!(sci(app, SCI_GETVIEWEOL, 0, 0) == 0, "改行を隠す");
        check!(sci(app, SCI_GETCARETLINEVISIBLE, 0, 0) == 1, "現在行の強調");
        Ok(())
    })??;

    cmd("view.tab_width", json!({ "width": "8" }))?;
    cmd("view.toggle_tab_spaces", Value::Null)?;
    step(|app| -> Check {
        check!(sci(app, SCI_GETTABWIDTH, 0, 0) == 8, "タブ幅 8");
        check!(sci(app, SCI_GETUSETABS, 0, 0) == 0, "スペースで入力");
        Ok(())
    })??;
    // 別の文書に切り替えても同じ（ドキュメントごとの設定を付け直す）。
    cmd("file.new", Value::Null)?;
    step(|app| -> Check {
        check!(
            sci(app, SCI_GETTABWIDTH, 0, 0) == 8,
            "新しいタブでもタブ幅 8"
        );
        check!(
            sci(app, SCI_GETUSETABS, 0, 0) == 0,
            "新しいタブでもスペース"
        );
        Ok(())
    })??;
    let id = step(|app| app.active_id().unwrap())?;
    step(move |app| app.close_document(id, true))?.map_err(super::e2s)?;
    cmd("view.tab_width", json!({ "width": "4" }))?;
    cmd("view.toggle_tab_spaces", Value::Null)?;
    step(|app| -> Check {
        check!(sci(app, SCI_GETTABWIDTH, 0, 0) == 4, "タブ幅を戻す");
        check!(sci(app, SCI_GETUSETABS, 0, 0) == 1, "タブ文字に戻す");
        Ok(())
    })?
}

/// 拡大・縮小（コマンドと、編集欄側の変更の取り込み）、書体と文字サイズ。
fn zoom_and_font(ctx: &Ctx) -> Check {
    cmd("view.zoom_in", Value::Null)?;
    cmd("view.zoom_in", Value::Null)?;
    step(|app| -> Check {
        check!(app.settings().view.zoom == 2, "拡大 2");
        check!(sci(app, SCI_GETZOOM, 0, 0) == 2, "編集欄の拡大 2");
        Ok(())
    })??;
    cmd("view.zoom_out", Value::Null)?;
    step(|app| -> Check {
        check!(app.settings().view.zoom == 1, "縮小");
        Ok(())
    })??;
    // Ctrl+ホイール相当（編集欄が自分で拡大率を変える）→ 通知で設定に取り込まれる。
    step(|app| sci(app, SCI_SETZOOM, 5, 0))?;
    step(|app| -> Check {
        check!(
            app.settings().view.zoom == 5,
            "編集欄の変更を設定に取り込む"
        );
        Ok(())
    })??;
    cmd("view.zoom_reset", Value::Null)?;
    step(|app| -> Check {
        check!(
            app.settings().view.zoom == 0 && sci(app, SCI_GETZOOM, 0, 0) == 0,
            "拡大率を戻す"
        );
        Ok(())
    })??;

    // 書体と大きさ（引数）。
    cmd("view.font", json!({ "font": "Consolas", "size": 14 }))?;
    step(|app| -> Check {
        let v = &app.settings().view;
        check!(v.font == "Consolas" && v.font_size == 14, "設定: {v:?}");
        let size = sci(app, SCI_STYLEGETSIZEFRACTIONAL, STYLE_DEFAULT, 0);
        check!(size == 1400, "編集欄の文字サイズ {size}");
        Ok(())
    })??;
    // 範囲外は変えない。
    cmd("view.font", json!({ "size": 3 }))?;
    step(|app| -> Check {
        check!(
            app.settings().view.font_size == 14,
            "範囲外のサイズは変えない"
        );
        Ok(())
    })??;
    // ダイアログ（台本）。取り消しでは変えない。
    cmd("view.font", Value::Null)?;
    ctx.prompts
        .font
        .borrow_mut()
        .push_back(Some(("MS Gothic".into(), 12)));
    cmd("view.font", Value::Null)?;
    step(|app| -> Check {
        let v = &app.settings().view;
        check!(
            v.font == "MS Gothic" && v.font_size == 12,
            "ダイアログの結果 {v:?}"
        );
        Ok(())
    })??;
    // 既定に戻す。
    cmd("view.font", json!({ "font": "" }))?;
    step(|app| -> Check {
        let v = &app.settings().view;
        check!(v.font.is_empty() && v.font_size == 11, "既定に戻す {v:?}");
        check!(
            sci(app, SCI_STYLEGETSIZEFRACTIONAL, STYLE_DEFAULT, 0) == 1100,
            "文字サイズを戻す"
        );
        Ok(())
    })?
}

/// 改行で前の行の行頭の空白を引き継ぐ（自動インデント。切ると引き継がない）。
fn auto_indent() -> Check {
    cmd("file.new", Value::Null)?;
    let id = step(|app| app.active_id().unwrap())?;
    step(|app| {
        app.with_text(id, |ed| {
            ed.set_text("  \tfoo");
            let end = ed.length();
            ed.set_selection(crate::document::Selection {
                anchor: end,
                caret: end,
            });
        })
    })?
    .map_err(super::e2s)?;
    step(|app| sci(app, SCI_NEWLINE, 0, 0))?;
    step(|app| -> Check {
        let text = text_of(app, id);
        check!(
            text.ends_with("foo\r\n  \t") || text.ends_with("foo\n  \t"),
            "インデントを引き継ぐ {text:?}"
        );
        Ok(())
    })??;
    cmd("view.toggle_auto_indent", Value::Null)?;
    step(|app| {
        app.with_text(id, |ed| {
            ed.set_text("  foo");
            let end = ed.length();
            ed.set_selection(crate::document::Selection {
                anchor: end,
                caret: end,
            });
        })
    })?
    .map_err(super::e2s)?;
    step(|app| sci(app, SCI_NEWLINE, 0, 0))?;
    step(|app| -> Check {
        let text = text_of(app, id);
        check!(
            text.ends_with("foo\r\n") || text.ends_with("foo\n"),
            "切ると引き継がない {text:?}"
        );
        Ok(())
    })??;
    cmd("view.toggle_auto_indent", Value::Null)?;
    step(move |app| app.close_document(id, true))?.map_err(super::e2s)?;
    Ok(())
}

/// 行番号の切り替えは、巨大ファイルのビューにも適用される（開いた後の切り替えと、
/// 隠したまま開いたビュー）。
fn big_view_line_numbers(ctx: &Ctx) -> Check {
    step(|app| {
        app.large_thresholds = LargeModeThresholds {
            max_bytes: 16 * 1024,
            max_lines: 1_000_000,
            max_line_bytes: 1 << 20,
        }
    })?;
    let path = ctx.dir.join("u2-big.log");
    write_numbered_lines(&path, 3000)?;
    cmd("file.open", json!({ "path": path.to_string_lossy() }))?;
    let p = path.clone();
    wait("巨大ファイルが開く", move |app| {
        find_big(app, &p).is_some()
    })?;
    let id = step(|app| find_big(app, &path).unwrap())?;
    let with_numbers = step(|app| app.bigviews[&id].view.gutter_width())?;
    check!(with_numbers > 20, "行番号の欄がある {with_numbers}");
    cmd("view.toggle_line_numbers", Value::Null)?;
    step(|app| -> Check {
        check!(!app.settings().view.line_numbers, "設定: 行番号なし");
        let w = app.bigviews[&id].view.gutter_width();
        check!(w < with_numbers && w < 20, "巨大ファイルの行番号を隠す {w}");
        check!(sci(app, 2243, 0, 0) == 0, "編集欄の行番号を隠す");
        Ok(())
    })??;
    // 隠したまま別の巨大ファイルを開いても隠れている。
    let path2 = ctx.dir.join("u2-big2.log");
    write_numbered_lines(&path2, 3000)?;
    cmd("file.open", json!({ "path": path2.to_string_lossy() }))?;
    let p2 = path2.clone();
    wait("2 つ目の巨大ファイルが開く", move |app| {
        find_big(app, &p2).is_some()
    })?;
    let id2 = step(|app| find_big(app, &path2).unwrap())?;
    step(|app| -> Check {
        check!(
            app.bigviews[&id2].view.gutter_width() < 20,
            "新しいビューも行番号なし"
        );
        Ok(())
    })??;
    cmd("view.toggle_line_numbers", Value::Null)?;
    step(|app| -> Check {
        check!(
            app.bigviews[&id].view.gutter_width() == with_numbers
                && app.bigviews[&id2].view.gutter_width() == with_numbers,
            "行番号を戻す"
        );
        check!(sci(app, 2243, 0, 0) > 0, "編集欄の行番号を戻す");
        Ok(())
    })??;
    for close in [id2, id] {
        step(move |app| app.close_document(close, true))?.map_err(super::e2s)?;
    }
    step(|app| app.large_thresholds = LargeModeThresholds::DEFAULT)?;
    Ok(())
}

/// 設定ファイルに保存され、読み直すと同じになる。
fn persistence(ctx: &Ctx) -> Check {
    cmd("view.wrap", json!({ "mode": "column", "column": 72 }))?;
    cmd("view.font", json!({ "font": "Consolas", "size": 13 }))?;
    cmd("view.toggle_whitespace", Value::Null)?;
    cmd("view.tab_width", json!({ "width": "2" }))?;
    cmd("view.zoom_in", Value::Null)?;
    let expected = step(|app| app.settings().view.clone())?;
    let file = ctx.paths.settings_file();
    let shown = file.clone();
    wait("設定ファイルに保存される", move |_| {
        std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| Settings::from_json(&t).ok())
            .is_some_and(|s| s.view == expected)
    })
    .map_err(|e| format!("{e} {:?}", std::fs::read_to_string(&shown)))?;
    Ok(())
}
