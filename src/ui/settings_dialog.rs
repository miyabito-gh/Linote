//! 設定画面（V4-1、V4-2 でキーのページを追加）。モーダルのダイアログで、左にページの一覧、右にそのページの項目を出す。
//!
//! - **ページと項目は表から作る**: [`crate::appdata::settings::pages`] の表を読み、項目の種類
//!   （チェック、選択肢、数値、書体）ごとに部品を作る。ページや項目を足すときは、この
//!   ファイルを触らず表に書き足す（V5・V7）。
//! - 部品は標準の見た目（テーマの色には合わせない）。部品の値は、OK・適用の時点でまとめて
//!   読み、設定（[`Settings`]）に写す。数値が不正なら、その欄に戻して知らせ、反映しない。
//! - OK・適用は [`SettingsSession::apply`] を呼ぶ（保存と実行中のウィンドウへの反映は
//!   アプリ側）。設定ファイルが壊れているなど保存できないときは、画面の上部に理由を出す。
//! - 試験用に、操作役（`driver`）を渡せる。ダイアログが開いた直後に呼ばれ、部品の操作と
//!   ボタンの押下を行う（[`set_check`]、[`press`] など）。

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX, LIST_VIEW_ITEM_STATE_FLAGS,
    LVCFMT_LEFT, LVCF_FMT, LVCF_SUBITEM, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVIS_FOCUSED,
    LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_ENSUREVISIBLE, LVM_GETITEMCOUNT,
    LVM_GETITEMTEXTW, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVNI_SELECTED,
    LVS_EX_FULLROWSELECT, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NMHDR, WC_LISTVIEWW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DialogBoxIndirectParamW, EndDialog, GetClientRect, GetDlgItem,
    GetNextDlgTabItem, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    MessageBoxW, PostMessageW, SendMessageW, SetWindowLongPtrW, SetWindowPos, SetWindowTextW,
    ShowWindow, GWLP_USERDATA, HMENU, IDCANCEL, IDOK, MB_ICONWARNING, MB_OK, SWP_NOZORDER, SW_HIDE,
    SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_COMMAND, WM_INITDIALOG, WM_SETFONT,
};

use crate::appdata::settings::keybind::{self, KeyRow};
use crate::appdata::settings::{pages, Item, ItemKind, ItemValue, PageKind};
use crate::appdata::Settings;
use crate::command::{CommandDef, KeyChord};
use crate::platform::system::{dpi_for_window, scale, to_wide};
use crate::ui::dialogs::{blank_dialog_template, SettingsSession};
use crate::ui::window::UiFont;

/// 「適用」ボタンの ID。
pub const ID_APPLY: i32 = 3003;
const ID_BANNER: i32 = 3001;
const ID_PAGE_LIST: i32 = 3002;
/// 項目の部品の ID の最初（項目の通し番号 × 8 を足す）。
const ID_ITEM_BASE: i32 = 4000;
/// 項目の部品の ID の枝番。
const SUB_MAIN: i32 = 0;
const SUB_LABEL: i32 = 1;
const SUB_FONT_CHOOSE: i32 = 2;
const SUB_FONT_DEFAULT: i32 = 3;
// 「キー」ページの部品の ID。
pub const ID_KEY_FILTER: i32 = 3101;
const ID_KEY_LIST: i32 = 3102;
pub const ID_KEY_CAPTURE: i32 = 3103;
pub const ID_KEY_ASSIGN: i32 = 3104;
pub const ID_KEY_CLEAR: i32 = 3105;
pub const ID_KEY_RESET: i32 = 3106;
pub const ID_KEY_RESET_ALL: i32 = 3107;
const ID_KEY_HINT: i32 = 3108;
const ID_KEY_FILTER_LABEL: i32 = 3109;
const ID_KEY_CAPTURE_LABEL: i32 = 3110;
/// 「キー」ページがある場合の、本文の高さ。
const KEYS_BODY_H: i32 = 330;
/// 操作役を呼ぶための自前のメッセージ。
const WM_RUN_DRIVER: u32 = WM_APP + 7;

// 部品のスタイルとメッセージ（値は Win32 の定義どおり）。
const BS_PUSHBUTTON: u32 = 0x0;
const BS_DEFPUSHBUTTON: u32 = 0x1;
const BS_AUTOCHECKBOX: u32 = 0x3;
const CBS_DROPDOWNLIST: u32 = 0x3;
const LBS_NOTIFY: u32 = 0x1;
const ES_AUTOHSCROLL: u32 = 0x80;
const WS_TABSTOP: u32 = 0x1_0000;
const WS_GROUP: u32 = 0x2_0000;
const WS_BORDER: u32 = 0x80_0000;
const WS_VSCROLL: u32 = 0x20_0000;
const BM_GETCHECK: u32 = 0xF0;
const BM_SETCHECK: u32 = 0xF1;
const CB_ADDSTRING: u32 = 0x143;
const CB_GETCURSEL: u32 = 0x147;
const CB_SETCURSEL: u32 = 0x14E;
const LB_ADDSTRING: u32 = 0x180;
const LB_SETCURSEL: u32 = 0x186;
const LB_GETCURSEL: u32 = 0x188;
const DM_SETDEFID: u32 = 0x401;
const LBN_SELCHANGE: u32 = 1;
const BN_CLICKED: u32 = 0;
const EN_CHANGE: u32 = 0x300;
const ES_READONLY: u32 = 0x800;
const WM_NOTIFY: u32 = 0x4E;
const NM_DBLCLK: u32 = 0xFFFF_FFFD;
const WM_GETDLGCODE: u32 = 0x87;
const DLGC_WANTARROWS: isize = 0x1;
const DLGC_WANTALLKEYS: isize = 0x4;

/// 項目の通し番号（ページの順に数える）から部品の ID を作る。
fn control_id(index: usize, sub: i32) -> i32 {
    ID_ITEM_BASE + index as i32 * 8 + sub
}

