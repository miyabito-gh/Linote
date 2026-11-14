//! 設定の差分を、プロセス間ロック内で最新ファイルへ反映する。

use std::path::Path;

use serde_json::Value;

use super::Settings;
use crate::core::{Error, Result, TaskContext};
use crate::file::{self, LockFile};

/// 成功したときだけ baseline を進める。失敗した変更も次回の保存で再試行できる。
pub fn save(path: &Path, baseline: &mut Settings, current: &Settings) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::corrupt("設定", "親フォルダーがありません"))?;
    std::fs::create_dir_all(parent).map_err(|e| Error::io("設定フォルダーを作成", parent, e))?;
    let _lock = LockFile::try_acquire(&path.with_extension("json.lock"))?;
    let latest = match std::fs::read_to_string(path) {
        Ok(text) => Settings::from_json(&text)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => baseline.clone(),
        Err(e) => return Err(Error::io("設定を読む", path, e)),
    };
    let parse = |settings: &Settings| {
        serde_json::from_str::<Value>(&settings.to_json()).expect("設定のJSON")
    };
    let mut merged = parse(&latest);
    merge_changes(&parse(baseline), &parse(current), &mut merged);
    let merged = Settings::from_json(&merged.to_string())?;
    file::write_atomic(path, merged.to_json().as_bytes(), &TaskContext::detached())?;
    *baseline = current.clone();
    Ok(())
}

fn merge_changes(before: &Value, after: &Value, latest: &mut Value) {
    if before == after {
        return;
    }
    if let (Some(before), Some(after), Some(latest)) = (
        before.as_object(),
        after.as_object(),
        latest.as_object_mut(),
    ) {
        for key in before.keys() {
            if !after.contains_key(key) {
                latest.remove(key);
            }
        }
        for (key, value) in after {
            if let Some(old) = before.get(key) {
                if old != value {
                    merge_changes(old, value, latest.entry(key.clone()).or_insert(Value::Null));
                }
            } else {
                latest.insert(key.clone(), value.clone());
            }
        }
    } else {
        *latest = after.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appdata::settings::ThemeMode;
    use crate::file::testutil::TempDir;

    #[test]
    fn stale_windows_preserve_each_others_changes_and_can_revert() {
        let dir = TempDir::new("settings-merge");
        let path = dir.path().join("settings.json");
        let mut a = Settings::default();
        let mut b = a.clone();
        let mut edited = a.clone();
        edited.theme = ThemeMode::Dark;
        save(&path, &mut a, &edited).unwrap();
        let mut other = b.clone();
        other.view.tab_width = 8;
        save(&path, &mut b, &other).unwrap();
        let loaded = Settings::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.theme, ThemeMode::Dark);
        assert_eq!(loaded.view.tab_width, 8);
        edited.theme = ThemeMode::System;
        save(&path, &mut a, &edited).unwrap();
        let loaded = Settings::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.theme, ThemeMode::System);
        assert_eq!(loaded.view.tab_width, 8);
    }

    #[test]
    fn broken_settings_are_never_overwritten() {
        let dir = TempDir::new("settings-merge-broken");
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "broken").unwrap();
        let mut baseline = Settings::default();
        let mut edited = baseline.clone();
        edited.theme = ThemeMode::Dark;
        assert!(save(&path, &mut baseline, &edited).is_err());
        assert_eq!(baseline, Settings::default());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "broken");
    }

    #[test]
    fn preserves_external_unknown_fields_and_deletions() {
        let mut latest = serde_json::json!({"view": {"size": 12, "future": true}});
        merge_changes(
            &serde_json::json!({"view": {"size": 11, "removed": true}}),
            &serde_json::json!({"view": {"size": 14, "removed": true}}),
            &mut latest,
        );
        assert_eq!(
            latest,
            serde_json::json!({"view": {"size": 14, "future": true}})
        );
    }
}
