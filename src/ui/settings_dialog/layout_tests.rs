//! Measure the actual controls, rather than asserting fixed implementation coordinates.
use super::*;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DrawTextW, SelectObject, DT_CALCRECT, DT_NOPREFIX, DT_WORDBREAK,
    HFONT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

struct Session;
impl SettingsSession for Session {
    fn current(&self) -> Settings {
        let mut s = Settings::default();
        s.view.font = "日本語を含む長いフォントファミリー名の表示確認".into();
        s
    }
    fn warning(&self) -> Option<String> {
        Some("設定ファイルを読めません。OK・適用は、この起動の間だけ反映します（保存しません）。ファイルを直して再読み込みすると、保存できるようになります。".repeat(2))
    }
    fn apply(&mut self, _: &Settings, edited: &Settings) -> Settings {
        edited.clone()
    }
    fn command_defs(&self) -> Vec<CommandDef> {
        Vec::new()
    }
    fn confirm(&mut self, _: HWND, _: &str, _: &str) -> bool {
        false
    }
    fn notice(&mut self, _: HWND, _: &str) {}
}

fn inspect(parent: HWND, dpi: u32, failures: &mut Vec<String>) {
    unsafe {
        let mut child = GetWindow(parent, GW_CHILD).ok();
        let mut area = RECT::default();
        let _ = GetClientRect(parent, &mut area);
        while let Some(control) = child {
            let mut class = [0u16; 64];
            let n = GetClassNameW(control, &mut class).max(0) as usize;
            let class = String::from_utf16_lossy(&class[..n]).to_ascii_uppercase();
            let text = get_text(control);
            if (class == "STATIC" || class == "BUTTON") && !text.is_empty() {
                let mut rect = RECT::default();
                let _ = GetClientRect(control, &mut rect);
                let style = GetWindowLongPtrW(control, GWL_STYLE) as u32;
                let pad = if class == "BUTTON" {
                    scale(
                        if style & 0xf == super::BS_AUTOCHECKBOX {
                            22
                        } else {
                            16
                        },
                        dpi,
                    )
                } else {
                    0
                };
                let width = (rect.right - pad).max(1);
                let font = HFONT(send(control, WM_GETFONT, 0, 0) as *mut _);
                let dc = CreateCompatibleDC(None);
                let old = SelectObject(dc, font.into());
                let mut measure = RECT {
                    right: width,
                    ..Default::default()
                };
                let mut wide: Vec<u16> = text.encode_utf16().collect();
                DrawTextW(
                    dc,
                    &mut wide,
                    &mut measure,
                    DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
                );
                SelectObject(dc, old);
                let _ = DeleteDC(dc);
                if measure.bottom > rect.bottom {
                    failures.push(format!(
                        "{dpi}dpi: {text:?} needs {}px, has {}px",
                        measure.bottom, rect.bottom
                    ));
                }
                let mut position = RECT::default();
                let _ = GetWindowRect(control, &mut position);
                let mut point = windows::Win32::Foundation::POINT {
                    x: position.right,
                    y: position.top,
                };
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(parent, &mut point);
                if point.x > area.right + 1 {
                    failures.push(format!("{dpi}dpi horizontal overflow: {text}"));
                }
            }
            inspect(control, dpi, failures);
            child = GetWindow(control, GW_HWNDNEXT).ok();
        }
    }
}

