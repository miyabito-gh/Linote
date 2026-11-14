//! 印刷（Win32）: 印刷ダイアログ、ページ設定、プリンターの DC への出力。
//!
//! 本文は Scintilla の `SCI_FORMATRANGEFULL` で描く（[`crate::ui::editor::printing`]）。
//! ページの区切りは先に（描かずに）計算し、ページ範囲と「n / m」に使う。区切りと
//! ヘッダー・フッターの文字列の組み立ては [`crate::printing`] の純粋な関数。
//!
//! DC に依存する部分（配置、ページ区切り、1 ページの描画）は、プリンターでもメモリ DC
//! でも動くように関数に分けてある（自己診断はメモリ DC で試す）。
//!
//! 印刷は UI スレッドで同期的に行う（Scintilla の操作は編集欄のスレッドでしかできない）。

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GlobalFree, HGLOBAL, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteDC, DeleteObject, DrawTextW, GetDeviceCaps, LineTo, MoveToEx, SelectObject,
    SetBkMode, SetTextAlign, SetTextColor, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH,
    DEFAULT_QUALITY, DEVMODEW, DM_ORIENTATION, DM_PAPERSIZE, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FW_NORMAL, HDC, HFONT, HORZRES, LOGPIXELSX,
    LOGPIXELSY, OUT_DEFAULT_PRECIS, PHYSICALHEIGHT, PHYSICALOFFSETX, PHYSICALOFFSETY,
    PHYSICALWIDTH, TA_LEFT, TA_TOP, TRANSPARENT, VERTRES,
};
use windows::Win32::Storage::Xps::{AbortDoc, EndDoc, EndPage, StartDocW, StartPage, DOCINFOW};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, PageSetupDlgW, PrintDlgW, PAGESETUPDLGW, PD_NOSELECTION, PD_PAGENUMS,
    PD_RETURNDC, PD_RETURNDEFAULT, PD_SELECTION, PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGW,
    PSD_INHUNDREDTHSOFMILLIMETERS, PSD_MARGINS,
};

use crate::appdata::settings::PrintSettings;
use crate::core::{Error, Result};
use crate::platform::system::to_wide;
use crate::printing::{
    footer_text, header_texts, page_jobs, page_layout, paginate, PageJob, PageLayout, PageMetrics,
    PageSelection, Timestamp,
};
use crate::ui::editor::printing::{PrintGuard, PrintParams};
use crate::ui::editor::Editor;

/// ヘッダー・フッターの文字サイズ（ポイント）。
const HEADER_POINTS: i32 = 9;

/// 印刷の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintOutcome {
    /// 利用者が印刷ダイアログで取り消した。
    Cancelled,
    /// このページ数を送った。
    Printed { pages: u32 },
}

/// 印刷する文書の情報。
pub struct PrintRequest<'a> {
    /// ヘッダーに出す名前（ファイル名またはタブ名）。
    pub title: &'a str,
    pub settings: &'a PrintSettings,
    /// 選択範囲（空でなければ `Some`）。
    pub selection: Option<(u64, u64)>,
    /// 本文の長さ（バイト）。
    pub doc_len: u64,
}

pub fn print_params(settings: &PrintSettings) -> PrintParams {
    PrintParams {
        color: settings.color,
        line_numbers: settings.line_numbers,
    }
}

/// 今の日時（ローカル）。
pub fn local_timestamp() -> Timestamp {
    // SAFETY: 引数なし。
    let t = unsafe { GetLocalTime() };
    Timestamp {
        year: t.wYear,
        month: t.wMonth as u8,
        day: t.wDay as u8,
        hour: t.wHour as u8,
        minute: t.wMinute as u8,
    }
}

// ---------------------------------------------------------------- DC に依存する計算

/// DC の紙の寸法。
pub fn metrics_of(hdc: HDC) -> PageMetrics {
    let caps = |i| {
        // SAFETY: 有効な DC への問い合わせ。
        unsafe { GetDeviceCaps(Some(hdc), i) }
    };
    PageMetrics {
        paper_w: caps(PHYSICALWIDTH),
        paper_h: caps(PHYSICALHEIGHT),
        printable_w: caps(HORZRES),
        printable_h: caps(VERTRES),
        offset_x: caps(PHYSICALOFFSETX),
        offset_y: caps(PHYSICALOFFSETY),
        dpi_x: caps(LOGPIXELSX).max(1),
        dpi_y: caps(LOGPIXELSY).max(1),
    }
}

