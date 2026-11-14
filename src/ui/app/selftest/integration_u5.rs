//! 改善 U5 の自己診断: 印刷のコマンドの有効・無効（巨大ファイルは対象外で案内を出す）、
//! 印刷の選択肢の切り替えと保存、メモリ上のビットマップの DC へのページ区切りの計算と描画
//! （余白、ヘッダー・フッター、ページ範囲、行番号・色の設定を戻すこと）。
//! プリンターへの出力（印刷ダイアログ、ページ設定、用紙）は Windows 実機で確認する
//! （`docs/verification/U.md`）。

use serde_json::json;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetPixel, PatBlt,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC, WHITENESS,
};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::SendMessageW;

use super::integration::{cmd, step, wait};
use super::integration_i2::{find_big, write_numbered_lines};
use super::{Check, Ctx};
use crate::appdata::settings::{PrintSettings, Settings};
use crate::bigfile::LargeModeThresholds;
use crate::command::{CommandId, Invocation, KeyChord, Value};
use crate::printing::{page_jobs, page_layout, PageLayout, PageMetrics, PageSelection};
use crate::ui::app::App;
use crate::ui::print::{draw_page, header_line_height, local_timestamp, paginate_range};

const SCI_GETMARGINWIDTHN: u32 = 2243;

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!()));
        }
    };
}

pub(super) fn run(ctx: &Ctx) -> Check {
    // 前の試験が壊した設定ファイルを捨てる（壊れていると保存しない）。
    let _ = std::fs::remove_file(ctx.paths.settings_file());
    step(|app| app.settings_state = super::super::fileio::SettingsState::Missing)?;
    availability_and_guidance(ctx)?;
    toggles_are_saved(ctx)?;
    paginate_and_draw()?;
    Ok(())
}

fn sci_margin(app: &App) -> isize {
    // SAFETY: 自分の編集欄への同期メッセージ（ポインタを渡さない読み出し）。
    unsafe {
        SendMessageW(
            app.editor().hwnd(),
            SCI_GETMARGINWIDTHN,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        )
        .0
    }
}

fn available(id: &'static str) -> Result<bool, String> {
    step(|app| {
        let registry = app.registry();
        registry
            .check_available(app, &Invocation::user(CommandId::new(id)))
            .is_ok()
    })
}

/// 印刷は編集欄の文書で有効、巨大ファイルでは無効（キーで呼ぶと案内）。ページ設定は常に有効。
fn availability_and_guidance(ctx: &Ctx) -> Check {
    cmd("file.new", Value::Null)?;
    check!(available("file.print")?, "通常の文書は印刷できる");
    check!(available("file.page_setup")?, "ページ設定は使える");
    let key = step(|app| {
        app.keymap()
            .command_for(&KeyChord::parse("Ctrl+P").unwrap())
    })?;
    check!(
        key == Some(CommandId::new("file.print")),
        "印刷は Ctrl+P: {key:?}"
    );

    step(|app| {
        app.large_thresholds = LargeModeThresholds {
            max_bytes: 16 * 1024,
            max_lines: 1_000_000,
            max_line_bytes: 1 << 20,
        }
    })?;
    let path = ctx.dir.join("u5-big.log");
    write_numbered_lines(&path, 3000)?;
    cmd("file.open", json!({ "path": path.to_string_lossy() }))?;
    let p = path.clone();
    wait("巨大ファイルが開く", move |app| {
        find_big(app, &p).is_some()
    })?;
    check!(!available("file.print")?, "巨大ファイルは印刷できない");
    check!(
        available("file.page_setup")?,
        "巨大ファイルでもページ設定は使える"
    );
    step(|app| app.run_command(CommandId::new("file.print"), Value::Null))?;
    let status = step(|app| app.status_message().map(str::to_owned))?;
    check!(
        status
            .as_deref()
            .is_some_and(|s| s.contains("巨大ファイル") && s.contains("印刷できません")),
        "案内が出る: {status:?}"
    );
    let id = step(|app| find_big(app, &path).unwrap())?;
    step(move |app| app.close_document(id, true))?.map_err(super::e2s)?;
    step(|app| app.large_thresholds = LargeModeThresholds::DEFAULT)?;
    Ok(())
}

