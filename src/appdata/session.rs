//! 前回のタブの記録（`session.json`、V1）。GUI に依存しない。
//!
//! ```json
//! {
//!   "version": 1,
//!   "active": 1,
//!   "docs": [
//!     { "path": "C:\\work\\a.txt", "caret": 120, "first_line": 3 },
//!     { "path": "C:\\work\\b.rs", "caret": 0, "first_line": 0 }
//!   ]
//! }
//! ```
//!
//! - 対象はファイルのある通常文書と巨大文書だけ（無題・メモ・クリップボード由来は含めない）。
//! - `docs` はタブ順。`active` は `docs` の中の添字（アクティブなタブが対象外なら省く）。
//! - `caret` はカーソルのバイト位置、`first_line` は表示の先頭行（0 始まり）。
//! - 読めない・壊れている記録は「なし」と同じに扱う（起動を妨げない）。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// 記録の形式の版。
pub const SESSION_VERSION: u64 = 1;

/// 記録するタブの最大数（壊れた記録や極端な数のタブで起動が重くならないように）。
pub const MAX_SESSION_DOCS: usize = 200;

/// タブ 1 つの記録。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDoc {
    pub path: PathBuf,
    /// カーソルのバイト位置。
    pub caret: u64,
    /// 表示の先頭行（0 始まり）。
    pub first_line: u64,
}

/// 前回のタブの一覧。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    pub docs: Vec<SessionDoc>,
    /// アクティブなタブ（`docs` の添字）。
    pub active: Option<usize>,
}

impl Session {
    /// JSON にする（末尾に改行）。
    pub fn to_json(&self) -> String {
        let docs: Vec<Value> = self
            .docs
            .iter()
            .map(|d| {
                json!({
                    "path": d.path.to_string_lossy(),
                    "caret": d.caret,
                    "first_line": d.first_line,
                })
            })
            .collect();
        let mut map = serde_json::Map::new();
        map.insert("version".into(), SESSION_VERSION.into());
        if let Some(a) = self.active.filter(|&a| a < self.docs.len()) {
            map.insert("active".into(), a.into());
        }
        map.insert("docs".into(), Value::Array(docs));
        let mut text = serde_json::to_string_pretty(&Value::Object(map))
            .expect("JSON の値は常に文字列にできる");
        text.push('\n');
        text
    }

    /// JSON から読む。形式が違う・壊れている場合は `None`。読めない要素は飛ばす。
    pub fn from_json(text: &str) -> Option<Session> {
        let value: Value = serde_json::from_str(text).ok()?;
        let map = value.as_object()?;
        let items = map.get("docs")?.as_array()?;
        let wanted_active = map.get("active").and_then(Value::as_u64);
        let mut docs = Vec::new();
        let mut active = None;
        for (i, item) in items.iter().enumerate() {
            let Some(path) = item
                .get("path")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
            else {
                continue;
            };
            if docs.len() >= MAX_SESSION_DOCS {
                break;
            }
            if wanted_active == Some(i as u64) {
                active = Some(docs.len());
            }
            docs.push(SessionDoc {
                path: PathBuf::from(path),
                caret: item.get("caret").and_then(Value::as_u64).unwrap_or(0),
                first_line: item.get("first_line").and_then(Value::as_u64).unwrap_or(0),
            });
        }
        Some(Session { docs, active })
    }

    /// 存在するファイルだけを残す（存在しないものは黙って飛ばす）。同じパスの重複も除く。
    /// アクティブなタブが飛ばされたときは、なしにする。
    pub fn existing(self, exists: impl Fn(&Path) -> bool) -> Session {
        let mut docs: Vec<SessionDoc> = Vec::new();
        let mut active = None;
        for (i, d) in self.docs.into_iter().enumerate() {
            if !exists(&d.path) || docs.iter().any(|k| k.path == d.path) {
                continue;
            }
            if self.active == Some(i) {
                active = Some(docs.len());
            }
            docs.push(d);
        }
        Session { docs, active }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(path: &str, caret: u64, first_line: u64) -> SessionDoc {
        SessionDoc {
            path: PathBuf::from(path),
            caret,
            first_line,
        }
    }

    #[test]
    fn round_trips_order_active_and_cursor() {
        let s = Session {
            docs: vec![
                doc(r"C:\メモ\あ い.txt", 12, 3),
                doc(r"C:\work\b.rs", 0, 0),
                doc(r"D:\big.log", 99, 7),
            ],
            active: Some(1),
        };
        let text = s.to_json();
        assert!(text.ends_with("}\n"));
        assert_eq!(Session::from_json(&text), Some(s));
    }

    #[test]
    fn empty_session_round_trips() {
        let s = Session::default();
        assert_eq!(Session::from_json(&s.to_json()), Some(s));
    }

    #[test]
    fn broken_input_is_none_and_bad_items_are_skipped() {
        for bad in ["", "{", "[]", r#"{"docs":1}"#, r#"{"version":1}"#] {
            assert_eq!(Session::from_json(bad), None, "{bad}");
        }
        let s = Session::from_json(
            r#"{"active":2,"docs":[{"path":"a"},{"caret":3},{"path":"c","caret":"x"},5]}"#,
        )
        .unwrap();
        assert_eq!(s.docs.len(), 2);
        assert_eq!(s.docs[1], doc("c", 0, 0));
        // 飛ばした要素の分だけ添字が詰まる。
        assert_eq!(s.active, Some(1));
        // 範囲外の active は書き出さない。
        let s = Session {
            docs: vec![doc("a", 0, 0)],
            active: Some(5),
        };
        assert_eq!(Session::from_json(&s.to_json()).unwrap().active, None);
    }

    #[test]
    fn missing_files_are_skipped_and_active_follows() {
        let s = Session {
            docs: vec![
                doc("a", 1, 0),
                doc("gone", 2, 0),
                doc("c", 3, 0),
                doc("a", 4, 0),
            ],
            active: Some(2),
        };
        let kept = s.clone().existing(|p| p != Path::new("gone"));
        assert_eq!(kept.docs, vec![doc("a", 1, 0), doc("c", 3, 0)]);
        assert_eq!(kept.active, Some(1));
        let lost = Session {
            active: Some(1),
            ..s
        }
        .existing(|p| p != Path::new("gone"));
        assert_eq!(lost.active, None);
    }
}
