//! Supported Win32 owner drawing for dark menus; native light menus remain available.
//! This state is independent of App so synchronous menu paint callbacks can reenter
//! the window procedure while an application command holds the App borrow.
use crate::platform::system::{scale, to_wide};
use crate::ui::theme::Theme;
use crate::ui::window::{Canvas, UiFont};
use std::cell::RefCell;
use windows::core::PWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, DrawTextW, GetDC, ReleaseDC, RestoreDC, SaveDC, SelectObject,
    SetBkMode, SetTextColor, DT_HIDEPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, HBRUSH,
    TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_CHECKED, ODS_DISABLED, ODS_GRAYED, ODS_HOTLIGHT,
    ODS_NOACCEL, ODS_SELECTED, ODT_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone)]
struct Item {
    text: String,
    bar: bool,
    separator: bool,
    submenu: bool,
}
struct Painter {
    items: Vec<Item>,
    theme: Theme,
    dpi: u32,
    font: UiFont,
    brush: HBRUSH,
}
impl Drop for Painter {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.brush.into());
        }
    }
}
thread_local! { static PAINTER: RefCell<Option<Painter>> = const { RefCell::new(None) }; }

pub(super) fn apply(hwnd: HWND, bar: HMENU, theme: &Theme, dpi: u32) {
    PAINTER.with(|cell| {
        let mut state = cell.borrow_mut();
        let old = state.take();
        let previous = old.as_ref().map_or(&[][..], |p| p.items.as_slice());
        let brush = unsafe {
            CreateSolidBrush(windows::Win32::Foundation::COLORREF(
                theme.ui.bar_bg.colorref(),
            ))
        };
        let mut painter = Painter {
            items: Vec::new(),
            theme: theme.clone(),
            dpi,
            font: UiFont::for_dpi(dpi),
            brush,
        };
        // Keep old item labels/brush alive until every menu has been updated.
        unsafe {
            configure(bar, true, previous, &mut painter);
        }
        *state = Some(painter);
    });
    // Paint callbacks must run after releasing PAINTER's mutable borrow.
    unsafe {
        let _ = DrawMenuBar(hwnd);
    }
}

unsafe fn configure(menu: HMENU, bar: bool, previous: &[Item], painter: &mut Painter) {
    // ライトでは背景を指定しない（null で既定に戻す）。COLOR_MENU の筆で塗ると、
    // Windows 11 の標準メニューの描画（左の印の列・選択の色）と食い違い、灰色の帯が出る。
    let bg = if painter.theme.dark {
        painter.brush
    } else {
        HBRUSH::default()
    };
    let info = MENUINFO {
        cbSize: std::mem::size_of::<MENUINFO>() as u32,
        fMask: MIM_BACKGROUND,
        hbrBack: bg,
        ..Default::default()
    };
    unsafe {
        let _ = SetMenuInfo(menu, &info);
    }
    for position in 0..unsafe { GetMenuItemCount(Some(menu)) }.max(0) as u32 {
        let mut info = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_FTYPE | MIIM_DATA | MIIM_SUBMENU,
            ..Default::default()
        };
        if unsafe { GetMenuItemInfoW(menu, position, true, &mut info) }.is_err() {
            continue;
        }
        let text = if info.fType.contains(MFT_OWNERDRAW) {
            previous
                .get(info.dwItemData.wrapping_sub(1))
                .map(|i| i.text.clone())
                .unwrap_or_default()
        } else {
            let mut text = vec![0u16; 2048];
            let n = unsafe { GetMenuStringW(menu, position, Some(&mut text), MF_BYPOSITION) }.max(0)
                as usize;
            String::from_utf16_lossy(&text[..n])
        };
        let item = Item {
            text,
            bar,
            separator: info.fType.contains(MFT_SEPARATOR),
            submenu: !info.hSubMenu.is_invalid(),
        };
        let mut wide = to_wide(&item.text);
        info.fMask = MIIM_FTYPE | MIIM_DATA | MIIM_STRING;
        info.dwTypeData = PWSTR(wide.as_mut_ptr());
        if painter.theme.dark {
            info.fType |= MFT_OWNERDRAW;
            painter.items.push(item);
            info.dwItemData = painter.items.len();
        } else {
            info.fType &= !MFT_OWNERDRAW;
            info.dwItemData = 0;
        }
        unsafe {
            let _ = SetMenuItemInfoW(menu, position, true, &info);
        }
        if !info.hSubMenu.is_invalid() {
            unsafe {
                configure(info.hSubMenu, false, previous, painter);
            }
        }
    }
}

pub(super) fn label(menu: HMENU, id: u32) -> Option<String> {
    let mut info = MENUITEMINFOW {
        cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
        fMask: MIIM_DATA | MIIM_FTYPE,
        ..Default::default()
    };
    if unsafe { GetMenuItemInfoW(menu, id, false, &mut info) }.is_err()
        || !info.fType.contains(MFT_OWNERDRAW)
    {
        return None;
    }
    PAINTER.with(|cell| {
        cell.borrow()
            .as_ref()?
            .items
            .get(info.dwItemData.wrapping_sub(1))
            .map(|i| i.text.clone())
    })
}