/// 印刷の選択肢の切り替えが設定に入り、設定ファイルに保存される。
fn toggles_are_saved(ctx: &Ctx) -> Check {
    let before = step(|app| app.settings().print.clone())?;
    check!(before == PrintSettings::default(), "既定の印刷設定");
    cmd("print.toggle_color", Value::Null)?;
    cmd("print.toggle_line_numbers", Value::Null)?;
    cmd("print.toggle_header", json!({ "enabled": false }))?;
    cmd("print.toggle_footer", Value::Null)?;
    let expected = step(|app| app.settings().print.clone())?;
    check!(
        !expected.color && expected.line_numbers && !expected.header && !expected.footer,
        "切り替えが反映される {expected:?}"
    );
    let checked = step(|app| {
        [
            "print.toggle_color",
            "print.toggle_line_numbers",
            "print.toggle_header",
        ]
        .map(|c| app.print_menu_checked(CommandId::new(c)))
    })?;
    check!(
        checked == [Some(false), Some(true), Some(false)],
        "メニューのチェック {checked:?}"
    );
    let file = ctx.paths.settings_file();
    let want = expected.clone();
    wait("印刷設定がファイルに保存される", move |_| {
        std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| Settings::from_json(&t).ok())
            .is_some_and(|s| s.print == want)
    })?;
    // 元に戻す（後の試験に影響させない）。
    cmd("print.toggle_color", Value::Null)?;
    cmd("print.toggle_line_numbers", Value::Null)?;
    cmd("print.toggle_header", Value::Null)?;
    cmd("print.toggle_footer", Value::Null)?;
    check!(
        step(|app| app.settings().print.clone())? == PrintSettings::default(),
        "元に戻る"
    );
    Ok(())
}

/// A4 を 96 dpi で見たビットマップ（794 x 1123）。
const PAGE_W: i32 = 794;
const PAGE_H: i32 = 1123;

fn metrics() -> PageMetrics {
    PageMetrics {
        paper_w: PAGE_W,
        paper_h: PAGE_H,
        printable_w: PAGE_W,
        printable_h: PAGE_H,
        offset_x: 0,
        offset_y: 0,
        dpi_x: 96,
        dpi_y: 96,
    }
}

struct MemDc {
    screen: HDC,
    dc: HDC,
    bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
    old: windows::Win32::Graphics::Gdi::HGDIOBJ,
}

impl MemDc {
    fn new() -> Result<Self, String> {
        // SAFETY: 画面の DC と互換のメモリ DC・ビットマップを作り、Drop で片付ける。
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: PAGE_W,
                    biHeight: -PAGE_H,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
                .unwrap_or_default();
            if dc.is_invalid() || bitmap.is_invalid() {
                return Err("メモリ DC を作れません".into());
            }
            let old = SelectObject(dc, bitmap.into());
            let me = MemDc {
                screen,
                dc,
                bitmap,
                old,
            };
            me.clear();
            Ok(me)
        }
    }

    fn clear(&self) {
        // SAFETY: 自分の DC。
        unsafe {
            let _ = PatBlt(self.dc, 0, 0, PAGE_W, PAGE_H, WHITENESS);
        }
    }

    /// 矩形のどこかに白でない画素があるか。
    fn has_ink(&self, left: i32, top: i32, right: i32, bottom: i32) -> bool {
        for y in top..bottom {
            for x in (left..right).step_by(2) {
                // SAFETY: 自分の DC の範囲内。
                let c = unsafe { GetPixel(self.dc, x, y) }.0;
                if c & 0x00FF_FFFF != 0x00FF_FFFF {
                    return true;
                }
            }
        }
        false
    }
}

impl Drop for MemDc {
    fn drop(&mut self) {
        // SAFETY: 作ったものを元の状態に戻して捨てる。
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
            ReleaseDC(None, self.screen);
        }
    }
}

fn layout(dc: HDC, header: bool, footer: bool) -> Result<PageLayout, String> {
    page_layout(
        metrics(),
        PrintSettings::default().margins,
        header,
        footer,
        header_line_height(dc),
    )
    .ok_or_else(|| "配置を作れません".to_owned())
}

fn pages_of(app: &App, dc: HDC, layout: &PageLayout, settings: &PrintSettings) -> Vec<u64> {
    let ed = app.editor();
    let guard = ed.begin_print(crate::ui::print::print_params(settings));
    paginate_range(&guard, dc, layout, 0, ed.length())
}