/// 項目（全ページを通した）の一覧。
fn all_items() -> Vec<(usize, &'static Item)> {
    pages()
        .iter()
        .enumerate()
        .flat_map(|(page, p)| p.items.iter().map(move |item| (page, item)))
        .collect()
}

/// 画面の状態。ダイアログが開いている間だけ有効。
struct State<'a> {
    session: &'a mut (dyn SettingsSession + 'a),
    /// いま実行中の設定（編集の基準。適用するたびに更新する）。
    baseline: Settings,
    /// 画面に出している値のうち、部品から読めないもの（書体）を持つ作業用の設定。
    working: Settings,
    /// 項目ごとに、(ページの番号, 項目)。
    items: Vec<(usize, &'static Item)>,
    /// ページごとの部品（ページを切り替えるときに出し入れする）。
    page_controls: Vec<Vec<HWND>>,
    font: UiFont,
    /// 「キー」ページ: コマンドの定義、いま一覧に出している行（絞り込み後）。
    defs: Vec<CommandDef>,
    key_visible: Vec<KeyRow>,
    driver: Option<&'a mut (dyn FnMut(HWND) + 'a)>,
}

/// 設定画面を開く。閉じるまで戻らない。`driver` は試験用の操作役（普段は `None`）。
pub fn run<'a>(
    owner: HWND,
    session: &'a mut (dyn SettingsSession + 'a),
    driver: Option<&'a mut (dyn FnMut(HWND) + 'a)>,
) {
    let baseline = session.current();
    let mut state = State {
        session,
        working: baseline.clone(),
        baseline,
        items: all_items(),
        page_controls: Vec::new(),
        font: UiFont::for_dpi(dpi_for_window(owner)),
        defs: Vec::new(),
        key_visible: Vec::new(),
        driver,
    };
    state.defs = state.session.command_defs();
    // SAFETY: 共通コントロール（ListView）の登録。
    unsafe {
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES,
        });
    }
    let template = blank_dialog_template(10, 10);
    // SAFETY: 自分のモジュールのハンドル。
    let Ok(instance) = (unsafe { GetModuleHandleW(PCWSTR::null()) }) else {
        return;
    };
    // SAFETY: テンプレートと state は呼び出しの間有効。モーダル。
    unsafe {
        DialogBoxIndirectParamW(
            Some(instance.into()),
            template.as_ptr() as *const windows::Win32::UI::WindowsAndMessaging::DLGTEMPLATE,
            Some(owner),
            Some(dialog_proc),
            LPARAM(&mut state as *mut State as isize),
        );
        // 所有者が無効のまま残らないようにする（異常終了の保険）。
        let _ = EnableWindow(owner, true);
    }
}

fn state_of<'a>(dlg: HWND) -> Option<&'a mut State<'static>> {
    // SAFETY: WM_INITDIALOG で渡した State はダイアログが閉じるまで有効（寿命は偽装）。
    unsafe {
        let p = GetWindowLongPtrW(dlg, GWLP_USERDATA) as *mut State<'static>;
        p.as_mut()
    }
}

#[allow(clippy::too_many_arguments)]
fn child(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: u32,
    ex: u32,
    (x, y, w, h): (i32, i32, i32, i32),
    id: i32,
    font: &UiFont,
) -> HWND {
    let wide = to_wide(text);
    // SAFETY: 親は作成中のダイアログ。
    unsafe {
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(ex),
            class,
            PCWSTR(wide.as_ptr()),
            WINDOW_STYLE(
                style | windows::Win32::UI::WindowsAndMessaging::WS_CHILD.0 | 0x1000_0000, /* WS_VISIBLE */
            ),
            x,
            y,
            w,
            h,
            Some(parent),
            Some(HMENU(id as usize as *mut _)),
            GetModuleHandleW(PCWSTR::null()).ok().map(Into::into),
            None,
        )
        .unwrap_or_default();
        SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.handle().0 as usize)),
            Some(LPARAM(1)),
        );
        hwnd
    }
}

fn set_text(hwnd: HWND, text: &str) {
    let wide = to_wide(text);
    // SAFETY: NUL 終端の文字列。
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
    }
}

fn get_text(hwnd: HWND) -> String {
    // SAFETY: 長さを調べて、その分の出力先を渡す。
    unsafe {
        let len = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..n.min(buf.len())])
    }
}

fn send(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
    // SAFETY: 自分のダイアログの部品へのメッセージ。
    unsafe { SendMessageW(hwnd, msg, Some(WPARAM(wparam)), Some(LPARAM(lparam))).0 }
}

fn font_label(name: &str, size: u32) -> String {
    let face = if name.is_empty() {
        "（既定）"
    } else {
        name
    };
    format!("{face}、{size} pt")
}

