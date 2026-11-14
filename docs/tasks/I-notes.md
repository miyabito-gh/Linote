# 統合（I1/I2/I4）で反映する依頼事項

各ワーカーの報告から、統合時に `src/ui/app` と共有ファイルへ反映すべきものを集めたもの。

## A（ファイル入出力）
- `stub_io.rs`（B の仮実装）を `file::read_text`／`file::save_text` に置き換える。
- `LoadedText::is_lossy()` のとき警告し、上書き前に確認する。
- 判定の信頼度が Low のとき、文字コードの指定を促す（手動指定で開き直すコマンド）。
- `SaveOutcome::Unmappable` の確認ダイアログ（承知したら `OnUnmappable::Replace` で再保存）。
- 保存失敗時は `file::classify` で原因別の文言を出す。外部変更は `Error::ExternalChange` → 別名保存を案内。

## D（復元）
- 起動時にワーカーで `RecoveryStore::scan()` → 孤児の候補を復元するか利用者に聞く（`Candidate::read_text`、`into_slot`、`discard`）。復元時は `meta.file_stamp` で外部変更を確認。
- 文書ごとに `open_slot()`、入力が落ち着いたら `RecoveryPolicy::next_due` に従って `RecoverySlot::write(&Snapshot, ctx)` をワーカーで実行。
- 保存成功で `saved(版)`、明示的な破棄で `discard()`。

## E（メモ）
- コマンド `memo.open_panel`: 初回に `MemoPanel::create(main_hwnd, MemoStore::new(&paths), &theme, dpi, Rc<on_event>)`、以降 `toggle()`。
- `PanelEvent::Open(Memo)` → `DocumentState::memo(id)` のタブ、本文に `memo.body`、`MemoTab::opened(&memo)` を保持、タブ名 `memo.name`。`Closed` → 本文にフォーカス。`Failed` → ステータスバー。コールバック内で重い処理をしない（`PostMessageW` で委ねる）。
- 自動保存: `tab.begin_save()` → `store.save_in_background(...)` → `ui_channel` で結果。`Saved` で `tab.finish` と `record_saved`、`forked` でタブを新メモに切り替え、`resave` なら再保存、失敗で `finish_failed`。
- 正常終了時は `store.save_or_fork(id, tab.store_version(), body)` を同期で。
- `src/ui/app/mod.rs` の `TargetRef::Memo(_) => Err(unsupported)` をメモの版を返すように直す。
- メモのタブの「保存」はメモ保存として扱う。`memo.rename` は `MemoStore::rename`。

## J（巨大ファイル）
- UI が検索結果を保持する場合は `BigFile::is_invalidated()` を確認する。
- 1 GiB の計測は `measure_large_fixtures`（`docs/tasks/J.md`）。

## F（ホットキー・クリップボード）
- 起動時に `GlobalHotkey::new(main_hwnd, CLIPBOARD_HOTKEY_ID)` → `set_from_text(settings の keys["clipboard.new_tab"] または DEFAULT_CLIPBOARD_CHORD = "Ctrl+Alt+V")`。`Conflict`／`Invalid` なら `status().message()` をステータスバーへ。設定変更時に再度 `set_from_text`。
- `WM_HOTKEY`（`wParam == CLIPBOARD_HOTKEY_ID`）→ `bring_to_front(hwnd)` → `Registry::execute("clipboard.new_tab")`。ハンドラーで `clipboard::read_text(Some(hwnd))`: `Some` → `DocumentState::clipboard` の未保存タブ、`None` → 「クリップボードにテキストがありません」、`Err(Busy)` → 「クリップボードを開けません」。
- `clipboard.new_tab` の既定キーを `Ctrl+Alt+V` にそろえる。
- 実機確認: 他アプリ前面からの前面化、最小化からの復元、競合表示、`Win+` の割り当て、リッチテキスト・画像のみのとき `None`。