fn paginate_and_draw() -> Check {
    cmd("file.new", Value::Null)?;
    let id = step(|app| app.active_id().unwrap())?;
    let mut text = String::new();
    for i in 1..=300 {
        text.push_str(&format!(
            "行 {i}: the quick brown fox jumps over the lazy dog\n"
        ));
    }
    // 紙の幅を超える行（折り返して印刷される）。
    text.push_str(&"long ".repeat(200));
    text.push('\n');
    step(|app| app.with_text(id, |ed| ed.set_text(&text)))?.map_err(super::e2s)?;
    let mem = MemDc::new()?;
    let settings = PrintSettings::default();
    let numbers = PrintSettings {
        line_numbers: true,
        ..settings.clone()
    };

    step(|app| -> Check {
        let len = app.editor().length();
        let margin_before = sci_margin(app);
        let full = layout(mem.dc, true, true)?;
        let starts = pages_of(app, mem.dc, &full, &settings);
        check!(starts.len() >= 3, "複数ページになる {}", starts.len());
        check!(starts[0] == 0, "最初のページは先頭から");
        check!(
            starts.windows(2).all(|w| w[0] < w[1]) && *starts.last().unwrap() < len,
            "区切りは単調に増える {starts:?}"
        );
        // ヘッダー・フッターを省くと本文の領域が広がり、ページ数は増えない。
        let bare = layout(mem.dc, false, false)?;
        let bare_pages = pages_of(app, mem.dc, &bare, &settings).len();
        check!(bare_pages <= starts.len(), "ヘッダーなしの方が少ないか同じ");
        // 行番号を付けても、同じ区切りの計算ができる。
        let with_numbers = pages_of(app, mem.dc, &full, &numbers);
        check!(
            with_numbers.len() >= starts.len(),
            "行番号つきでも計算できる"
        );
        // 印刷中だけ変え、終わると元に戻る。
        check!(sci_margin(app) == margin_before, "行番号の余白を元に戻す");

        // ページ範囲: 2 ページ目だけ。
        let jobs = page_jobs(&starts, len, PageSelection::Pages { from: 2, to: 2 });
        check!(
            jobs.len() == 1 && jobs[0].start == starts[1],
            "2 ページ目だけ {jobs:?}"
        );

        // 1 ページ目を描く: 本文、ヘッダー、フッターに描画があり、余白は白のまま。
        let all = page_jobs(&starts, len, PageSelection::All);
        let total = all.len() as u32;
        let guard = app
            .editor()
            .begin_print(crate::ui::print::print_params(&settings));
        draw_page(
            &guard,
            mem.dc,
            &full,
            all[0],
            total,
            "試験.txt",
            local_timestamp(),
        );
        let b = full.body;
        check!(
            mem.has_ink(b.left, b.top, b.right, b.top + b.height() / 2),
            "本文が描かれる"
        );
        let h = full.header.unwrap();
        check!(
            mem.has_ink(h.left, h.bottom, h.right, h.bottom + h.height() / 4),
            "ヘッダーの罫線が描かれる"
        );
        let f = full.footer.unwrap();
        check!(
            mem.has_ink(f.left, f.top - f.height() / 4, f.right, f.top),
            "フッターの罫線が描かれる"
        );
        check!(
            !mem.has_ink(0, 0, b.left - 4, PAGE_H) && !mem.has_ink(b.right + 4, 0, PAGE_W, PAGE_H),
            "左右の余白は白"
        );
        check!(!mem.has_ink(0, 0, PAGE_W, h.top - 4), "上の余白は白");
        // ヘッダーの文字が（Scintilla が変える DC の状態に関わらず）ヘッダーの帯の中に出る。
        check!(
            mem.has_ink(h.left, h.top, h.left + 200, h.bottom),
            "ヘッダーの文字が帯の中にある（Wine に字形がなければこの確認は外す）"
        );
        // 最後のページも描ける（範囲の終端まで）。
        mem.clear();
        draw_page(
            &guard,
            mem.dc,
            &full,
            *all.last().unwrap(),
            total,
            "試験.txt",
            local_timestamp(),
        );
        check!(
            mem.has_ink(b.left, b.top, b.right, b.top + 40),
            "最後のページ"
        );
        Ok(())
    })??;

    step(|app| app.close_document(id, true))?.map_err(super::e2s)?;
    Ok(())
}