/// ダイアログの部品を作る（`WM_INITDIALOG`）。
fn build(dlg: HWND, state: &mut State<'_>, owner: HWND) {
    let dpi = dpi_for_window(owner);
    let px = |n: i32| scale(n, dpi);
    set_text(dlg, "設定");
    let warning = state.session.warning();
    let banner_h = if warning.is_some() { 52 } else { 0 };
    let margin = 12;
    let list_w = 150;
    let page_w = 470;
    let row_h = 28;
    let max_rows = pages().iter().map(|p| p.items.len()).max().unwrap_or(0) as i32;
    let body_top = margin + if banner_h > 0 { banner_h + 8 } else { 0 };
    let has_keys = pages().iter().any(|p| p.kind == PageKind::Keys);
    let body_h = (max_rows * row_h + 8)
        .max(160)
        .max(if has_keys { KEYS_BODY_H } else { 0 });
    let client_w = margin + list_w + 14 + page_w + margin;
    let client_h = body_top + body_h + 14 + 28 + margin;

    // 大きさ（クライアント領域から外枠を逆算）と位置（所有者の中央）。
    // SAFETY: 自分のダイアログと所有者の矩形を読むだけ。
    unsafe {
        let mut outer = windows::Win32::Foundation::RECT::default();
        let mut client = windows::Win32::Foundation::RECT::default();
        let _ = GetWindowRect(dlg, &mut outer);
        let _ = GetClientRect(dlg, &mut client);
        let extra_w = (outer.right - outer.left) - (client.right - client.left);
        let extra_h = (outer.bottom - outer.top) - (client.bottom - client.top);
        let (w, h) = (px(client_w) + extra_w, px(client_h) + extra_h);
        let mut o = windows::Win32::Foundation::RECT::default();
        let _ = GetWindowRect(owner, &mut o);
        let x = o.left + ((o.right - o.left) - w) / 2;
        let y = o.top + ((o.bottom - o.top) - h) / 2;
        let _ = SetWindowPos(dlg, None, x.max(0), y.max(0), w, h, SWP_NOZORDER);
    }

    // 上部の警告。
    if let Some(text) = &warning {
        child(
            dlg,
            windows::core::w!("STATIC"),
            text,
            0,
            0,
            (
                px(margin),
                px(margin),
                px(client_w - margin * 2),
                px(banner_h),
            ),
            ID_BANNER,
            &state.font,
        );
    }

    // 左のページの一覧。
    let list = child(
        dlg,
        windows::core::w!("LISTBOX"),
        "",
        LBS_NOTIFY | WS_TABSTOP | WS_BORDER | WS_VSCROLL,
        0,
        (px(margin), px(body_top), px(list_w), px(body_h)),
        ID_PAGE_LIST,
        &state.font,
    );
    for p in pages() {
        let wide = to_wide(p.title);
        send(list, LB_ADDSTRING, 0, wide.as_ptr() as isize);
    }

    // ページごとの部品。
    state.page_controls = vec![Vec::new(); pages().len()];
    let page_x = margin + list_w + 14;
    let mut row_in_page = vec![0i32; pages().len()];
    for (index, (page, item)) in state.items.clone().into_iter().enumerate() {
        let y = body_top + 4 + row_in_page[page] * row_h;
        row_in_page[page] += 1;
        let label_w = 210;
        let ctl_x = page_x + label_w;
        let ctl_w = page_w - label_w;
        let mut made = Vec::new();
        let id = |sub| control_id(index, sub);
        let tab = WS_TABSTOP;
        match item.kind {
            ItemKind::Check => {
                made.push(child(
                    dlg,
                    windows::core::w!("BUTTON"),
                    item.label,
                    BS_AUTOCHECKBOX | tab,
                    0,
                    (px(page_x), px(y), px(page_w), px(row_h - 4)),
                    id(SUB_MAIN),
                    &state.font,
                ));
            }
            ItemKind::Choice(choices) => {
                made.push(child(
                    dlg,
                    windows::core::w!("STATIC"),
                    item.label,
                    0,
                    0,
                    (px(page_x), px(y + 3), px(label_w - 8), px(row_h - 6)),
                    id(SUB_LABEL),
                    &state.font,
                ));
                let combo = child(
                    dlg,
                    windows::core::w!("COMBOBOX"),
                    "",
                    CBS_DROPDOWNLIST | WS_VSCROLL | tab,
                    0,
                    (px(ctl_x), px(y), px(ctl_w.min(250)), px(200)),
                    id(SUB_MAIN),
                    &state.font,
                );
                for c in choices {
                    let wide = to_wide(c.label);
                    send(combo, CB_ADDSTRING, 0, wide.as_ptr() as isize);
                }
                made.push(combo);
            }
            ItemKind::Number { .. } => {
                made.push(child(
                    dlg,
                    windows::core::w!("STATIC"),
                    item.label,
                    0,
                    0,
                    (px(page_x), px(y + 3), px(label_w - 8), px(row_h - 6)),
                    id(SUB_LABEL),
                    &state.font,
                ));
                made.push(child(
                    dlg,
                    windows::core::w!("EDIT"),
                    "",
                    ES_AUTOHSCROLL | WS_BORDER | tab,
                    0,
                    (px(ctl_x), px(y), px(80), px(row_h - 6)),
                    id(SUB_MAIN),
                    &state.font,
                ));
            }
            ItemKind::Font => {
                made.push(child(
                    dlg,
                    windows::core::w!("STATIC"),
                    item.label,
                    0,
                    0,
                    (px(page_x), px(y + 3), px(label_w - 8), px(row_h - 6)),
                    id(SUB_LABEL),
                    &state.font,
                ));
                made.push(child(
                    dlg,
                    windows::core::w!("STATIC"),
                    "",
                    0,
                    0,
                    (px(ctl_x), px(y + 3), px(ctl_w - 150), px(row_h - 6)),
                    id(SUB_MAIN),
                    &state.font,
                ));
                made.push(child(
                    dlg,
                    windows::core::w!("BUTTON"),
                    "選択...",
                    BS_PUSHBUTTON | tab,
                    0,
                    (px(ctl_x + ctl_w - 145), px(y), px(70), px(row_h - 4)),
                    id(SUB_FONT_CHOOSE),
                    &state.font,
                ));
                made.push(child(
                    dlg,
                    windows::core::w!("BUTTON"),
                    "既定",
                    BS_PUSHBUTTON | tab,
                    0,
                    (px(ctl_x + ctl_w - 70), px(y), px(66), px(row_h - 4)),
                    id(SUB_FONT_DEFAULT),
                    &state.font,
                ));
            }
        }
        state.page_controls[page].extend(made);
    }

    // 専用のページ（キー）。
    for (page, p) in pages().iter().enumerate() {
        if p.kind == PageKind::Keys {
            let area = (page_x, body_top + 4, page_w, body_h);
            let made = build_keys_page(dlg, state, area, dpi);
            state.page_controls[page].extend(made);
        }
    }

    // 下のボタン。
    let by = client_h - margin - 28;
    let bw = 92;
    for (i, (label, id, style)) in [
        ("OK", IDOK.0, BS_DEFPUSHBUTTON),
        ("キャンセル", IDCANCEL.0, BS_PUSHBUTTON),
        ("適用", ID_APPLY, BS_PUSHBUTTON),
    ]
    .into_iter()
    .enumerate()
    {
        let x = client_w - margin - (3 - i as i32) * bw - (2 - i as i32) * 8;
        child(
            dlg,
            windows::core::w!("BUTTON"),
            label,
            style | WS_TABSTOP | if i == 0 { WS_GROUP } else { 0 },
            0,
            (px(x), px(by), px(bw), px(28)),
            id,
            &state.font,
        );
    }
    send(dlg, DM_SETDEFID, IDOK.0 as usize, 0);

    refresh_controls(dlg, state);
    send(list, LB_SETCURSEL, 0, 0);
    show_page(state, 0);
    // SAFETY: 自分のダイアログの部品。
    unsafe {
        let _ = SetFocus(Some(list));
    }
}