#[test]
fn all_settings_text_fits_at_scaled_dpi_and_pages_scroll() {
    crate::platform::system::enable_per_monitor_dpi();
    let owner = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            windows::core::w!("STATIC"),
            windows::core::w!("settings layout test"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            1000,
            700,
            None,
            None,
            None,
            None,
        )
    }
    .expect("test owner");
    let mut failures = Vec::new();
    for dpi in [96, 144, 192] {
        let mut session = Session;
        let mut driver = |dlg| {
            for page in pages() {
                select_page(dlg, page.id);
                inspect(dlg, dpi, &mut failures);
                capture(dlg, dpi, page.id);
            }
            // Last item remains accessible when the page exceeds the monitor work area.
            select_page(dlg, "view");
            if let Ok(host) = dialog_control(dlg, ID_PAGE_HOST_BASE + 2) {
                send(host, WM_VSCROLL, SB_BOTTOM.0 as usize, 0);
                if let Some(last) = item_control(dlg, "view.auto_indent") {
                    let mut rect = RECT::default();
                    let mut client = RECT::default();
                    unsafe {
                        let _ = GetWindowRect(last, &mut rect);
                        let _ = GetClientRect(host, &mut client);
                        let mut point = windows::Win32::Foundation::POINT {
                            x: rect.left,
                            y: rect.bottom,
                        };
                        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(host, &mut point);
                        if point.y > client.bottom {
                            failures.push(format!("{dpi}dpi: final row is not reachable"));
                        }
                    }
                }
            }
            press(dlg, IDCANCEL.0);
        };
        run_at_dpi(owner, &mut session, Some(&mut driver), dpi);
    }
    unsafe {
        let _ = DestroyWindow(owner);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Optional bitmap evidence from the same real controls checked above.
fn capture(dlg: HWND, dpi: u32, page: &str) {
    let Some(root) = std::env::var_os("LINOTE_LAYOUT_CAPTURE") else {
        return;
    };
    use windows::Win32::Graphics::Gdi::*;
    unsafe {
        let _ = RedrawWindow(
            Some(dlg),
            None,
            None,
            RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW,
        );
        let mut rect = RECT::default();
        let _ = GetWindowRect(dlg, &mut rect);
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        let source = GetWindowDC(Some(dlg));
        let dc = CreateCompatibleDC(Some(source));
        let bitmap = CreateCompatibleBitmap(source, width, height);
        let old = SelectObject(dc, bitmap.into());
        let _ = BitBlt(dc, 0, 0, width, height, Some(source), 0, 0, SRCCOPY);
        SelectObject(dc, old);
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
        ReleaseDC(Some(dlg), source);
        if lines == 0 {
            return;
        }
        let mut bmp = Vec::new();
        bmp.extend(b"BM");
        bmp.extend((54u32 + pixels.len() as u32).to_le_bytes());
        bmp.extend(0u32.to_le_bytes());
        bmp.extend(54u32.to_le_bytes());
        bmp.extend(40u32.to_le_bytes());
        bmp.extend(width.to_le_bytes());
        bmp.extend((-height).to_le_bytes());
        bmp.extend(1u16.to_le_bytes());
        bmp.extend(32u16.to_le_bytes());
        bmp.extend([0u8; 24]);
        bmp.extend(pixels);
        let root = std::path::PathBuf::from(root);
        let _ = std::fs::create_dir_all(&root);
        let _ = std::fs::write(root.join(format!("{dpi}-{page}.bmp")), bmp);
    }
}

#[test]
fn invalid_setting_on_hidden_page_is_revealed_and_selected() {
    let owner = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            windows::core::w!("STATIC"),
            windows::core::w!("settings validation test"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            1000,
            700,
            None,
            None,
            None,
            None,
        )
    }
    .expect("test owner");
    let mut session = Session;
    let mut driver = |dlg| {
        assert!(select_page(dlg, "view"));
        let ctl = item_control(dlg, "view.font_size").expect("font size control");
        set_text(ctl, "999");
        assert!(select_page(dlg, "general"));
        assert!(!unsafe { IsWindowVisible(ctl) }.as_bool());
        let state = state_of(dlg).expect("dialog state");
        let (page, invalid, _) = read_controls(dlg, state).expect_err("invalid font size");
        assert_eq!(invalid, ctl);
        reveal_invalid_control(dlg, state, page, invalid);
        assert!(unsafe { IsWindowVisible(ctl) }.as_bool());
        let list = dialog_control(dlg, ID_PAGE_LIST).unwrap();
        assert_eq!(send(list, super::LB_GETCURSEL, 0, 0), page as isize);
        assert_eq!(
            unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() },
            ctl
        );
        // EM_GETSEL returns the selection bounds packed into the result.
        assert_eq!(send(ctl, 0xB0, 0, 0) as u32, 3 << 16);
        let mut control_rect = RECT::default();
        let mut page_rect = RECT::default();
        unsafe {
            GetWindowRect(ctl, &mut control_rect).unwrap();
            GetWindowRect(GetParent(ctl).unwrap(), &mut page_rect).unwrap();
        }
        assert!(control_rect.top >= page_rect.top);
        assert!(control_rect.bottom <= page_rect.bottom);
        set_text(ctl, "12");
        assert!(read_controls(dlg, state).is_ok());
        press(dlg, IDCANCEL.0);
    };
    run_at_dpi(owner, &mut session, Some(&mut driver), 192);
    unsafe {
        let _ = DestroyWindow(owner);
    }
}
