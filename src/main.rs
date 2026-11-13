#![cfg_attr(not(test), windows_subsystem = "windows")]

#[cfg(windows)]
mod smoke {
    use linote::sci_sys::*;
    use windows::core::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;

    pub fn run() {
        unsafe {
            let hinst = GetModuleHandleW(None).unwrap();
            assert!(Scintilla_RegisterClasses(hinst.0 as _) != 0);
            assert!(!linote_create_lexer(c"rust".as_ptr()).is_null());
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("Scintilla"),
                w!(""),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                400,
                300,
                None,
                None,
                Some(hinst.into()),
                None,
            )
            .unwrap();
            println!("scintilla ok {:?}", hwnd.0);
            let _ = DestroyWindow(hwnd);
        }
    }
}

fn main() {
    #[cfg(windows)]
    smoke::run();
}