/// 作業用の設定の値を部品に出す。
fn refresh_controls(dlg: HWND, state: &mut State<'_>) {
    for (index, (_, item)) in state.items.iter().enumerate() {
        // SAFETY: 自分のダイアログの部品（なければエラーで飛ばす）。
        let Ok(ctl) = (unsafe { GetDlgItem(Some(dlg), control_id(index, SUB_MAIN)) }) else {
            continue;
        };
        match (item.kind, item.get(&state.working)) {
            (ItemKind::Check, ItemValue::Bool(b)) => {
                send(ctl, BM_SETCHECK, b as usize, 0);
            }
            (ItemKind::Choice(_), ItemValue::Text(t)) => {
                let i = item.choice_index(&t).unwrap_or(0);
                send(ctl, CB_SETCURSEL, i, 0);
            }
            (ItemKind::Number { .. }, ItemValue::Number(n)) => set_text(ctl, &n.to_string()),
            (ItemKind::Font, ItemValue::Font(name, size)) => {
                set_text(ctl, &font_label(&name, size))
            }
            _ => {}
        }
    }
    refresh_key_list(dlg, state);
}

/// 指定のページの部品だけを出す。
fn show_page(state: &State<'_>, page: usize) {
    for (i, controls) in state.page_controls.iter().enumerate() {
        for c in controls {
            // SAFETY: 自分のダイアログの部品。
            unsafe {
                let _ = ShowWindow(*c, if i == page { SW_SHOW } else { SW_HIDE });
            }
        }
    }
}

/// 部品の値を読んで設定にする。数値が不正なら、その部品と文を返す。
fn read_controls(dlg: HWND, state: &State<'_>) -> Result<Settings, (HWND, String)> {
    let mut settings = state.working.clone();
    for (index, (_, item)) in state.items.iter().enumerate() {
        // SAFETY: 自分のダイアログの部品。
        let Ok(ctl) = (unsafe { GetDlgItem(Some(dlg), control_id(index, SUB_MAIN)) }) else {
            continue;
        };
        match item.kind {
            ItemKind::Check => {
                let on = send(ctl, BM_GETCHECK, 0, 0) == 1;
                item.set(&mut settings, &ItemValue::Bool(on));
            }
            ItemKind::Choice(choices) => {
                let i = send(ctl, CB_GETCURSEL, 0, 0);
                if let Some(c) = usize::try_from(i).ok().and_then(|i| choices.get(i)) {
                    item.set(&mut settings, &ItemValue::Text(c.value.to_owned()));
                }
            }
            ItemKind::Number { .. } => match item.parse_number(&get_text(ctl)) {
                Ok(n) => item.set(&mut settings, &ItemValue::Number(n)),
                Err(e) => return Err((ctl, e.message(item.label))),
            },
            // 書体は、選択ボタンで作業用の設定に入っている。
            ItemKind::Font => {
                let value = item.get(&state.working);
                item.set(&mut settings, &value);
            }
        }
    }
    Ok(settings)
}

/// OK・適用。反映できたら `true`（数値が不正で止めたら `false`）。
fn apply(dlg: HWND, state: &mut State<'_>) -> bool {
    let edited = match read_controls(dlg, state) {
        Ok(s) => s,
        Err((ctl, message)) => {
            let text = to_wide(&message);
            let caption = to_wide("設定");
            // SAFETY: NUL 終端の文字列。
            unsafe {
                MessageBoxW(
                    Some(dlg),
                    PCWSTR(text.as_ptr()),
                    PCWSTR(caption.as_ptr()),
                    MB_OK | MB_ICONWARNING,
                );
                let _ = SetFocus(Some(ctl));
            }
            send(ctl, 0xB1 /* EM_SETSEL */, 0, -1);
            return false;
        }
    };
    if edited != state.baseline {
        let now = state.session.apply(&state.baseline, &edited);
        state.baseline = now.clone();
        state.working = now;
        refresh_controls(dlg, state);
        // 保存できない事情が変わったかもしれない（設定ファイルが直った）。
        if let Ok(banner) = unsafe { GetDlgItem(Some(dlg), ID_BANNER) } {
            set_text(banner, &state.session.warning().unwrap_or_default());
        }
    }
    true
}

fn choose_font(dlg: HWND, state: &mut State<'_>) {
    use crate::ui::dialogs::{Prompts, Win32Prompts};
    let v = &state.working.view;
    let current = if v.font.is_empty() {
        crate::ui::editor::default_font_name().to_owned()
    } else {
        v.font.clone()
    };
    if let Some((name, size)) = Win32Prompts.choose_font(dlg, &current, v.font_size) {
        state.working.view.font = name;
        state.working.view.font_size = size;
        refresh_controls(dlg, state);
    }
}

// ------------------------------------------------------------------ 「キー」ページ（V4-2）

/// 一覧の列（見出し、幅）。
const KEY_COLUMNS: [(&str, i32); 4] = [
    ("分類", 62),
    ("コマンド", 160),
    ("キー", 112),
    ("既定との違い", 128),
];