/// Called before borrowing App; pointers are only dereferenced for the matching Win32 message.
pub(crate) fn handle(msg: u32, lparam: LPARAM) -> Option<LRESULT> {
    if lparam.0 == 0 || !matches!(msg, WM_MEASUREITEM | WM_DRAWITEM) {
        return None;
    }
    PAINTER.with(|cell| {
        let state = cell.try_borrow().ok()?;
        let p = state.as_ref()?;
        if !p.theme.dark {
            return None;
        }
        if msg == WM_MEASUREITEM {
            let m = unsafe { &mut *(lparam.0 as *mut MEASUREITEMSTRUCT) };
            if m.CtlType != ODT_MENU {
                return None;
            }
            let item = p.items.get(m.itemData.wrapping_sub(1))?;
            let dc = unsafe { GetDC(None) };
            let saved = unsafe { SaveDC(dc) };
            unsafe {
                SelectObject(dc, p.font.handle().into());
            }
            let c = Canvas {
                hdc: dc,
                dpi: p.dpi,
            };
            let (title, key) = item.text.split_once('\t').unwrap_or((&item.text, ""));
            m.itemWidth = if item.bar {
                c.measure(title) + scale(12, p.dpi)
            } else {
                c.measure(title)
                    + c.measure(key)
                    + scale(if key.is_empty() { 48 } else { 76 }, p.dpi)
            }
            .max(1) as u32;
            m.itemHeight = if item.separator {
                scale(8, p.dpi)
            } else {
                p.font.tab_height(p.dpi)
            }
            .max(1) as u32;
            unsafe {
                let _ = RestoreDC(dc, saved);
                ReleaseDC(None, dc);
            }
        } else {
            let d = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            if d.CtlType != ODT_MENU {
                return None;
            }
            let item = p.items.get(d.itemData.wrapping_sub(1))?;
            let saved = unsafe { SaveDC(d.hDC) };
            unsafe {
                SelectObject(d.hDC, p.font.handle().into());
                SetBkMode(d.hDC, TRANSPARENT);
            }
            let c = Canvas {
                hdc: d.hDC,
                dpi: p.dpi,
            };
            let u = &p.theme.ui;
            let selected = d.itemState.0 & (ODS_SELECTED.0 | ODS_HOTLIGHT.0) != 0;
            let disabled = d.itemState.0 & (ODS_DISABLED.0 | ODS_GRAYED.0) != 0;
            c.fill(&d.rcItem, if selected { u.tab_hover_bg } else { u.bar_bg });
            let color = if disabled {
                u.status_dim
            } else {
                u.tab_active_fg
            };
            let mut r = d.rcItem;
            if item.separator {
                let y = (r.top + r.bottom) / 2;
                c.fill(
                    &RECT {
                        left: r.left + c.px(24),
                        right: r.right - c.px(8),
                        top: y,
                        bottom: y + 1,
                    },
                    u.tab_separator,
                );
            } else {
                let gutter = if item.bar { 6 } else { 26 };
                r.left += c.px(gutter);
                r.right -= c.px(if item.bar { 6 } else { 24 });
                let (title, key) = item.text.split_once('\t').unwrap_or((&item.text, ""));
                let flags = DT_SINGLELINE
                    | DT_VCENTER
                    | if d.itemState.0 & ODS_NOACCEL.0 != 0 {
                        DT_HIDEPREFIX
                    } else {
                        Default::default()
                    };
                unsafe {
                    SetTextColor(
                        d.hDC,
                        windows::Win32::Foundation::COLORREF(color.colorref()),
                    );
                }
                let mut title = to_wide(title);
                title.pop();
                unsafe {
                    DrawTextW(d.hDC, &mut title, &mut r, flags);
                }
                if !key.is_empty() {
                    let mut key = to_wide(key);
                    key.pop();
                    unsafe {
                        DrawTextW(d.hDC, &mut key, &mut r, flags | DT_RIGHT);
                    }
                }
                let cy = (r.top + r.bottom) / 2;
                if d.itemState.0 & ODS_CHECKED.0 != 0 && !item.bar {
                    let x = d.rcItem.left + c.px(10);
                    c.line(x, cy, x + c.px(3), cy + c.px(3), c.px(1).max(1), color);
                    c.line(
                        x + c.px(3),
                        cy + c.px(3),
                        x + c.px(8),
                        cy - c.px(4),
                        c.px(1).max(1),
                        color,
                    );
                }
                if item.submenu && !item.bar {
                    let x = d.rcItem.right - c.px(10);
                    c.line(x - c.px(3), cy - c.px(3), x, cy, 1, color);
                    c.line(x, cy, x - c.px(3), cy + c.px(3), 1, color);
                }
            }
            unsafe {
                let _ = RestoreDC(d.hDC, saved);
            }
        }
        Some(LRESULT(1))
    })
}

