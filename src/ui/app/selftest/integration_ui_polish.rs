//! Menu callbacks must work during an App borrow and retain Alt access keys.
use super::integration::step;
use super::Check;
use crate::appdata::ThemeMode;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::Controls::{MEASUREITEMSTRUCT, ODT_MENU};
use windows::Win32::UI::WindowsAndMessaging::*;

pub(super) fn run() -> Check {
    step(|app| -> Check {
        for mode in [ThemeMode::Dark, ThemeMode::Light, ThemeMode::Dark] {
            app.set_theme_mode(mode);
            let menu = app.menu().ok_or("メニューがない")?.handle();
            let mut info = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_DATA,
                ..Default::default()
            };
            unsafe { GetMenuItemInfoW(menu, 0, true, &mut info) }.map_err(|e| e.to_string())?;
            if info.fType.contains(MFT_OWNERDRAW) != (mode == ThemeMode::Dark) {
                return Err("テーマ切替でメニューの描画方式が戻らない".into());
            }
            if mode == ThemeMode::Dark {
                let key = super::super::dispatch(
                    app.hwnd(),
                    WM_MENUCHAR,
                    WPARAM(b'f' as usize),
                    LPARAM(menu.0 as isize),
                );
                if key.map(|r| r.0 as u32) != Some(MNC_EXECUTE << 16) {
                    return Err("ダークメニューのAlt+F".into());
                }
                let mut measure = MEASUREITEMSTRUCT {
                    CtlType: ODT_MENU,
                    itemData: info.dwItemData,
                    ..Default::default()
                };
                let result = super::super::dispatch(
                    app.hwnd(),
                    WM_MEASUREITEM,
                    WPARAM(0),
                    LPARAM(&mut measure as *mut _ as isize),
                );
                if result.is_none() || measure.itemWidth == 0 || measure.itemHeight == 0 {
                    return Err("App借用中のメニュー寸法コールバック".into());
                }
            }
        }
        Ok(())
    })?
}