/// 「キー」ページの部品を作り、ページに属する部品の一覧を返す。`area` は (x, y, 幅, 高さ)。
fn build_keys_page(
    dlg: HWND,
    state: &State<'_>,
    (x0, y0, w, body_h): (i32, i32, i32, i32),
    dpi: u32,
) -> Vec<HWND> {
    let px = |n: i32| scale(n, dpi);
    let font = &state.font;
    let mut made = Vec::new();
    made.push(child(
        dlg,
        windows::core::w!("STATIC"),
        "絞り込み",
        0,
        0,
        (px(x0), px(y0 + 3), px(60), px(20)),
        ID_KEY_FILTER_LABEL,
        font,
    ));
    made.push(child(
        dlg,
        windows::core::w!("EDIT"),
        "",
        ES_AUTOHSCROLL | WS_BORDER | WS_TABSTOP,
        0,
        (px(x0 + 64), px(y0), px(w - 64), px(22)),
        ID_KEY_FILTER,
        font,
    ));
    let list_h = body_h - 132;
    let list = child(
        dlg,
        WC_LISTVIEWW,
        "",
        LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | WS_TABSTOP | WS_BORDER,
        0,
        (px(x0), px(y0 + 28), px(w), px(list_h)),
        ID_KEY_LIST,
        font,
    );
    send(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        LVS_EX_FULLROWSELECT as usize,
        LVS_EX_FULLROWSELECT as isize,
    );
    for (i, (title, width)) in KEY_COLUMNS.iter().enumerate() {
        let mut wide = to_wide(title);
        let col = LVCOLUMNW {
            mask: LVCF_FMT | LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM,
            fmt: LVCFMT_LEFT,
            cx: px(*width),
            pszText: PWSTR(wide.as_mut_ptr()),
            iSubItem: i as i32,
            ..Default::default()
        };
        send(
            list,
            LVM_INSERTCOLUMNW,
            i,
            &col as *const LVCOLUMNW as isize,
        );
    }
    made.push(list);

    let y1 = y0 + 28 + list_h + 8;
    made.push(child(
        dlg,
        windows::core::w!("STATIC"),
        "新しいキー",
        0,
        0,
        (px(x0), px(y1 + 3), px(72), px(20)),
        ID_KEY_CAPTURE_LABEL,
        font,
    ));
    let capture = child(
        dlg,
        windows::core::w!("EDIT"),
        "",
        ES_READONLY | WS_BORDER | WS_TABSTOP,
        0,
        (px(x0 + 76), px(y1), px(166), px(22)),
        ID_KEY_CAPTURE,
        font,
    );
    // SAFETY: 自分のダイアログの部品。解除は WM_NCDESTROY で行う。
    unsafe {
        let _ = SetWindowSubclass(capture, Some(capture_proc), 1, 0);
    }
    made.push(capture);
    let buttons: [(&str, i32, i32, i32, i32); 4] = [
        ("割り当て", ID_KEY_ASSIGN, x0 + 248, y1 - 1, 84),
        ("割り当てを外す", ID_KEY_CLEAR, x0 + 338, y1 - 1, 120),
        ("既定に戻す", ID_KEY_RESET, x0, y1 + 30, 110),
        ("すべて既定に戻す", ID_KEY_RESET_ALL, x0 + 118, y1 + 30, 140),
    ];
    for (label, id, bx, by, bw) in buttons {
        made.push(child(
            dlg,
            windows::core::w!("BUTTON"),
            label,
            BS_PUSHBUTTON | WS_TABSTOP,
            0,
            (px(bx), px(by), px(bw), px(24)),
            id,
            font,
        ));
    }
    made.push(child(
        dlg,
        windows::core::w!("STATIC"),
        "一覧でコマンドを選び、「新しいキー」の欄で割り当てたいキー（Ctrl／Alt／Shift との組み合わせ、F キーなど）を押して、「割り当て」を押します。他のコマンドが使っているキーは、付け替えるかどうかを確認します。",
        0,
        0,
        (px(x0), px(y1 + 60), px(w), px(32)),
        ID_KEY_HINT,
        font,
    ));
    made
}

/// キー入力欄のウィンドウ処理（サブクラス）。押したキーの組み合わせを、表記の文字にして入れる。
unsafe extern "system" fn capture_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    use crate::ui::menu::{chord_from_input, current_modifiers};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetParent, WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_NCDESTROY, WM_SYSCHAR, WM_SYSKEYDOWN,
        WM_SYSKEYUP,
    };
    match msg {
        WM_GETDLGCODE => LRESULT(DLGC_WANTALLKEYS | DLGC_WANTARROWS),
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wparam.0 as u32;
            let mods = current_modifiers();
            if vk == 0x09 && !mods.ctrl && !mods.alt {
                // Tab・Shift+Tab は、次・前の部品へ移る（入力欄に閉じ込めない）。
                // SAFETY: 自分のダイアログの部品。
                unsafe {
                    if let Ok(dlg) = GetParent(hwnd) {
                        let next = GetNextDlgTabItem(dlg, Some(hwnd), mods.shift);
                        if let Ok(next) = next {
                            let _ = SetFocus(Some(next));
                        }
                    }
                }
                return LRESULT(0);
            }
            if let Some(chord) = chord_from_input(vk, mods) {
                set_text(hwnd, &chord.to_string());
            }
            LRESULT(0)
        }
        WM_KEYUP | WM_SYSKEYUP | WM_CHAR | WM_SYSCHAR => LRESULT(0),
        WM_NCDESTROY => {
            // SAFETY: 自分のサブクラスの解除。
            unsafe {
                let _ = RemoveWindowSubclass(hwnd, Some(capture_proc), 1);
                DefSubclassProc(hwnd, msg, wparam, lparam)
            }
        }
        // SAFETY: 既定の処理に渡す。
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

fn dlg_item(dlg: HWND, id: i32) -> Option<HWND> {
    // SAFETY: 自分のダイアログの部品（なければ `None`）。
    unsafe { GetDlgItem(Some(dlg), id) }.ok()
}

