//! タブバー上のホイールで前後のタブへ移る。本文上の入力は消費しない。
use windows::Win32::Foundation::{LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::WindowsAndMessaging::{IsChild, MSG, WM_MOUSEWHEEL};

use super::App;

#[derive(Default)]
pub(super) struct TabWheel {
    remainder: i32,
}

impl TabWheel {
    fn target(
        &mut self,
        delta: i16,
        over_tabs: bool,
        active: Option<usize>,
        count: usize,
    ) -> Option<usize> {
        let Some(active) = active.filter(|&i| over_tabs && i < count) else {
            self.remainder = 0;
            return None;
        };
        self.remainder += i32::from(delta);
        let steps = self.remainder / 120;
        self.remainder %= 120;
        // 上方向で左、下方向で右。端を越えた入力は反対端へ回さない。
        let next = (active as i64 - i64::from(steps)).clamp(0, count as i64 - 1) as usize;
        if (next == 0 && self.remainder > 0) || (next == count - 1 && self.remainder < 0) {
            self.remainder = 0;
        }
        (next != active).then_some(next)
    }
}

impl App {
    /// Scintilla が入力を消費する前に、タブバー上の入力だけを拾う。
    pub(super) fn translate_tab_wheel(&mut self, msg: &MSG) -> bool {
        if msg.message != WM_MOUSEWHEEL {
            return false;
        }
        // 巨大ファイルのビューやプレビューがフォーカスを持つ場合も対象にする。
        // SAFETY: 生存中のウィンドウ同士の親子関係だけを問い合わせる。
        if msg.hwnd != self.hwnd && !unsafe { IsChild(self.hwnd, msg.hwnd) }.as_bool() {
            return false;
        }
        self.on_tab_wheel(msg.wParam, msg.lParam)
    }

    /// WM_MOUSEWHEEL の座標は画面座標（マウス移動通知のクライアント座標とは異なる）。
    pub(super) fn on_tab_wheel(&mut self, wparam: WPARAM, lparam: LPARAM) -> bool {
        let mut point = POINT {
            x: (lparam.0 & 0xffff) as i16 as i32,
            y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
        };
        // SAFETY: 自分のウィンドウの座標変換だけを行う。
        let over_tabs = unsafe { ScreenToClient(self.hwnd, &mut point) }.as_bool()
            && crate::ui::window::contains(&self.layout.tabs, point.x, point.y);
        let delta = ((wparam.0 >> 16) & 0xffff) as u16 as i16;
        if let Some(next) = self
            .tab_wheel
            .target(delta, over_tabs, self.active, self.docs.len())
        {
            let _ = self.activate(self.docs[next].id());
        }
        over_tabs
    }
}

#[cfg(test)]
mod tests {
    use super::TabWheel;

    #[test]
    fn wheel_moves_both_directions_and_handles_multiple_notches() {
        let mut wheel = TabWheel::default();
        assert_eq!(wheel.target(-120, true, Some(1), 8), Some(2));
        assert_eq!(wheel.target(120, true, Some(2), 8), Some(1));
        assert_eq!(wheel.target(-360, true, Some(1), 8), Some(4));
    }

    #[test]
    fn high_resolution_deltas_accumulate_and_reset_outside_tabs() {
        let mut wheel = TabWheel::default();
        assert_eq!(wheel.target(-40, true, Some(1), 8), None);
        assert_eq!(wheel.target(-40, true, Some(1), 8), None);
        assert_eq!(wheel.target(-40, true, Some(1), 8), Some(2));
        assert_eq!(wheel.target(-60, true, Some(2), 8), None);
        assert_eq!(wheel.target(-120, false, Some(2), 8), None);
        assert_eq!(wheel.target(-60, true, Some(2), 8), None);
        assert_eq!(wheel.target(-60, true, Some(2), 8), Some(3));
    }

    #[test]
    fn edges_do_not_wrap_or_retain_overscroll() {
        let mut wheel = TabWheel::default();
        assert_eq!(wheel.target(32767, true, Some(0), 5), None);
        assert_eq!(wheel.target(-120, true, Some(0), 5), Some(1));
        assert_eq!(wheel.target(-32768, true, Some(4), 5), None);
        assert_eq!(wheel.target(120, true, Some(4), 5), Some(3));
        assert_eq!(wheel.target(-120, true, None, 0), None);
        assert_eq!(wheel.target(-120, true, Some(0), 1), None);
    }
}