fn header_font(hdc: HDC) -> HFONT {
    // SAFETY: 有効な DC への問い合わせと、フォントの作成。
    unsafe {
        let dpi = GetDeviceCaps(Some(hdc), LOGPIXELSY).max(1);
        let face = to_wide("Yu Gothic UI");
        CreateFontW(
            -(HEADER_POINTS * dpi / 72),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            DEFAULT_QUALITY,
            DEFAULT_PITCH.0 as u32,
            PCWSTR(face.as_ptr()),
        )
    }
}

/// ヘッダー・フッターの 1 行の高さ（デバイスの単位）。
pub fn header_line_height(hdc: HDC) -> i32 {
    // SAFETY: 有効な DC。
    let dpi = unsafe { GetDeviceCaps(Some(hdc), LOGPIXELSY) }.max(1);
    HEADER_POINTS * dpi / 72 * 5 / 4
}

/// 設定の余白とヘッダー・フッターから、DC に対する配置を求める。
pub fn layout_for(hdc: HDC, settings: &PrintSettings) -> Result<PageLayout> {
    page_layout(
        metrics_of(hdc),
        settings.margins,
        settings.header,
        settings.footer,
        header_line_height(hdc),
    )
    .ok_or_else(|| {
        Error::unavailable(
            "印刷",
            "余白が大きすぎて、本文を印刷する領域がありません。ページ設定で余白を小さくしてください",
        )
    })
}

/// `start..end` のページの区切り（各ページの先頭位置）を、描かずに求める。
pub fn paginate_range(
    guard: &PrintGuard<'_>,
    hdc: HDC,
    layout: &PageLayout,
    start: u64,
    end: u64,
) -> Vec<u64> {
    paginate(start, end, |pos| {
        guard.format_page(hdc, layout, pos, end, false)
    })
}

fn draw_band(hdc: HDC, rect: crate::printing::Rect, text: &str, align: u32) {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    if wide.is_empty() {
        return;
    }
    let mut r = RECT {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    };
    // SAFETY: 有効な DC と矩形。
    unsafe {
        DrawTextW(
            hdc,
            &mut wide,
            &mut r,
            windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
                align | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0 | DT_END_ELLIPSIS.0,
            ),
        );
        let _ = windows::Win32::Graphics::Gdi::TextOutW(
            hdc,
            r.left,
            r.top,
            &text.encode_utf16().collect::<Vec<_>>(),
        );
    }
}

/// 1 ページ（ヘッダー、本文、フッター）を描く。`hdc` は `StartPage` 済みの DC か、メモリ DC。
#[allow(clippy::too_many_arguments)]
pub fn draw_page(
    guard: &PrintGuard<'_>,
    hdc: HDC,
    layout: &PageLayout,
    job: PageJob,
    total: u32,
    title: &str,
    now: Timestamp,
) {
    let font = header_font(hdc);
    // SAFETY: 作った（または既定の）フォントを選んで描き、元に戻して捨てる。
    unsafe {
        let old = SelectObject(hdc, font.into());
        // Scintilla は DC の文字の基準位置を「ベースライン」にして戻さないので、
        // ヘッダー・フッターを描く前に上端に直す。
        SetTextAlign(hdc, TA_LEFT | TA_TOP);
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(0));
        if let Some(h) = layout.header {
            let (left, right) = header_texts(title, now);
            // 日時を先に右へ、ファイル名は残りの左側へ（長ければ省略記号）。
            draw_band(hdc, h, &right, DT_RIGHT.0);
            let mut name_area = h;
            let date_w = h.width().min(h.height() * 14);
            name_area.right -= date_w;
            draw_band(hdc, name_area, &left, DT_LEFT.0);
            let y = h.bottom + h.height() / 8;
            let _ = MoveToEx(hdc, h.left, y, None);
            let _ = LineTo(hdc, h.right, y);
        }
        if let Some(f) = layout.footer {
            let y = f.top - f.height() / 8;
            let _ = MoveToEx(hdc, f.left, y, None);
            let _ = LineTo(hdc, f.right, y);
            draw_band(hdc, f, &footer_text(job.number, total), DT_CENTER.0);
        }
        SelectObject(hdc, old);
        let _ = DeleteObject(font.into());
    }
    guard.format_page(hdc, layout, job.start, job.end, true);
}

// ---------------------------------------------------------------- 印刷ダイアログ

/// ハンドルを持つ構造体の後始末（`GlobalFree` と `DeleteDC`）。
struct DialogHandles {
    dev_mode: HGLOBAL,
    dev_names: HGLOBAL,
    dc: HDC,
}

impl Drop for DialogHandles {
    fn drop(&mut self) {
        // SAFETY: ダイアログが返した（このプロセスが所有する）ハンドル。
        unsafe {
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
            if !self.dev_mode.is_invalid() {
                let _ = GlobalFree(Some(self.dev_mode));
            }
            if !self.dev_names.is_invalid() {
                let _ = GlobalFree(Some(self.dev_names));
            }
        }
    }
}