/// 一覧で選んでいる行の位置（なければ `None`）。
fn selected_key_index(list: HWND) -> Option<usize> {
    usize::try_from(send(
        list,
        LVM_GETNEXTITEM,
        usize::MAX,
        LVNI_SELECTED as isize,
    ))
    .ok()
}

fn lv_set_text(list: HWND, index: usize, sub: usize, text: &str) {
    let mut wide = to_wide(text);
    let item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: index as i32,
        iSubItem: sub as i32,
        pszText: PWSTR(wide.as_mut_ptr()),
        ..Default::default()
    };
    let message = if sub == 0 {
        LVM_INSERTITEMW
    } else {
        LVM_SETITEMTEXTW
    };
    send(
        list,
        message,
        if sub == 0 { 0 } else { index },
        &item as *const LVITEMW as isize,
    );
}

/// 一覧の 1 行の文字。
fn key_row_texts(row: &KeyRow) -> [String; 4] {
    let diff = if row.custom {
        format!(
            "変更済（既定: {}）",
            row.default
                .map(|c| c.to_string())
                .unwrap_or_else(|| "なし".to_owned())
        )
    } else {
        String::new()
    };
    let mut title = row.title.to_owned();
    if row.global {
        title.push_str("（グローバル）");
    }
    [row.category.to_owned(), title, row.chord_text(), diff]
}

/// 作業用の設定と絞り込みから、一覧を作り直す（選んでいたコマンドは、残っていれば選び直す）。
fn refresh_key_list(dlg: HWND, state: &mut State<'_>) {
    let (Some(list), Some(filter)) = (dlg_item(dlg, ID_KEY_LIST), dlg_item(dlg, ID_KEY_FILTER))
    else {
        return;
    };
    let selected = selected_key_index(list).and_then(|i| state.key_visible.get(i).map(|r| r.id));
    let filter = get_text(filter);
    state.key_visible = keybind::key_rows(&state.defs, &state.working)
        .into_iter()
        .filter(|r| keybind::row_matches(r, &filter))
        .collect();
    send(list, LVM_DELETEALLITEMS, 0, 0);
    for (i, row) in state.key_visible.iter().enumerate() {
        for (sub, text) in key_row_texts(row).iter().enumerate() {
            lv_set_text(list, i, sub, text);
        }
    }
    if let Some(i) = selected.and_then(|id| state.key_visible.iter().position(|r| r.id == id)) {
        select_key_index(list, i);
    }
}

fn select_key_index(list: HWND, index: usize) {
    let item = LVITEMW {
        state: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
        stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
        ..Default::default()
    };
    send(
        list,
        LVM_SETITEMSTATE,
        index,
        &item as *const LVITEMW as isize,
    );
    send(list, LVM_ENSUREVISIBLE, index, 0);
}

/// 一覧で選んでいるコマンド（なければ知らせて `None`）。
fn selected_key_row(dlg: HWND, state: &mut State<'_>) -> Option<KeyRow> {
    let row = dlg_item(dlg, ID_KEY_LIST)
        .and_then(selected_key_index)
        .and_then(|i| state.key_visible.get(i).cloned());
    if row.is_none() {
        state
            .session
            .notice(dlg, "先に、一覧でコマンドを選んでください。");
    }
    row
}

fn clear_capture(dlg: HWND) {
    if let Some(c) = dlg_item(dlg, ID_KEY_CAPTURE) {
        set_text(c, "");
    }
}

/// 「割り当て」: 入力欄のキーを選んだコマンドに割り当てる。重複は確認して付け替える。
fn key_assign(dlg: HWND, state: &mut State<'_>) {
    let Some(row) = selected_key_row(dlg, state) else {
        return;
    };
    let text = dlg_item(dlg, ID_KEY_CAPTURE)
        .map(get_text)
        .unwrap_or_default();
    let Ok(chord) = KeyChord::parse(&text) else {
        state.session.notice(
            dlg,
            "「新しいキー」の欄を選んで、割り当てたいキーを押してから「割り当て」を押してください。",
        );
        return;
    };
    let mut next = state.working.clone();
    let mut result = keybind::assign_chord(&mut next, &state.defs, row.id, chord, false);
    if let Err(keybind::AssignError::Conflict(_, other_title)) = &result {
        let question = format!(
            "{chord} は「{other_title}」が使っています。\n「{}」に付け替えますか？\n（「{other_title}」は割り当てなしになります）",
            row.title
        );
        if !state.session.confirm(dlg, "キーの付け替え", &question) {
            return;
        }
        next = state.working.clone();
        result = keybind::assign_chord(&mut next, &state.defs, row.id, chord, true);
    }
    match result {
        Ok(()) => {
            state.working = next;
            clear_capture(dlg);
            refresh_key_list(dlg, state);
        }
        Err(keybind::AssignError::Rejected(why)) => state.session.notice(dlg, &why),
        Err(_) => state
            .session
            .notice(dlg, "このコマンドには割り当てられません。"),
    }
}

/// 「割り当てを外す」「既定に戻す」「すべて既定に戻す」。
fn key_button(dlg: HWND, state: &mut State<'_>, id: i32) {
    match id {
        ID_KEY_ASSIGN => key_assign(dlg, state),
        ID_KEY_CLEAR | ID_KEY_RESET => {
            let Some(row) = selected_key_row(dlg, state) else {
                return;
            };
            if id == ID_KEY_CLEAR {
                keybind::clear_chord(&mut state.working, &state.defs, row.id);
            } else {
                keybind::reset_chord(&mut state.working, row.id);
            }
            // 既定に戻した先が、別のコマンドが使っているキーなら知らせる（重複は先の定義が優先）。
            if id == ID_KEY_RESET {
                let now = keybind::key_rows(&state.defs, &state.working)
                    .into_iter()
                    .find(|r| r.id == row.id)
                    .and_then(|r| r.chord);
                if let Some(chord) = now {
                    if let Some(other) =
                        keybind::find_conflict(&state.defs, &state.working, row.id, &chord)
                    {
                        let text = format!(
                            "既定の {chord} は「{}」が使っています。どちらかの割り当てを変えてください。",
                            other.title
                        );
                        state.session.notice(dlg, &text);
                    }
                }
            }
            refresh_key_list(dlg, state);
        }
        ID_KEY_RESET_ALL
            if state.session.confirm(
                dlg,
                "すべて既定に戻す",
                "すべてのコマンドのキー割り当てを既定に戻します。よろしいですか？",
            ) =>
        {
            keybind::reset_all(&mut state.working, &state.defs);
            refresh_key_list(dlg, state);
        }
        _ => {}
    }
}

