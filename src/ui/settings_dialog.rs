//! 設定画面（V4-1）。モーダルのダイアログで、左にページの一覧、右にそのページの項目を出す。
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

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DialogBoxIndirectParamW, EndDialog, GetClientRect, GetDlgItem,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, MessageBoxW,
    PostMessageW, SendMessageW, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    GWLP_USERDATA, HMENU, IDCANCEL, IDOK, MB_ICONWARNING, MB_OK, SWP_NOZORDER, SW_HIDE, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_COMMAND, WM_INITDIALOG, WM_SETFONT,
};

use crate::appdata::settings::{pages, Item, ItemKind, ItemValue};
use crate::appdata::Settings;
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
        driver,
    };
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
    let body_h = (max_rows * row_h + 8).max(160);
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
fn refresh_controls(dlg: HWND, state: &State<'_>) {
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
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let notify = ((wparam.0 >> 16) & 0xFFFF) as u32;
            let Some(state) = state_of(dlg) else { return 0 };
            if id == IDOK.0 && notify == BN_CLICKED {
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