/// 既定のプリンターの `DEVMODE`（確保したハンドル）に、設定の用紙と向きを入れる。
/// プリンターがなければ `None`。
fn default_dev_mode(owner: HWND, settings: &PrintSettings) -> Option<DialogHandles> {
    let mut pd = PRINTDLGW {
        lStructSize: std::mem::size_of::<PRINTDLGW>() as u32,
        hwndOwner: owner,
        Flags: PD_RETURNDEFAULT,
        ..Default::default()
    };
    // SAFETY: 初期化した PRINTDLGW を渡す。
    if !unsafe { PrintDlgW(&mut pd) }.as_bool() {
        return None;
    }
    let handles = DialogHandles {
        dev_mode: pd.hDevMode,
        dev_names: pd.hDevNames,
        dc: HDC::default(),
    };
    apply_settings_to_dev_mode(handles.dev_mode, settings);
    Some(handles)
}

fn apply_settings_to_dev_mode(handle: HGLOBAL, settings: &PrintSettings) {
    if handle.is_invalid() {
        return;
    }
    // SAFETY: DEVMODE を持つグローバルメモリをロックして書き換える。
    unsafe {
        let p = GlobalLock(handle) as *mut DEVMODEW;
        if p.is_null() {
            return;
        }
        let dm = &mut *p;
        dm.dmFields |= DM_ORIENTATION;
        dm.Anonymous1.Anonymous1.dmOrientation = if settings.landscape { 2 } else { 1 };
        if settings.paper_size != 0 {
            dm.dmFields |= DM_PAPERSIZE;
            dm.Anonymous1.Anonymous1.dmPaperSize = settings.paper_size as i16;
        }
        let _ = GlobalUnlock(handle);
    }
}

/// `DEVMODE` から用紙の種類と向きを読む（なければ `None`）。
fn read_dev_mode(handle: HGLOBAL) -> Option<(u32, bool)> {
    if handle.is_invalid() {
        return None;
    }
    // SAFETY: DEVMODE を持つグローバルメモリをロックして読む。
    unsafe {
        let p = GlobalLock(handle) as *const DEVMODEW;
        if p.is_null() {
            return None;
        }
        let dm = &*p;
        let paper = if dm.dmFields.0 & DM_PAPERSIZE.0 != 0 {
            dm.Anonymous1.Anonymous1.dmPaperSize.max(0) as u32
        } else {
            0
        };
        let landscape =
            dm.dmFields.0 & DM_ORIENTATION.0 != 0 && dm.Anonymous1.Anonymous1.dmOrientation == 2;
        let _ = GlobalUnlock(handle);
        Some((paper, landscape))
    }
}

/// ページ設定のダイアログ（用紙、向き、余白）。変更があれば新しい設定を返す。
pub fn page_setup(owner: HWND, current: &PrintSettings) -> Result<Option<PrintSettings>> {
    let mut handles = default_dev_mode(owner, current).ok_or_else(no_printer)?;
    let m = current.margins;
    let mut psd = PAGESETUPDLGW {
        lStructSize: std::mem::size_of::<PAGESETUPDLGW>() as u32,
        hwndOwner: owner,
        hDevMode: handles.dev_mode,
        hDevNames: handles.dev_names,
        Flags: PSD_MARGINS | PSD_INHUNDREDTHSOFMILLIMETERS,
        rtMargin: RECT {
            left: m.left,
            top: m.top,
            right: m.right,
            bottom: m.bottom,
        },
        ..Default::default()
    };
    // SAFETY: 初期化した PAGESETUPDLGW を渡す。
    let ok = unsafe { PageSetupDlgW(&mut psd) }.as_bool();
    // ダイアログがハンドルを作り直すことがあるので、返ったものを持ち直す。
    handles.dev_mode = psd.hDevMode;
    handles.dev_names = psd.hDevNames;
    if !ok {
        // SAFETY: 直前のダイアログのエラー。0 なら利用者の取り消し。
        return match unsafe { CommDlgExtendedError() }.0 {
            0 => Ok(None),
            code => Err(Error::unavailable(
                "ページ設定",
                format!("ダイアログを開けませんでした（{code:#x}）"),
            )),
        };
    }
    let mut next = current.clone();
    next.margins = crate::printing::Margins {
        left: psd.rtMargin.left,
        top: psd.rtMargin.top,
        right: psd.rtMargin.right,
        bottom: psd.rtMargin.bottom,
    }
    .clamped();
    if let Some((paper, landscape)) = read_dev_mode(handles.dev_mode) {
        next.paper_size = paper;
        next.landscape = landscape;
    }
    Ok((next != *current).then_some(next))
}