/// Native menus leave the unused menubar area in the system light color.
/// Paint only that area after default non-client processing, preserving the
/// standard frame, menu hit testing and keyboard behavior.
pub(crate) fn nonclient(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    if !matches!(msg, WM_NCPAINT | WM_NCACTIVATE) {
        return None;
    }
    if !PAINTER.with(|c| {
        c.try_borrow()
            .ok()
            .is_some_and(|s| s.as_ref().is_some_and(|p| p.theme.dark))
    }) {
        return None;
    }
    let result = unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    PAINTER.with(|cell| {
        let state = cell.borrow();
        let Some(p) = state.as_ref() else {
            return;
        };
        let mut bar = MENUBARINFO {
            cbSize: std::mem::size_of::<MENUBARINFO>() as u32,
            ..Default::default()
        };
        let mut window = RECT::default();
        // OBJID_MENU is the documented accessibility object ID for the menubar.
        unsafe {
            if GetMenuBarInfo(hwnd, OBJID_MENU, 0, &mut bar).is_err()
                || GetWindowRect(hwnd, &mut window).is_err()
            {
                return;
            }
            let mut last = RECT::default();
            let count = GetMenuItemCount(Some(bar.hMenu));
            if count == 0
                || GetMenuItemRect(Some(hwnd), bar.hMenu, (count - 1) as u32, &mut last).is_err()
            {
                return;
            }
            let dc = windows::Win32::Graphics::Gdi::GetWindowDC(Some(hwnd));
            let c = Canvas {
                hdc: dc,
                dpi: p.dpi,
            };
            c.fill(
                &RECT {
                    left: last.right - window.left,
                    right: (window.right - window.left) - (bar.rcBar.left - window.left),
                    top: bar.rcBar.top - window.top,
                    bottom: bar.rcBar.bottom - window.top,
                },
                p.theme.ui.bar_bg,
            );
            ReleaseDC(Some(hwnd), dc);
        }
    });
    Some(result)
}

pub(crate) fn popup(menu: HMENU) -> Option<usize> {
    PAINTER.with(|cell| {
        let mut state = cell.borrow_mut();
        let p = state.as_mut()?;
        if !p.theme.dark {
            return None;
        }
        let len = p.items.len();
        unsafe {
            configure(menu, false, &[], p);
        }
        Some(len)
    })
}
pub(crate) fn popup_closed(previous_len: Option<usize>) {
    if let Some(len) = previous_len {
        PAINTER.with(|cell| {
            if let Some(p) = cell.borrow_mut().as_mut() {
                p.items.truncate(len);
            }
        });
    }
}

fn mnemonic(text: &str) -> Option<char> {
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '&' {
            match chars.next() {
                Some('&') => continue,
                next => return next.map(|c| c.to_ascii_lowercase()),
            }
        }
    }
    None
}

/// Owner-drawn menus still use the normal keyboard menu loop; supply the
/// documented WM_MENUCHAR response for their access keys.
pub(crate) fn character(wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let key = char::from_u32((wparam.0 & 0xffff) as u32)?.to_ascii_lowercase();
    let menu = HMENU(lparam.0 as *mut _);
    PAINTER.with(|cell| {
        let state = cell.try_borrow().ok()?;
        let p = state.as_ref()?;
        if !p.theme.dark {
            return None;
        }
        let mut matches = Vec::new();
        let mut selected = None;
        for pos in 0..unsafe { GetMenuItemCount(Some(menu)) }.max(0) as u32 {
            let mut info = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_DATA | MIIM_STATE | MIIM_FTYPE,
                ..Default::default()
            };
            if unsafe { GetMenuItemInfoW(menu, pos, true, &mut info) }.is_err() {
                continue;
            }
            if info.fState.contains(MFS_HILITE) {
                selected = Some(pos);
            }
            if info.fType.contains(MFT_OWNERDRAW) && !info.fState.contains(MFS_DISABLED) {
                if let Some(item) = p.items.get(info.dwItemData.wrapping_sub(1)) {
                    if mnemonic(&item.text) == Some(key) {
                        matches.push(pos);
                    }
                }
            }
        }
        let position = matches
            .iter()
            .copied()
            .find(|&pos| selected.is_none_or(|s| pos > s))
            .or_else(|| matches.first().copied())?;
        let action = if matches.len() == 1 {
            MNC_EXECUTE
        } else {
            MNC_SELECT
        };
        Some(LRESULT((position | (action << 16)) as isize))
    })
}

#[cfg(test)]
mod tests {
    use super::mnemonic;
    #[test]
    fn menu_access_keys_ignore_escaped_ampersands() {
        assert_eq!(mnemonic("ファイル(&F)"), Some('f'));
        assert_eq!(mnemonic("Save && E&xit"), Some('x'));
        assert_eq!(mnemonic("A && B"), None);
        assert_eq!(mnemonic("plain"), None);
    }
}
