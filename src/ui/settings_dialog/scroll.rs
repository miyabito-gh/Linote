//! A settings page viewport. Its children retain their controls and edited values
//! while scrolling; only their positions change.
use std::collections::HashMap;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::platform::system::{dpi_for_window, scale};

const SUBCLASS: usize = 1;
const FOCUS_SUBCLASS: usize = 2;
const ENSURE_CHILD: u32 = WM_APP + 17;

struct ScrollState {
    position: i32,
    viewport: i32,
    content: i32,
    wheel: i32,
    line: i32,
    anchors: HashMap<isize, POINT>,
}

impl ScrollState {
    fn limit(&self) -> i32 {
        self.content.saturating_sub(self.viewport).max(0)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn create(
    parent: HWND,
    id: i32,
    x: i32,
    y: i32,
    width: i32,
    viewport_height: i32,
    content_height: i32,
) -> HWND {
    // SAFETY: The caller owns parent; this child is destroyed with it.
    let result = unsafe {
        CreateWindowExW(
            WS_EX_CONTROLPARENT,
            w!("STATIC"),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_CLIPCHILDREN,
            x,
            y,
            width.max(1),
            viewport_height.max(1),
            Some(parent),
            Some(HMENU(id as usize as *mut _)),
            GetModuleHandleW(PCWSTR::null()).ok().map(Into::into),
            None,
        )
    };
    let Ok(hwnd) = result else {
        return HWND::default();
    };
    let state = Box::new(ScrollState {
        position: 0,
        viewport: viewport_height.max(1),
        content: content_height.max(0),
        wheel: 0,
        line: scale(28, dpi_for_window(parent)).max(1),
        anchors: HashMap::new(),
    });
    let info = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
        nMin: 0,
        nMax: state.content.saturating_sub(1).max(0),
        nPage: state.viewport as u32,
        nPos: 0,
        ..Default::default()
    };
    let raw = Box::into_raw(state);
    // SAFETY: raw is held by this subclass until WM_NCDESTROY.
    unsafe {
        if !SetWindowSubclass(hwnd, Some(page_proc), SUBCLASS, raw as usize).as_bool() {
            drop(Box::from_raw(raw));
            let _ = DestroyWindow(hwnd);
            return HWND::default();
        }
        // Without SIF_DISABLENOSCROLL, Windows hides an unnecessary scrollbar.
        SetScrollInfo(hwnd, SB_VERT, &info, true);
    }
    hwnd
}

fn scroll_to(hwnd: HWND, state: &mut ScrollState, target: i32) {
    let target = target.clamp(0, state.limit());
    let delta = state.position - target;
    if delta == 0 {
        return;
    }
    // Include children outside the viewport. ScrollWindowEx by itself only
    // relocates children intersecting the scroll rectangle on some systems.
    let mut children = Vec::new();
    unsafe {
        let mut child = GetWindow(hwnd, GW_CHILD).ok();
        while let Some(window) = child {
            let mut rect = RECT::default();
            if GetWindowRect(window, &mut rect).is_ok() {
                let mut point = POINT {
                    x: rect.left,
                    y: rect.top,
                };
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point);
                let anchor = *state.anchors.entry(window.0 as isize).or_insert(POINT {
                    x: point.x,
                    y: point.y.saturating_add(state.position),
                });
                children.push((window, anchor));
            }
            child = GetWindow(window, GW_HWNDNEXT).ok();
        }
        state.position = target;
        let info = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_POS,
            nPos: target,
            ..Default::default()
        };
        SetScrollInfo(hwnd, SB_VERT, &info, true);
        ScrollWindowEx(
            hwnd,
            0,
            delta,
            None,
            None,
            None,
            None,
            SW_SCROLLCHILDREN | SW_INVALIDATE | SW_ERASE,
        );
        for (child, point) in children {
            let _ = SetWindowPos(
                child,
                None,
                point.x,
                point.y.saturating_sub(target),
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

fn ensure_child(hwnd: HWND, child: HWND, state: &mut ScrollState) {
    let mut rect = RECT::default();
    unsafe {
        if GetWindowRect(child, &mut rect).is_err() {
            return;
        }
        let mut point = POINT {
            x: rect.left,
            y: rect.top,
        };
        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point);
        // Native window coordinates can clamp far-offscreen positions. Keep
        // focus calculations in the logical content coordinate system as well.
        if let Some(anchor) = state.anchors.get(&(child.0 as isize)) {
            point.y = anchor.y.saturating_sub(state.position);
        }
        let bottom = point.y.saturating_add(rect.bottom - rect.top);
        let adjustment = if point.y < 0 {
            point.y
        } else if bottom > state.viewport {
            bottom - state.viewport
        } else {
            0
        };
        scroll_to(hwnd, state, state.position.saturating_add(adjustment));
    }
}

unsafe extern "system" fn page_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    match msg {
        WM_COMMAND | WM_NOTIFY => {
            // Notifications must reach the real dialog, where settings are read.
            if let Ok(parent) = unsafe { GetParent(hwnd) } {
                return unsafe { SendMessageW(parent, msg, Some(wparam), Some(lparam)) };
            }
        }
        WM_PARENTNOTIFY if wparam.0 & 0xffff == WM_CREATE as usize => {
            // Tab navigation must also reveal a newly focused offscreen control.
            unsafe {
                let _ = SetWindowSubclass(
                    HWND(lparam.0 as *mut _),
                    Some(focus_proc),
                    FOCUS_SUBCLASS,
                    0,
                );
            }
        }
        WM_PARENTNOTIFY if wparam.0 & 0xffff == WM_DESTROY as usize => {
            let state = unsafe { &mut *(data as *mut ScrollState) };
            state.anchors.remove(&lparam.0);
        }
        WM_VSCROLL => {
            let state = unsafe { &mut *(data as *mut ScrollState) };
            let action = (wparam.0 & 0xffff) as i32;
            let next = match action {
                n if n == SB_LINEUP.0 => state.position.saturating_sub(state.line),
                n if n == SB_LINEDOWN.0 => state.position.saturating_add(state.line),
                n if n == SB_PAGEUP.0 => state.position.saturating_sub(state.viewport),
                n if n == SB_PAGEDOWN.0 => state.position.saturating_add(state.viewport),
                n if n == SB_TOP.0 => 0,
                n if n == SB_BOTTOM.0 => state.limit(),
                n if n == SB_THUMBTRACK.0 || n == SB_THUMBPOSITION.0 => {
                    let mut info = SCROLLINFO {
                        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                        fMask: SIF_TRACKPOS,
                        ..Default::default()
                    };
                    if unsafe { GetScrollInfo(hwnd, SB_VERT, &mut info) }.is_ok() {
                        info.nTrackPos
                    } else {
                        state.position
                    }
                }
                _ => state.position,
            };
            scroll_to(hwnd, state, next);
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let state = unsafe { &mut *(data as *mut ScrollState) };
            state.wheel += ((wparam.0 >> 16) as u16 as i16) as i32;
            let steps = state.wheel / 120;
            state.wheel %= 120;
            let distance = steps.saturating_mul(state.line).saturating_mul(3);
            scroll_to(hwnd, state, state.position.saturating_sub(distance));
            return LRESULT(0);
        }
        ENSURE_CHILD => {
            let state = unsafe { &mut *(data as *mut ScrollState) };
            ensure_child(hwnd, HWND(lparam.0 as *mut _), state);
            return LRESULT(0);
        }
        WM_NCDESTROY => unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(page_proc), id);
            drop(Box::from_raw(data as *mut ScrollState));
            return DefSubclassProc(hwnd, msg, wparam, lparam);
        },
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn focus_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_SETFOCUS => {
            if let Ok(parent) = unsafe { GetParent(hwnd) } {
                unsafe {
                    SendMessageW(parent, ENSURE_CHILD, None, Some(LPARAM(hwnd.0 as isize)));
                }
            }
        }
        WM_NCDESTROY => unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(focus_proc), id);
        },
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_pages_scroll_offscreen_children_and_reveal_focus() {
        unsafe {
            let parent = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("page scroll test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                300,
                200,
                None,
                None,
                None,
                None,
            )
            .expect("test owner");
            let host = create(parent, 1, 0, 0, 200, 60, 100_000);
            assert!(!host.is_invalid());
            let control = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("EDIT"),
                w!("last"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                0,
                9_000,
                100,
                24,
                Some(host),
                None,
                None,
                None,
            )
            .expect("offscreen child");
            let position = || {
                let mut info = SCROLLINFO {
                    cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_ALL,
                    ..Default::default()
                };
                GetScrollInfo(host, SB_VERT, &mut info).unwrap();
                info
            };
            let child_y = || {
                let mut rect = RECT::default();
                GetWindowRect(control, &mut rect).unwrap();
                let mut point = POINT {
                    x: rect.left,
                    y: rect.top,
                };
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(host, &mut point);
                point.y
            };
            assert_eq!(position().nPage, 60);
            assert_eq!(position().nMax, 99_999);
            SendMessageW(host, WM_VSCROLL, Some(WPARAM(SB_BOTTOM.0 as usize)), None);
            assert_eq!(position().nPos, 99_940);
            assert!(child_y() < 0);
            SendMessageW(host, ENSURE_CHILD, None, Some(LPARAM(control.0 as isize)));
            assert!(child_y() >= 0 && child_y() + 24 <= 60);
            SendMessageW(host, WM_VSCROLL, Some(WPARAM(SB_BOTTOM.0 as usize)), None);
            SendMessageW(host, WM_VSCROLL, Some(WPARAM(SB_TOP.0 as usize)), None);
            assert_eq!(child_y(), 9_000);
            // The same path used by a tabbed-to child must reveal the whole control.
            SendMessageW(host, ENSURE_CHILD, None, Some(LPARAM(control.0 as isize)));
            assert!(child_y() >= 0 && child_y() + 24 <= 60);
            SendMessageW(host, WM_VSCROLL, Some(WPARAM(SB_PAGEDOWN.0 as usize)), None);
            assert!(position().nPos <= 99_940);
            let _ = DestroyWindow(parent);
        }
    }
}
