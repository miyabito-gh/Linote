//! Alt を他の操作（Alt＋ドラッグの矩形選択、Alt＋Shift＋矢印）に使ったあと、Alt を離した
//! ときにメニューバーへフォーカスが移らないようにする（V1）。
//!
//! 何も押さずに Alt だけを押して離すと、Windows はメニューバーを有効にする。編集欄が
//! Alt を使った操作を処理しても、離したときの `WM_SYSKEYUP`（VK_MENU）が既定の処理に
//! 回るとメニューが有効になり、次の入力が奪われる。Alt を使った操作があった間は、その
//! `WM_SYSKEYUP` を既定の処理に渡さない。

const WM_SYSKEYDOWN: u32 = 0x0104;
const WM_SYSKEYUP: u32 = 0x0105;
const WM_LBUTTONDOWN: u32 = 0x0201;
const VK_MENU: usize = 0x12;
/// `WM_SYSKEYDOWN` の lParam: 直前にも押されていた（キーリピート）。
const PREVIOUS_STATE: isize = 1 << 30;

#[derive(Debug, Default)]
pub(super) struct AltTracker {
    /// 今の Alt の押下で、Alt と組み合わせた操作があった。
    used: bool,
}

impl AltTracker {
    /// 編集欄宛てのメッセージを見て、離したときの `WM_SYSKEYUP` を既定の処理に渡さない
    /// （握りつぶす）なら `true`。`alt_down` は、そのときの Alt の押下状態。
    pub(super) fn track(
        &mut self,
        message: u32,
        wparam: usize,
        lparam: isize,
        alt_down: bool,
    ) -> bool {
        match message {
            WM_SYSKEYDOWN if wparam == VK_MENU => {
                if lparam & PREVIOUS_STATE == 0 {
                    self.used = false;
                }
                false
            }
            WM_SYSKEYDOWN => {
                self.used = true;
                false
            }
            WM_LBUTTONDOWN => {
                if alt_down {
                    self.used = true;
                }
                false
            }
            WM_SYSKEYUP if wparam == VK_MENU => std::mem::take(&mut self.used),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_alone_still_opens_the_menu() {
        let mut t = AltTracker::default();
        assert!(!t.track(WM_SYSKEYDOWN, VK_MENU, 0, true));
        assert!(!t.track(WM_SYSKEYUP, VK_MENU, 0, false));
    }

    #[test]
    fn alt_drag_does_not_activate_the_menu() {
        let mut t = AltTracker::default();
        t.track(WM_SYSKEYDOWN, VK_MENU, 0, true);
        t.track(WM_LBUTTONDOWN, 0, 0, true);
        // キーリピートの Alt は、使った印を消さない。
        t.track(WM_SYSKEYDOWN, VK_MENU, PREVIOUS_STATE, true);
        assert!(t.track(WM_SYSKEYUP, VK_MENU, 0, false));
        // 次に Alt だけ押して離したときは、通常どおり。
        t.track(WM_SYSKEYDOWN, VK_MENU, 0, true);
        assert!(!t.track(WM_SYSKEYUP, VK_MENU, 0, false));
    }

    #[test]
    fn alt_shift_arrow_does_not_activate_the_menu() {
        let mut t = AltTracker::default();
        t.track(WM_SYSKEYDOWN, VK_MENU, 0, true);
        t.track(WM_SYSKEYDOWN, 0x27, 0, true); // VK_RIGHT
        assert!(t.track(WM_SYSKEYUP, VK_MENU, 0, false));
    }

    #[test]
    fn plain_click_without_alt_is_ignored() {
        let mut t = AltTracker::default();
        t.track(WM_SYSKEYDOWN, VK_MENU, 0, true);
        t.track(WM_LBUTTONDOWN, 0, 0, false);
        assert!(!t.track(WM_SYSKEYUP, VK_MENU, 0, false));
    }
}