fn no_printer() -> Error {
    Error::unavailable(
        "印刷",
        "使えるプリンターがありません。プリンターを追加するか、「Microsoft Print to PDF」を有効にしてください",
    )
}

/// 印刷ダイアログを出して印刷する。
pub fn print_with_dialog(
    owner: HWND,
    editor: &Editor,
    request: &PrintRequest<'_>,
) -> Result<PrintOutcome> {
    let defaults = default_dev_mode(owner, request.settings).ok_or_else(no_printer)?;
    let mut flags = PD_RETURNDC | PD_USEDEVMODECOPIESANDCOLLATE;
    match request.selection {
        Some(_) => flags |= PD_SELECTION,
        None => flags |= PD_NOSELECTION,
    }
    let mut pd = PRINTDLGW {
        lStructSize: std::mem::size_of::<PRINTDLGW>() as u32,
        hwndOwner: owner,
        hDevMode: defaults.dev_mode,
        hDevNames: defaults.dev_names,
        Flags: flags,
        nFromPage: 1,
        nToPage: 1,
        nMinPage: 1,
        nMaxPage: u16::MAX,
        nCopies: 1,
        ..Default::default()
    };
    // 以後のハンドルの持ち主は `handles`（ダイアログが作り直したものも含めて解放する）。
    std::mem::forget(defaults);
    let mut handles = DialogHandles {
        dev_mode: pd.hDevMode,
        dev_names: pd.hDevNames,
        dc: HDC::default(),
    };
    // SAFETY: 初期化した PRINTDLGW を渡す。
    let ok = unsafe { PrintDlgW(&mut pd) }.as_bool();
    handles.dev_mode = pd.hDevMode;
    handles.dev_names = pd.hDevNames;
    handles.dc = pd.hDC;
    if !ok {
        // SAFETY: 直前のダイアログのエラー。0 なら利用者の取り消し。
        return match unsafe { CommDlgExtendedError() }.0 {
            0 => Ok(PrintOutcome::Cancelled),
            code => Err(Error::unavailable(
                "印刷",
                format!("印刷ダイアログを開けませんでした（{code:#x}）"),
            )),
        };
    }
    if handles.dc.is_invalid() {
        return Err(no_printer());
    }
    let (from, to) = (u32::from(pd.nFromPage), u32::from(pd.nToPage));
    let range = if pd.Flags.0 & PD_SELECTION.0 != 0 {
        request.selection.unwrap_or((0, request.doc_len))
    } else {
        (0, request.doc_len)
    };
    let pages = if pd.Flags.0 & PD_PAGENUMS.0 != 0 {
        PageSelection::Pages { from, to }
    } else {
        PageSelection::All
    };
    let pages = print_to_dc(editor, handles.dc, request, range, pages)?;
    Ok(PrintOutcome::Printed { pages })
}

/// プリンターの DC に文書を送る（`StartDoc` から `EndDoc`）。送ったページ数を返す。
fn print_to_dc(
    editor: &Editor,
    hdc: HDC,
    request: &PrintRequest<'_>,
    range: (u64, u64),
    selection: PageSelection,
) -> Result<u32> {
    let layout = layout_for(hdc, request.settings)?;
    let guard = editor.begin_print(print_params(request.settings));
    let starts = paginate_range(&guard, hdc, &layout, range.0, range.1);
    let total = starts.len() as u32;
    let jobs = page_jobs(&starts, range.1, selection);
    if jobs.is_empty() {
        return Err(Error::invalid_argument(
            "ページ範囲",
            format!("印刷するページがありません（全 {total} ページ）"),
        ));
    }
    let now = local_timestamp();
    let name = to_wide(request.title);
    let doc = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: PCWSTR(name.as_ptr()),
        ..Default::default()
    };
    let fail = |what: &str| Error::unavailable("印刷", format!("{what}に失敗しました"));
    // SAFETY: 有効なプリンターの DC に、StartDoc → (StartPage → 描画 → EndPage)* → EndDoc。
    unsafe {
        if StartDocW(hdc, &doc) <= 0 {
            return Err(fail("印刷の開始"));
        }
        for job in &jobs {
            if StartPage(hdc) <= 0 {
                AbortDoc(hdc);
                return Err(fail("ページの開始"));
            }
            draw_page(&guard, hdc, &layout, *job, total, request.title, now);
            if EndPage(hdc) <= 0 {
                AbortDoc(hdc);
                return Err(fail("ページの送信"));
            }
        }
        if EndDoc(hdc) <= 0 {
            return Err(fail("印刷の終了"));
        }
    }
    Ok(jobs.len() as u32)
}
