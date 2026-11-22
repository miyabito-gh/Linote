use std::collections::HashMap;

use windows::core::PCWSTR;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, TrackPopupMenu, MF_CHECKED, MF_STRING,
    TPM_RETURNCMD, TPM_RIGHTALIGN,
};

use super::{fileops::shorten_path, App};

impl App {
    pub(super) fn show_tab_list(&mut self) {
        if self.docs.len() < 2 {
            return;
        }
        self.reset_tab_tip();
        let mut counts = HashMap::new();
        for doc in &self.docs {
            *counts.entry(doc.title.as_str()).or_insert(0usize) += 1;
        }
        let bar = self.tabs.bar_rect();
        let mut anchor = POINT {
            x: self.tabs.list_button_rect().map_or(bar.right, |r| r.right),
            y: bar.bottom,
        };
        // SAFETY: メニューと座標はメインウィンドウのためにこの呼び出し内で使う。
        let chosen = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            for (index, doc) in self.docs.iter().enumerate() {
                let mut label = doc.title.clone();
                if counts[doc.title.as_str()] > 1 {
                    if let Some(parent) = doc.state.path().and_then(|path| path.parent()) {
                        label.push_str(&format!(
                            "  ({})",
                            shorten_path(&parent.to_string_lossy(), 52)
                        ));
                    } else {
                        label.push_str(&format!("  ({})", index + 1));
                    }
                }
                if doc.state.is_dirty() {
                    label.push_str("  •");
                }
                let wide: Vec<_> = label
                    .replace('&', "&&")
                    .replace(['\t', '\n', '\r'], " ")
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
                let flags = MF_STRING
                    | if self.active == Some(index) {
                        MF_CHECKED
                    } else {
                        Default::default()
                    };
                let _ = AppendMenuW(menu, flags, index + 1, PCWSTR(wide.as_ptr()));
            }
            let _ = ClientToScreen(self.hwnd, &mut anchor);
            let paint = crate::ui::menu::drawing::popup(menu);
            let result = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTALIGN,
                anchor.x,
                anchor.y,
                None,
                self.hwnd,
                None,
            );
            crate::ui::menu::drawing::popup_closed(paint);
            let _ = DestroyMenu(menu);
            result.0 as usize
        };
        if let Some(doc) = chosen.checked_sub(1).and_then(|index| self.docs.get(index)) {
            let _ = self.activate(doc.id());
        }
    }
}