unsafe extern "system" fn dialog_proc(
    dlg: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match msg {
        WM_INITDIALOG => {
            // SAFETY: init_param に渡した State は、ダイアログが閉じるまで有効。
            let state = unsafe { &mut *(lparam.0 as *mut State<'static>) };
            // SAFETY: 自分のダイアログに State を結び付ける。
            unsafe { SetWindowLongPtrW(dlg, GWLP_USERDATA, lparam.0) };
            // SAFETY: 所有者の取得。
            let owner = unsafe {
                windows::Win32::UI::WindowsAndMessaging::GetWindow(
                    dlg,
                    windows::Win32::UI::WindowsAndMessaging::GW_OWNER,
                )
            }
            .unwrap_or(dlg);
            build(dlg, state, owner);
            if state.driver.is_some() {
                // SAFETY: 自分のダイアログへの投函。
                unsafe {
                    let _ = PostMessageW(Some(dlg), WM_RUN_DRIVER, WPARAM(0), LPARAM(0));
                }
            }
            // フォーカスは自分で置いた。
            0
        }
        WM_RUN_DRIVER => {
            if let Some(state) = state_of(dlg) {
                if let Some(driver) = state.driver.as_mut() {
                    driver(dlg);
                }
            }
            1
        }
        WM_NOTIFY => {
            // 一覧の行のダブルクリックで、キー入力欄へ移る。
            // SAFETY: WM_NOTIFY の lparam は NMHDR を指す。
            let hdr = unsafe { (lparam.0 as *const NMHDR).as_ref() };
            if let Some(h) = hdr {
                if h.idFrom == ID_KEY_LIST as usize && h.code == NM_DBLCLK {
                    if let Some(c) = dlg_item(dlg, ID_KEY_CAPTURE) {
                        // SAFETY: 自分のダイアログの部品。
                        unsafe {
                            let _ = SetFocus(Some(c));
                        }
                    }
                }
            }
            0
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let notify = ((wparam.0 >> 16) & 0xFFFF) as u32;
            let Some(state) = state_of(dlg) else { return 0 };
            if id == ID_KEY_FILTER && notify == EN_CHANGE {
                refresh_key_list(dlg, state);
                1
            } else if matches!(
                id,
                ID_KEY_ASSIGN | ID_KEY_CLEAR | ID_KEY_RESET | ID_KEY_RESET_ALL
            ) && notify == BN_CLICKED
            {
                key_button(dlg, state, id);
                1
            } else if id == IDOK.0 && notify == BN_CLICKED {
                if apply(dlg, state) {
                    // SAFETY: 自分のダイアログを閉じる。
                    unsafe {
                        let _ = EndDialog(dlg, IDOK.0 as isize);
                    }
                }
                1
            } else if id == IDCANCEL.0 {
                // SAFETY: 自分のダイアログを閉じる。
                unsafe {
                    let _ = EndDialog(dlg, IDCANCEL.0 as isize);
                }
                1
            } else if id == ID_APPLY && notify == BN_CLICKED {
                apply(dlg, state);
                1
            } else if id == ID_PAGE_LIST && notify == LBN_SELCHANGE {
                // SAFETY: 自分のダイアログの部品。
                if let Ok(list) = unsafe { GetDlgItem(Some(dlg), ID_PAGE_LIST) } {
                    let sel = send(list, LB_GETCURSEL, 0, 0);
                    if let Ok(page) = usize::try_from(sel) {
                        show_page(state, page);
                    }
                }
                1
            } else if notify == BN_CLICKED {
                // 書体の選択・既定。
                for (index, (_, item)) in state.items.clone().iter().enumerate() {
                    if item.kind != ItemKind::Font {
                        continue;
                    }
                    if id == control_id(index, SUB_FONT_CHOOSE) {
                        choose_font(dlg, state);
                    } else if id == control_id(index, SUB_FONT_DEFAULT) {
                        let d = crate::appdata::settings::ViewSettings::default();
                        state.working.view.font = d.font;
                        state.working.view.font_size = d.font_size;
                        refresh_controls(dlg, state);
                    }
                }
                1
            } else {
                0
            }
        }
        _ => 0,
    }
}

// ------------------------------------------------------------------ 試験用の操作

/// 項目の識別子（`view.wrap` など）から部品を探す。
pub fn item_control(dlg: HWND, item_id: &str) -> Option<HWND> {
    let index = all_items()
        .iter()
        .position(|(_, item)| item.id == item_id)?;
    // SAFETY: 自分のダイアログの部品。
    unsafe { GetDlgItem(Some(dlg), control_id(index, SUB_MAIN)) }.ok()
}

/// 項目の部品の ID（書体の選択ボタンなど、枝番つき）。
pub fn item_control_id(item_id: &str, font_button: Option<bool>) -> Option<i32> {
    let index = all_items()
        .iter()
        .position(|(_, item)| item.id == item_id)?;
    Some(control_id(
        index,
        match font_button {
            None => SUB_MAIN,
            Some(true) => SUB_FONT_CHOOSE,
            Some(false) => SUB_FONT_DEFAULT,
        },
    ))
}

/// チェックボックスの項目を設定する。
pub fn set_check(dlg: HWND, item_id: &str, on: bool) -> bool {
    item_control(dlg, item_id).is_some_and(|c| {
        send(c, BM_SETCHECK, on as usize, 0);
        true
    })
}

/// チェックボックスの項目の状態。
pub fn get_check(dlg: HWND, item_id: &str) -> Option<bool> {
    item_control(dlg, item_id).map(|c| send(c, BM_GETCHECK, 0, 0) == 1)
}

/// 選択肢の項目を `value`（設定ファイルでの値）で選ぶ。
pub fn select_choice(dlg: HWND, item_id: &str, value: &str) -> bool {
    let Some((_, item)) = all_items().into_iter().find(|(_, i)| i.id == item_id) else {
        return false;
    };
    let (Some(ctl), Some(i)) = (item_control(dlg, item_id), item.choice_index(value)) else {
        return false;
    };
    send(ctl, CB_SETCURSEL, i, 0);
    true
}

/// 選択肢の項目で選んでいる値（設定ファイルでの値）。
pub fn get_choice(dlg: HWND, item_id: &str) -> Option<String> {
    let (_, item) = all_items().into_iter().find(|(_, i)| i.id == item_id)?;
    let ctl = item_control(dlg, item_id)?;
    let ItemKind::Choice(choices) = item.kind else {
        return None;
    };
    let i = usize::try_from(send(ctl, CB_GETCURSEL, 0, 0)).ok()?;
    choices.get(i).map(|c| c.value.to_owned())
}

/// 数値の項目の入力欄に文字を入れる。
pub fn set_number_text(dlg: HWND, item_id: &str, text: &str) -> bool {
    item_control(dlg, item_id).is_some_and(|c| {
        set_text(c, text);
        true
    })
}

/// 項目の部品の文字（数値の欄、書体の表示）。
pub fn control_text(dlg: HWND, item_id: &str) -> Option<String> {
    item_control(dlg, item_id).map(get_text)
}

/// 上部の警告の文（なければ `None`）。
pub fn banner_text(dlg: HWND) -> Option<String> {
    // SAFETY: 自分のダイアログの部品。
    unsafe { GetDlgItem(Some(dlg), ID_BANNER) }
        .ok()
        .map(get_text)
}

/// 項目の部品がいま見えているか（ページが選ばれているか。部品自身の表示の指定で判断する）。
pub fn item_visible(dlg: HWND, item_id: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, GWL_STYLE};
    // SAFETY: 自分のダイアログの部品のスタイルを読むだけ。
    item_control(dlg, item_id)
        .is_some_and(|c| unsafe { GetWindowLongPtrW(c, GWL_STYLE) } as u32 & 0x1000_0000 != 0)
}

/// 左の一覧でページ（`general`、`view`）を選ぶ（選んだときの通知も送る）。
pub fn select_page(dlg: HWND, page_id: &str) -> bool {
    let Some(i) = pages().iter().position(|p| p.id == page_id) else {
        return false;
    };
    // SAFETY: 自分のダイアログの部品。
    let Ok(list) = (unsafe { GetDlgItem(Some(dlg), ID_PAGE_LIST) }) else {
        return false;
    };
    send(list, LB_SETCURSEL, i, 0);
    send(
        dlg,
        WM_COMMAND,
        ID_PAGE_LIST as usize | ((LBN_SELCHANGE as usize) << 16),
        list.0 as isize,
    );
    true
}

/// ボタン（`IDOK`、`IDCANCEL`、[`ID_APPLY`]、書体のボタン）を押す。
pub fn press(dlg: HWND, id: i32) {
    send(
        dlg,
        WM_COMMAND,
        id as usize | ((BN_CLICKED as usize) << 16),
        0,
    );
}

/// 「キー」ページの一覧の文字（行ごとに 4 列: 分類、コマンド、キー、既定との違い）。
pub fn key_list_texts(dlg: HWND) -> Vec<[String; 4]> {
    let Some(list) = dlg_item(dlg, ID_KEY_LIST) else {
        return Vec::new();
    };
    let count = send(list, LVM_GETITEMCOUNT, 0, 0).max(0) as usize;
    (0..count)
        .map(|i| {
            let mut cells: [String; 4] = Default::default();
            for (sub, cell) in cells.iter_mut().enumerate() {
                let mut buf = [0u16; 256];
                let item = LVITEMW {
                    iSubItem: sub as i32,
                    pszText: PWSTR(buf.as_mut_ptr()),
                    cchTextMax: buf.len() as i32,
                    ..Default::default()
                };
                let n = send(list, LVM_GETITEMTEXTW, i, &item as *const LVITEMW as isize).max(0)
                    as usize;
                *cell = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
            }
            cells
        })
        .collect()
}

/// 一覧で、コマンド（`file.save` など）の行を選ぶ。一覧にない（絞り込みで隠れている）なら `false`。
pub fn key_select(dlg: HWND, command: &str) -> bool {
    let (Some(list), Some(state)) = (dlg_item(dlg, ID_KEY_LIST), state_of(dlg)) else {
        return false;
    };
    let Some(i) = state.key_visible.iter().position(|r| r.id == command) else {
        return false;
    };
    select_key_index(list, i);
    true
}

/// 絞り込みの欄に文字を入れる（一覧が作り直される）。
pub fn key_filter(dlg: HWND, text: &str) {
    if let Some(c) = dlg_item(dlg, ID_KEY_FILTER) {
        set_text(c, text);
    }
}

/// キー入力欄に、押したことにするキーの表記を入れる。
pub fn key_capture(dlg: HWND, chord: &str) {
    if let Some(c) = dlg_item(dlg, ID_KEY_CAPTURE) {
        set_text(c, chord);
    }
}

/// キー入力欄の文字。
pub fn key_capture_text(dlg: HWND) -> String {
    dlg_item(dlg, ID_KEY_CAPTURE)
        .map(get_text)
        .unwrap_or_default()
}

/// キー入力欄に仮想キーの押下を送る（修飾キーは実際の状態に従う）。
pub fn key_capture_press(dlg: HWND, vk: u32) {
    use windows::Win32::UI::WindowsAndMessaging::WM_KEYDOWN;
    if let Some(c) = dlg_item(dlg, ID_KEY_CAPTURE) {
        send(c, WM_KEYDOWN, vk as usize, 0);
    }
}
