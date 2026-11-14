//! 巨大ファイルのビューの純粋な計算（`src/ui/bigview/logic.rs`）のテスト。Linux でも動く。
#![allow(dead_code)]

#[path = "../src/ui/bigview/logic.rs"]
mod logic;

use logic::*;

#[test]
fn scroll_mapping_round_trips_roughly() {
    let len = 10_000_000_000u64;
    assert_eq!(scroll_pos(0, len), 0);
    assert_eq!(scroll_pos(len, len), SCROLL_RANGE);
    assert_eq!(scroll_pos(5, 0), 0);
    let pos = scroll_pos(len / 2, len);
    assert_eq!(pos, SCROLL_RANGE / 2);
    assert_eq!(offset_from_scroll(pos, len), len / 2);
    assert_eq!(offset_from_scroll(-5, len), 0);
    assert_eq!(offset_from_scroll(i32::MAX, len), len);
}

#[test]
fn scroll_page_is_clamped() {
    assert_eq!(scroll_page(0, 100), 1);
    assert_eq!(scroll_page(1000, 100), SCROLL_RANGE as u32);
    assert_eq!(scroll_page(10, 100), (SCROLL_RANGE / 10) as u32);
}

#[test]
fn wide_chars_and_tabs() {
    let t = "a\tb日本c";
    let c = cells(t);
    assert_eq!(c[0].col, 0);
    assert_eq!(c[1].cols, 3); // タブは次の 4 桁目まで
    assert_eq!(c[2].col, 4);
    assert_eq!(c[3].cols, 2);
    assert_eq!(total_cols(t), 4 + 1 + 4 + 1);
    assert_eq!(col_of_byte(t, 2), 4);
    assert_eq!(col_of_byte(t, 3), 5);
    assert_eq!(col_of_byte(t, 999), total_cols(t));
}

#[test]
fn byte_at_col_picks_nearest_boundary() {
    let t = "ab日c";
    assert_eq!(byte_at_col(t, -3.0), 0);
    assert_eq!(byte_at_col(t, 0.4), 0);
    assert_eq!(byte_at_col(t, 0.6), 1);
    assert_eq!(byte_at_col(t, 2.9), 2); // 全角の左半分
    assert_eq!(byte_at_col(t, 3.1), 5); // 全角の右半分は次の文字の手前
    assert_eq!(byte_at_col(t, 100.0), t.len());
}

#[test]
fn visible_part_slices_by_columns() {
    let v = visible_part("abcdef", 2, 3);
    assert_eq!(
        v,
        Visible {
            text: "cde".into(),
            col0: 2
        }
    );
    assert_eq!(visible_part("a\tb", 0, 10).text, "a   b");
    // 全角が左端で切れるときは、その文字の左端から描く
    let v = visible_part("日本", 1, 10);
    assert_eq!(v.col0, 0);
    assert_eq!(v.text, "日本");
    assert_eq!(visible_part("ab\u{1}", 0, 10).text, "ab\u{b7}");
    assert_eq!(visible_part("abc", 10, 5).text, "");
}

#[test]
fn selection_clips_to_rows() {
    assert_eq!(selection_in_row(10, 20, 0, 5), None);
    assert_eq!(selection_in_row(10, 20, 12, 15), Some((2, 5, false)));
    assert_eq!(selection_in_row(10, 20, 5, 25), Some((0, 10, true)));
    assert_eq!(selection_in_row(10, 20, 20, 25), Some((10, 10, true)));
    assert_eq!(selection_in_row(10, 20, 21, 25), None);
    assert_eq!(selection_in_row(10, 20, 15, 15), None);
}

#[test]
fn copy_range_limits() {
    assert_eq!(copy_range(9, 3, 100), Some((3, 9)));
    assert_eq!(copy_range(0, 101, 100), None);
    assert_eq!(copy_range(4, 4, 0), Some((4, 4)));
}

#[test]
fn goto_input_parsing() {
    assert_eq!(parse_line_number(" 1,234 "), Some(1234));
    assert_eq!(parse_line_number("１２３"), Some(123));
    assert_eq!(parse_line_number("0"), None);
    assert_eq!(parse_line_number("-3"), None);
    assert_eq!(parse_line_number("abc"), None);
    assert_eq!(parse_line_number(""), None);
    assert_eq!(parse_line_number("99999999999999999999"), None);
    assert_eq!(gutter_digits(5), 4);
    assert_eq!(gutter_digits(123_456_789), 9);
}

#[test]
fn hscroll_and_rows() {
    assert_eq!(hscroll_max(50, 80), 0);
    assert_eq!(hscroll_max(200, 80), 121);
    assert_eq!(rows_that_fit(100, 18), 5);
    assert_eq!(rows_that_fit(5, 18), 1);
    assert_eq!(rows_that_fit(5, 0), 1);
}
