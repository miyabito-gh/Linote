//! 画面の自己診断（`linote::ui::app::selftest`）を実行する。
//!
//! 画面を作れない環境（仮想ディスプレイのない Wine など）では飛ばす。必ず実行するには
//! 環境変数 `LINOTE_REQUIRE_GUI=1` を付ける:
//! `xvfb-run -a env LINOTE_REQUIRE_GUI=1 WINEDEBUG=-all cargo test --test ui_selftest`
#![cfg(windows)]

use linote::ui::app::selftest;

#[test]
fn gui_self_test() {
    let code = selftest::run_self_test();
    if code == selftest::UNAVAILABLE && std::env::var_os("LINOTE_REQUIRE_GUI").is_none() {
        eprintln!("画面を作れないので飛ばします（LINOTE_REQUIRE_GUI=1 で必須にできます）");
        return;
    }
    assert_eq!(code, selftest::PASSED);
}
