# Linote アーキテクチャ（共通の取り決め）

要件の正本は [requirements.md](requirements.md)、作業分担は [PLAN.md](PLAN.md)。この文書は、並行して実装する各タスクが共有する取り決め（モジュール構成、型、スレッド、データの置き場所）を定める。型の正確な定義は `src/` のコードとドキュメントコメントにあり、食い違う場合はコードが正しい。

**取り決めを変える必要が出たら、作業を止めて報告する**（PLAN.md「失敗時の扱い」）。公開済みの型に項目を足すのは自由だが、既存の公開項目の意味や署名は変えない。

## 1. モジュール構成と依存方向

一つのクレート（`linote`）をモジュールで分ける。依存は下の図の左から右への一方向だけを許す。

```text
core ← text, file ← document ← command ← appdata, mcp
     ← memo, recovery, search, bigfile, markdown        GUI 非依存（windows crate 禁止）
     ← platform ← ui                                     Windows 専用（#[cfg(windows)]）
     ← main.rs（起動、--mcp-stdio の分岐）
```

- **GUI 非依存の中核**は `windows` crate を使わない。Linux でもテストできる（`cargo test --lib --target x86_64-unknown-linux-gnu`）。Windows 固有の処理が要る場合は `std::os::windows` の範囲にとどめ、`#[cfg(windows)]` で分ける。
- **`ui` と `platform` だけ**が `windows` crate を使う。中核から `ui`／`platform` を参照しない。
- 中核どうしの参照は必要最小限にする（例: `search` は `bigfile` の具体型ではなく `document::TextSource` を使う）。

### モジュールの責務と担当

| モジュール（担当パス） | 責務 | 担当タスク |
|---|---|---|
| `src/core/` | ID、版、実行者、エラー、中止、進捗、上限、バックグラウンド実行と UI への通路 | C0 |
| `src/document/` | 文書の種類と機能、文書の状態（版・保存済みの版）、本文へのアクセスのトレイト | C0 |
| `src/command/` | コマンドの定義・登録表・実行の入口、キー表記 | C0（登録のつなぎ込みは I1） |
| `src/appdata/` | データ保存場所のパス、設定の形式 | C0 |
| `src/mcp/` | 将来の MCP 中継口の型（初版は型だけ） | C0（実装は P1〜P3） |
| `src/text/` | 文字コードの判定・変換、BOM、改行、変換不能文字の検出 | A |
| `src/file/` | 読み込み、保存（一時ファイル経由の置換）、外部変更の検知、原子的な書き込み、ロックファイル | A |
| `src/recovery/` | 復元データの形式・書き出し・所有と排他・削除 | D |
| `src/memo/` | メモの保存・検索・一覧（版の照合つき） | E |
| `src/markdown/` | Markdown → HTML、スタイル、HTML・スクリプト・外部リソースの無効化 | G |
| `src/bigfile/` | 部分読み込み、疎な行索引、メモリ上限、無効化 | J |
| `src/search/` | 文字列・正規表現検索、置換、Grep、結果の構造化 | K |
| `src/ui/app.rs` `window.rs` `tabs.rs` `statusbar.rs` `theme.rs` `editor.rs` | アプリの状態、ウィンドウ、タブ、ステータスバー、テーマ、Scintilla の包み | B |
| `src/ui/menu.rs` `dialogs.rs` | メニュー・ショートカット、ダイアログ | I1（統括） |
| `src/ui/find.rs` | 検索・置換欄、Grep の結果 | K |
| `src/ui/memo_panel.rs` | メモの小窓 | E |
| `src/ui/preview.rs` | Markdown プレビュー（右ペイン・別ウィンドウ） | H |
| `src/ui/bigview.rs` | 巨大ファイルの表示（DirectWrite） | L |
| `src/platform/system.rs` | 明暗設定、DPI、文字列変換の補助 | B |
| `src/platform/hotkey.rs` `clipboard.rs` | グローバルショートカット、クリップボード | F |
| `src/platform/webview2_loader.rs` | WebView2 の自前ローダー | H |
| `tests/fixtures/`、生成スクリプト | テストデータ | T1 |

- 各ファイルの先頭のドキュメントコメントに、責務と使うべき共通型を書いてある。**実装を始める前に必ず読む。**
- 担当のファイルはディレクトリに変えてよい（`ui/preview.rs` → `ui/preview/mod.rs`）。`ui/mod.rs`、`platform/mod.rs`、`src/lib.rs` のモジュール宣言は C0 で済ませてあるので、ワーカーは触らない。足りないモジュールがあれば報告する。
- `Cargo.toml`、`build.rs`、`src/main.rs`、`src/lib.rs`、コマンドの登録のつなぎ込みは統括が編集する。依存の追加（`pulldown-cmark`、`grep-searcher`、`windows` の機能フラグなど）は報告で依頼する。
- 各モジュールはコマンドを `pub fn register<H: …>(registry: &mut Registry<H>)` のような登録関数として公開し、つなぎ込みは統合時に行う（§5）。

## 2. スレッドモデル

| スレッド | 数 | 役割 |
|---|---|---|
| UI スレッド | 1 | ウィンドウ、Scintilla、文書の状態（`DocumentState`）、コマンドの実行。**文書の状態を読み書きするのはここだけ。** |
| ワーカー（`core::WorkerPool`） | 0〜4 | 読み込み、保存、復元データの書き出し、検索、Grep、索引作成、Markdown 変換 |
| MCP の待ち受け（次の版） | 0〜1 | 名前付きパイプの待機（重複 I/O）。MCP が無効なら作らない |

- **UI スレッドで長い処理をしない。** ファイル入出力、検索、変換、ロック待ちはワーカーで行う。
- `WorkerPool` は自前の小さな実装（`std::thread` のみ）。**最初の処理までスレッドを 1 本も作らず**、30 秒仕事がなければスレッドは終了する。非同期ランタイムは使わない（ADR-5）。
- 入力が落ち着くのを待つ処理（自動保存、復元の書き出し、プレビューの更新）の待ち時間は、UI スレッドのタイマー（`SetTimer`）で管理する。待つためのスレッドは作らない。

### 処理の流れ

1. UI スレッドが、対象の `DocStamp`（文書 ID＋版）と入力を用意する。入力は必要な範囲の本文の写し、パス、設定など、**所有する値**だけ（Scintilla や `DocumentState` への参照は渡さない）。
2. `WorkerPool::global().spawn(…)` で処理を渡す。処理は `TaskContext`（中止・進捗・上限）を使う。
3. 処理は結果を `UiSender::send` で送る。送信は `UiWaker::wake` を呼ぶ。`ui::app` はこれを `PostMessageW`（アプリ定義のメッセージ 1 種類）で実装する。
4. UI スレッドはそのメッセージを受けたら `UiReceiver::drain` で結果をすべて取り出し、**照合してから**反映する。

`ui_channel::<M>` のメッセージ型 `M` は `ui::app` が定義する列挙型で、文書に関する結果は必ず `DocStamp` を含める。

### 照合の規則

- **表示・参照のための結果**（検索のヒット、プレビューの HTML、巨大ファイルの索引の進み、復元の書き出し完了）: `stamp.is_current(現在の版)` が偽なら**捨てる**。文書が閉じられている場合（現在の版が `None`）も捨てる。
- **保存の完了**: 文書が開いていれば、版が進んでいても `DocumentState::record_saved(保存した版, …)` で記録する。保存した版より後の変更は未保存のまま残る。文書が閉じられていれば何もしない。
- 版が進んだ・文書を閉じた時点で、UI は不要になった処理の `CancelToken` を中止する。ただし合図は間に合わないことがあるので、**中止を合図しても照合は省略しない**。

## 3. 版付き文書モデル

### 識別子と版（`core::ids`）

- `DocumentId`: 開いている文書（タブ）の ID。プロセス内で一意、再利用しない、永続しない。
- `Version`: 内容の版。**内容が変わるたびに単調に増え、戻らない。** 利用者の編集、AI の編集、Undo／Redo のどれでも進む。
- `DocStamp`: `DocumentId`＋`Version`。バックグラウンド処理の結果の照合に使う。
- `MemoId`: メモの永続的な ID（英小文字・数字・`-`、ファイル名に使える）。
- `TargetRef`: 版を持つ対象（文書またはメモ）。コマンドの対象と、版の食い違いの報告に使う。
- `Actor`: `User`（利用者）または `Ai`（将来の MCP）。

### 本文の置き場所

- **通常編集の本文は Scintilla が持つ。Rust 側は全文の写しを常時保持しない。** 保存・復元・変換・検索のために写しが要るときは、UI スレッドでその時点の `DocStamp` と一緒に写しを作り、ワーカーへ渡して、処理が終われば捨てる。
- 巨大モードの本文はファイル上にあり、`bigfile` が部分的に読む。
- 編集中の本文は**常に UTF-8**（Scintilla のコードページは UTF-8）。位置は**先頭からの UTF-8 のバイト数**（`u64`、`document::TextRange`）で、Scintilla の位置と一致する。行番号は内部 0 始まり、画面とコマンドの引数は 1 始まり。
- 本文への共通の窓口は `document::TextSource`（読み取り）と `document::EditTarget`（編集）。Scintilla の包み（`ui::editor`）、`bigfile`、テスト用の `MemoryText` が実装する。

### 版の進め方

- `ui::editor` が Scintilla の `SCN_MODIFIED`（`SC_MOD_INSERTTEXT`／`SC_MOD_DELETETEXT`）を受けるたびに `DocumentState::record_edit(actor)` を呼ぶ。`actor` は、実行中のコマンドの実行者（`CommandHost::begin_edit`〜`end_edit` の間）。コマンドの外（通常のキー入力、IME）は `Actor::User`。
- 1 回のコマンドで版が複数進んでもよい。版は「同じかどうか」だけを比べる。
- 未保存かどうかは `DocumentState::is_dirty()`（保存済みの版 ≠ 現在の版）。Undo で保存時点に戻ったとき（`SCN_SAVEPOINTREACHED`）は `record_unmodified()` を呼ぶ（版は戻さない）。
- 巨大モードは編集できないが、元ファイルの切り詰め・置換を検知したら版を進め、古い索引・検索結果が照合で捨てられるようにする。
- メモの版は保存領域に保存する（§7、タスク E）。

### 文書の種類と機能（`document`）

| 種類（`DocumentKind`） | 本文 | Edit | Undo | Save（上書き） | SaveAs | Recover | AutoSave | Highlight / Wrap / Preview |
|---|---|---|---|---|---|---|---|---|
| `File` 通常ファイル | Scintilla | ○ | ○ | ○ | ○ | ○ | – | ○ |
| `LargeFile` 巨大・読み取り専用 | ファイル | – | – | – | – | – | – | – |
| `Untitled` 新規未保存 | Scintilla | ○ | ○ | – | ○ | ○ | – | ○ |
| `Clipboard` クリップボード由来 | Scintilla | ○ | ○ | – | ○ | ○ | – | ○ |
| `Memo(MemoId)` 一時メモ | Scintilla | ○ | ○ | – | ○（書き出し） | – | ○ | ○ |

- 行ジャンプ、範囲コピー、検索は全種類で使えるので、機能として挙げない。
- 実際に使える機能は `DocumentState::capabilities()`。種類の基本の機能から、状態による制限を引く。現在の制限は一つ: **外部変更を検知している間は `Save` がない**（別名保存はできる。利用者が再読み込みするか、上書きを明示的に選ぶと `resolve_external_change` で戻る）。
- `Untitled`／`Clipboard` は、保存してパスが決まると `File` になる（`record_saved`）。「上書き保存」コマンドは `Save` がなく `SaveAs` がある文書では別名保存に進む（I1）。
- メモは保存領域への自動保存がそのまま永続化なので、復元（`Recover`）の対象にしない（ADR-12）。
- コマンドは必要な機能を `CommandDef::requires` に書く。実行可否の判定を、種類の名前で分岐して書かない。

## 4. 中止・進捗・件数上限（`core`）

- `CancelToken`: 複製が同じ合図を共有する。処理側は区切りごとに `check()?` を呼ぶ（数十ミリ秒以内に一度が目安。長大な 1 行の途中でも一定バイトごと）。
- `ProgressSink`／`Progress`: `done` と `total`（不明なら `None`）。単位は処理ごとに決めて文書化する。報告はまとまった単位で行う（1 件ごとに呼ばない）。
- `Limits`: 結果の件数とバイト数の上限。`Budget::try_add` で集計する。既定値は各モジュールが実測で決め、定数として公開する。
- `TaskContext`: 上の三つの一式。**長時間かかりうる中核の関数は `&TaskContext` を引数に取る。**
- `Outcome<T>`: 途中で終わっても意味のある結果（検索のヒットなど）は、上限・中止で打ち切ってもエラーにせず、そこまでの結果と終わり方（`Completion`）を返す。画面には打ち切ったことを必ず示す。結果に意味がない処理（保存など）の中止は `Error::Cancelled`。

## 5. コマンド（`command`）

**すべての操作は `Registry::execute` を通る。** メニュー、ショートカット、グローバルショートカット、将来の MCP は、どれも `Invocation` を作ってここへ渡す。メニューやキー入力の処理に操作の中身を直接書かない（例外は Scintilla が自分で処理する文字入力・カーソル移動・IME）。

### 定義（`CommandDef`）

静的なデータで、`const` として書く。GUI を初期化しなくても一覧でき、`describe()`／`input_schema()` で JSON（JSON Schema）にできる。

| 項目 | 内容 |
|---|---|
| `id: CommandId` | `"file.save"` のような `領域.動作` の名前。設定のキー割り当てと将来の MCP の操作名にもなる。公開後は変えない |
| `title`／`description` | メニューの表示名／説明（AI にも渡す） |
| `params: &[ParamDef]` | 引数の名前・型（文字列、0 以上の整数、真偽値、列挙）・必須かどうか・説明 |
| `result` | 結果（JSON）の形の説明 |
| `target: TargetKind` | 対象なし／文書／メモ |
| `requires: Capabilities` | 対象の文書に必要な機能（実行可能条件） |
| `effect: Effect` | `View`（画面だけ）／`Read`（読み取り）／`Edit`（内容の変更）／`Disk`（保存・削除） |
| `ai_callable` | AI から呼べるか。既定は利用者専用 |
| `single_undo` | 利用者の実行でも Undo の 1 単位にまとめるか（「すべて置換」など） |
| `default_key` | 既定のキー（`"Ctrl+S"` の表記）。設定が優先する |

名前の付け方: 領域は `file`、`edit`、`search`、`view`、`memo`、`clipboard`、`preview`、`document`、`app` を基本にする。新しい領域が要るときは報告する。

### 実行（`Invocation` → `Registry::execute`）

`Invocation` は、コマンド、実行者（`Actor`）、対象（`Option<TargetRef>`）、版の前提（`expected_version`）、引数（JSON）を持つ。`execute` は次の順に検査し、どこかで外れればハンドラーを呼ばずにエラーを返す。

1. **実行者**: AI から呼べないコマンドは `Denied`。
2. **対象の解決**: 文書が対象で指定がなければ、利用者の操作ではアクティブな文書。AI は必ず指定する。
3. **版の前提**: 指定されていれば現在の版と照合し、食い違えば `VersionMismatch`（現在の版を含む）。**AI の `Edit`／`Disk` で、文書・メモを対象にするものは前提が必須**（なければ `InvalidArgument`）。利用者の操作は UI スレッドで同期して実行するので、前提を省略できる。
4. **機能**: 対象の文書に `requires` の機能が欠けていれば `Unsupported`。
5. **引数**: 定義と照らして検査する（未知の引数、必須の欠落、型の違いは `InvalidArgument`）。
6. **実行**: 文書を対象にする `Edit` は `CommandHost::begin_edit`〜`end_edit` で囲む（失敗しても必ず閉じる）。
7. **記録**: `CommandHost::record` に `CommandRecord`（コマンド、実行者、対象、前後の版、エラーコード）を渡す。

メニューの有効・無効は `Registry::check_available`（1〜4 と同じ検査）で決める。

### Undo 単位のまとめ方

- `begin_edit(document, actor, single_undo)` で、`ui::app` は (a) その間の変更通知の実行者を `actor` として版に記録し、(b) `single_undo` が真なら Scintilla の `SCI_BEGINUNDOACTION`〜`SCI_ENDUNDOACTION` で囲む。
- **AI の編集は常に 1 単位**（`execute` が `single_undo` を真にする）。利用者は 1 回の Undo で取り消せる。
- AI の変更があった文書は `DocumentState::has_ai_edits()` が真になる。タブ・ステータスバーで示し、変更箇所の強調は `ui::editor` が行う（次の版）。

### ハンドラーと登録

- ハンドラーは `Fn(&mut H, &Call) -> Result<Value>`。`H` は `CommandHost` を実装するアプリの状態（GUI では `ui::app` の型）。中核のコマンドは、必要な操作をトレイト境界で要求する形で書けば、テスト用の状態でも動かせる（`src/command/mod.rs` のテストが例）。
- 長時間かかるコマンド（Grep、保存、巨大ファイルの検索）のハンドラーは、バックグラウンド処理を始めてすぐ戻る。結果は §2 の通路で届く。
- 登録表のつなぎ込み（どの `register` 関数を呼ぶか）は統括が管理する。二重登録と既定キーの表記の誤りは `register` がエラーにする。既定キーの重複は統合時のテストで検出する。

### キー割り当て（`command::keys`）

`KeyChord` の表記は `Ctrl+Shift+V`（修飾キーは `Ctrl`／`Alt`／`Shift`／`Win`、キーは `A`〜`Z`、`0`〜`9`、`F1`〜`F24`、`Key::NAMED` の名前）。設定ファイル、定義の既定値、グローバルショートカットで同じ表記を使う。仮想キーコードとの対応は `ui`／`platform` が持つ。

クリップボードの展開は一つのコマンド（`clipboard.new_tab`）で、その割り当てをエディタ内とグローバル（`RegisterHotKey`）の両方に使う。専用のアプリ内キーは設けない。

## 6. エラー（`core::Error`）

- モジュールの境界を越える公開関数は `core::Result<T>` を返す。`anyhow`／`thiserror` は使わない（ADR-9）。
- モジュール内部で細かいエラー型や結果型を持つのは自由（例: 変換できない文字の位置の一覧）。境界では `From` で `core::Error` に変換する。
- `Error::code()` は機械可読な安定した名前（`version_mismatch`、`unsupported`、`busy` など）。将来の MCP 応答にそのまま載せるので、値を変えない。
- `Display` は利用者に見せられる日本語の短い文にする。
- 使い分け:

| 状況 | エラー |
|---|---|
| ファイルの読み書きの失敗 | `Io { op, path, source }`（`Error::io(…)`） |
| 利用者・呼び出し側による中止 | `Cancelled`（失敗として表示しない） |
| 前提の版と現在の版の食い違い | `VersionMismatch { target, expected, actual }` |
| 文書・メモ・コマンド・ファイルがない、既に閉じた | `NotFound` |
| その文書ではできない操作 | `Unsupported` |
| 不正な引数（無効な正規表現、範囲外） | `InvalidArgument { name, reason }` |
| 入力が上限を超えた | `LimitExceeded` |
| 変換できない文字、不正なバイト列 | `Encoding` |
| 他のウィンドウ・プロセスがロック中 | `Busy` |
| 元ファイルが外部で変更された | `ExternalChange` |
| 実行者・認可による拒否 | `Denied` |
| 保存データが壊れている | `Corrupt` |
| 部品が使えない（WebView2 がない、保存場所がない、GUI 未起動） | `Unavailable` |
| 想定外の状態 | `Internal` |

- **パニックで済ませない。** リリースビルドは `panic = "abort"` なので、パニックは編集中の内容を失うことを意味する。`unwrap`／`expect` は、起こり得ないことを説明できる箇所だけに使う。`todo!()`／`unimplemented!()` は使わない。

## 7. データ保存場所と同時アクセス

### 配置（`appdata::AppPaths`）

```text
%LOCALAPPDATA%\Linote\
  settings.json            設定（§8）
  memos\                   一時メモ（中の形式はタスク E が決めてここに追記する）
  recovery\                復元データ（中の形式はタスク D が決めてここに追記する）
  mcp\authorization.json   MCP の認可（次の版）
  mcp\operations.jsonl     AI の操作記録（次の版。1 行 1 件の JSON）
```

- 場所は環境変数 `LOCALAPPDATA` から決める。環境変数 `LINOTE_DATA_DIR`（絶対パス）があれば、そちらを丸ごと使う（結合テスト・検証用）。単体テストは `AppPaths::with_root` で一時ディレクトリを指す。
- **ディレクトリもファイルも、初めて書き込む時点で作る**（`AppPaths::ensure_dir`）。起動しただけ、パスを求めただけでは何も作らない。
- 保存場所が決まらない場合（`Error::Unavailable`）も編集は続けられるようにし、メモ・復元・設定の保存ができないことを画面で知らせる。
- 配布物は EXE とライセンス表示だけで、設定ファイルは同梱しない。

### 同時アクセスの基本方針

**1 プロセスにメインウィンドウは 1 つ**で、複数のウィンドウは複数のプロセスになる（ADR-4）。次の版では MCP プロセスも加わる。したがって保存場所のデータは、常に別プロセスからの同時アクセスを前提にする。

1. **原子的な置換**: 書き込みは、同じディレクトリの一時ファイルに書いて同期し、置換する。途中で落ちても、古い内容か新しい内容のどちらかが完全な形で残る。実装は `file` モジュール（タスク A）に一つだけ置き、メモ・復元・設定・利用者のファイルの保存のすべてがそれを使う。
2. **ロック**: 読んで・照合して・書く一連の操作は、対象ごとのロックファイルを OS の排他ロックで保持して行う。ロックは**プロセスが落ちれば OS が解放する方式**にする（「ファイルが存在すればロック中」という方式は、強制終了で残るので使わない）。ロックは短時間だけ持ち、取れなければ待たずに `Error::Busy` を返す（UI スレッドで待たない。再試行はワーカーかタイマーで行う）。実装は `file` モジュール（タスク A）。
3. **版の照合**: メモの更新・名前変更・削除は前提の版を受け取り、ロックの中で現在の版と比べ、食い違えば `VersionMismatch`（現在の版つき）で拒否する。**黙って上書きしない。** 後から書いた方が勝つ方式にはしない。
4. **所有**: 復元データは、書き出したプロセスが所有する。動作中のプロセスの所有は、そのプロセスが保持し続けるロックで示す。別のプロセスは、ロックされている復元データを復元も削除もしない。ロックされていない（持ち主が落ちた）復元データだけが、次の起動で復元の対象になる。同じ復元データを 2 つのプロセスが同時に引き取らないよう、引き取りもロックで排他する（タスク D）。
5. **削除の時機**: 復元データは、元ファイルへの保存が成功したとき（保存した版以前の分）、または利用者が変更を明示的に破棄したときに削除する。保存中に編集が進んでいれば、その分の復元データは残す。
6. **外部変更**: 利用者のファイルは `file::FileStamp`（長さと更新時刻）を開いたとき・保存したときに控え、ウィンドウが前面に来たとき・保存の直前・復元のときに比べる。食い違えば `DocumentState::record_external_change()`。 比較は `file::check_external_change(path, 控え)`（変化なし／変更／削除／出現を返す）。長さと更新時刻だけを見るので、同じ長さのまま更新時刻の刻みの中で書き換えられた場合は検知できない。

### `file` モジュールの共通部品（タスク A が確定）

D・E・I1・設定の保存は、次の部品を使う。独自に一時ファイルやロックを実装しない。署名の正本は `src/file/` のドキュメントコメント。

```rust
// ---- 原子的な書き込み（メモ・復元・設定） ----
/// 同じディレクトリの一時ファイルに書き、sync_all で確定させてから置換する。
/// 失敗・中止・パニックでは一時ファイルを消し、元のファイルに触れない。
/// 親ディレクトリは作らない（先に AppPaths::ensure_dir）。進捗はバイト数。
pub fn write_atomic(path: &Path, bytes: &[u8], ctx: &TaskContext) -> Result<()>;

/// 少しずつ書く版。`write` が Err を返したら置換しない。置換後の FileStamp を返す。
pub fn write_atomic_with(
    path: &Path,
    options: AtomicOptions,            // { mode: ReplaceMode }。既定は Rename
    ctx: &TaskContext,
    write: impl FnOnce(&mut dyn io::Write) -> Result<()>,
) -> Result<FileStamp>;

pub enum ReplaceMode { Rename, PreserveMetadata }

/// この実装が作る一時ファイルの名前（`.~linote-<pid>-<時刻>-<連番>.tmp`）か。
/// ディレクトリを列挙する側（メモ・復元）は、当てはまるファイルをデータとして
/// 扱わない。強制終了で残った古いものは消してよい。
pub fn is_temp_file_name(name: &OsStr) -> bool;

// ---- ロック ----
/// OS の排他ロック（std の File::try_lock。Windows: LockFileEx、Unix: flock）。
/// 取れなければ待たずに Error::Busy。Drop で解放。プロセスが落ちれば OS が解放する。
pub struct LockFile { /* … */ }
impl LockFile {
    pub fn try_acquire(path: &Path) -> Result<LockFile>;
    pub fn path(&self) -> &Path;
    pub fn release(self);
}

// ---- 利用者のファイル ----
pub fn read_text(path: &Path, options: &ReadOptions, ctx: &TaskContext) -> Result<LoadedText>;
pub fn read_bytes(path: &Path, ctx: &TaskContext) -> Result<(Vec<u8>, FileStamp)>;
pub fn save_text(request: &SaveRequest<'_>, ctx: &TaskContext) -> Result<SaveOutcome>;

// ---- 外部変更・失敗の分類 ----
impl FileStamp { pub fn read(path: &Path) -> Result<Option<FileStamp>>; }
pub fn check_external_change(path: &Path, known: Option<FileStamp>) -> Result<ExternalChange>;
pub fn classify(error: &Error) -> Option<IoFailure>;   // Error::Io の原因
```

**置換の方法（`ReplaceMode`）と、属性・ACL の扱い**

| | Windows | Unix（テスト用） | 使う場面 |
|---|---|---|---|
| `Rename`（既定） | `MoveFileExW(MOVEFILE_REPLACE_EXISTING \| MOVEFILE_WRITE_THROUGH)`。ファイルシステムの単位で原子的。新しいファイルの属性・ACL はディレクトリの既定になる | `rename` ＋ 親ディレクトリの `fsync` | **アプリのデータ**（メモ・復元・設定）。`write_atomic` は常にこれ |
| `PreserveMetadata` | `ReplaceFileW`（属性、ACL、作成日時、代替データストリームを引き継ぐ）。保存先がなければ `Rename` と同じ。`ReplaceFileW` が使えないファイルシステム（FAT、一部の共有）では `Rename` の置換に切り替える | 許可ビットを写してから `rename`（所有者は引き継がない） | **利用者のファイル**（`save_text`） |

- `ReplaceFileW` は内部で「保存先を別名に退避 → 一時ファイルを保存先の名前に変更 → 退避を削除」と進むので、厳密には一瞬だけ保存先の名前が存在しない。その間にプロセスが落ちても内容は一時ファイルか退避（`~RF….TMP` の類）に残るが、保存先の名前には何もない状態がありうる。利用者のファイルでは ACL と属性を失わないことを優先してこちらを選んだ。アプリのデータは `Rename` なので、この隙間はない。
- `ReplaceFileW` を呼ぶ前に、保存先を削除アクセスで開けるか試す。他のプログラムが共有を許さずに開いている場合などは、ここで失敗させて何も動かさない。
- 中核は `windows` crate を使わない取り決めなので、`MoveFileExW`／`ReplaceFileW` は `src/file/sys.rs` で `kernel32` の関数を直接宣言して呼ぶ（std の `fs::rename` は `MOVEFILE_WRITE_THROUGH` を付けず、属性の引き継ぎもできないため）。長いパスは `\\?\` 形式に直して渡す。
- **読み取り専用の属性**が付いたファイルは上書きしない（`IoFailure::ReadOnly`）。属性を外して書くことはしない。
- **シンボリックリンク**はリンク先を保存先にする（リンクを通常のファイルで置き換えない）。**ハードリンク**は切れる（新しいファイルで置き換える方式のため）。
- 失敗は `Error::Io`（`op` が段階: 「保存先の確認」「一時ファイルの作成」「一時ファイルの書き込み」「一時ファイルの書き込みの確定」「ファイルの置換」）。原因は `file::classify` で `NotFound`／`PermissionDenied`／`ReadOnly`／`InUse`／`DiskFull`／`InvalidPath`／`IsDirectory`／`Other` に分ける。

**ロックファイルの使い方**

- ロックは**データとは別の専用ファイル**に掛ける（例: `memos/.lock`、`recovery/<id>.lock`）。Windows ではロックした範囲を他のハンドルから読み書きできなくなるため、データのファイルそのものには掛けない。
- ロックファイルは消さない（存在はロック中を意味しない。消すと、同時に取りに来た相手と別々のファイルをロックする競合が起きる）。
- 同じプロセスの中でも、同じパスを 2 回取ることはできない（2 回目は `Busy`）。
- 復元データの所有（上の 4）は、`LockFile` を保持し続けることで示す。孤児の引き取りは `try_acquire` が成功したこと自体を「引き取った」とする。

### 復元データの形式（タスク D が確定）

`recovery/` の中（実装は `src/recovery/`）:

```text
recovery\
  <id>.rec          復元データ 1 件。<id> = <プロセス ID>-<時刻ms>-<連番>（16 進、英小文字・数字・`-`）
  <id>.lock         所有のロックファイル。所有プロセスが文書を閉じるまで排他ロックを保持
  quarantine\       壊れた・未知の版の隔離先（<id>.corrupt.rec / <id>.unknown-version.rec）。削除しない
```

- **`.rec` の中身**: 1 行目がコンパクトな JSON のヘッダー、改行、続けて本文（UTF-8 の生バイト）。ヘッダーは `format`（`linote-recovery`）、`version`（形式の版。現在 1）、`id`、`kind`（`file`／`untitled`／`clipboard`／`ai`）、`source_path`（なければ null。UTF-8 に変換できないパスは置換文字になる）、`encoding`／`bom`／`eol`（`TextFormat`）、`file_stamp`（`{len, modified_ns}` または null。復元時に `file::check_external_change` へ渡す）、`title`、`doc_version`、`created_ms`／`updated_ms`、`owner_pid`（参考情報）、`body_len`、`body_fnv`（本文の FNV-1a 64 ビット）。1 ファイルなので原子的な置換だけで整合する。長さと検査値が合わないものは壊れているとみなす。
- **所有**: `RecoveryStore::open_slot()` が新しい `<id>` を作り、`.lock` を `file::LockFile` で取る（文書ごとに 1 つ、閉じるまで保持）。各プロセスは自分の `<id>` にしか書かないので、同じ文書を複数ウィンドウで開いても上書きし合わない。プロセスが落ちれば OS がロックを解放する。
- **孤児の引き取り**: `RecoveryStore::scan()` は `.rec` ごとに `.lock` を `try_acquire` し、取れたもの（持ち主が落ちた）を `Candidate` として返す。`Candidate` はロックを保持するので他のプロセスは引き取れない。取れない（`Busy`）ものは `live` に載せるだけで触らない。`Candidate::read_text()` で本文、`meta`（`file_stamp` を含む）で付随情報を得る。復元しても元ファイルには書かない。`into_slot()` で復元した文書が `<id>` を引き継ぎ、`discard()` で削除する。
- **隔離**: 引き取れたもののうち、壊れている・未知の版のものは `quarantine/` へ移して `ScanReport::quarantined` で報告する（削除しない）。生きているプロセスのものは、読めなくても触らない。
- **書き出し**（`RecoverySlot::write`、ワーカーで呼ぶ）: `file::write_atomic` で `.rec` を置換する。UI が `Snapshot`（本文の写し・版）を渡し、完了の通知は呼び出し側が `DocStamp` で行う。書き出した版より古い版、保存済みの版以下は `Stale` で捨てる。Windows では他のプロセスが走査で読んでいる最中の置換が拒否されうるので、拒否・使用中なら数回やり直す。
- **削除**: `RecoverySlot::saved(版)` は保存した版以前の分だけを削除する（保存中に進んだ分は残す）。`discard()` は `.rec` と `.lock` を削除して所有を手放す。`.lock` は `<id>` が一意なので、`discard`／隔離のときに限り削除してよい。
- **掃除**: 走査は、1 時間以上前の一時ファイル（`is_temp_file_name`）と、データのない 1 時間以上前のロックファイルを削除する。作られた直後のロックファイルは、作成とロック取得の間に消すと持ち主が消えたファイルをロックしてしまうので触らない。
- **書き出しの間隔**: `RecoveryPolicy` の既定は、最後の編集から 2 秒（入力が落ち着いたとき）、ただし未書き出しの編集が 30 秒を超えて溜まらないようにする。設定項目にするかは統括の判断。
- 別プロセス（将来の MCP）も同じ `scan()` を呼んでよい。候補を捨てれば（Drop）ロックは解放され、孤児のまま残る。

### 読み込みの流れ（通常ファイル）

`file::read_text` が、読み込み・判定・変換・改行の集計をまとめて行う（ワーカーで呼ぶ）。

1. `read_bytes`: 読み始める前の `FileStamp` を控えてから全体を読む。`ctx.limits.max_bytes` を超えるなら読まずに `LimitExceeded`（巨大モードへの振り分けに使える）。
2. `text::detect`: BOM → UTF-16 らしさ（NUL を含む場合）→ UTF-8 として妥当か → CP932。根拠（`DetectReason`）と信頼度（`Confidence`）を返す。`ReadOptions::with_encoding` で文字コードを指定して開き直せる。
3. `text::decode`: UTF-8 にする。**不正なバイトは U+FFFD に置き換え、個数と位置を `LoadedText::invalid` で返す**（エラーにしない）。`LoadedText::is_lossy()` が真なら、UI は「この文字コードでは正しく読めていない。保存すると元に戻せない」ことを知らせ、上書き保存の前に確認する。
4. 改行は変換しない。`EolStats`（CRLF・LF・CR の数）を返し、最も多い形式を `TextFormat::eol` にする（改行がなければ `ReadOptions::default_eol`）。混在は `EolStats::is_mixed()`。
5. `LoadedText::reencodes_exactly` は「編集せずに保存すると元のバイト列と一致するか」。偽になるのは、不正なバイトがあった場合と、CP932 の重複文字（下記）を含む場合。

**CP932 の扱い**（`encoding_rs::SHIFT_JIS` を使う。詳細は `src/text/encode.rs`）

- NEC 特殊文字（①、㈱ など）、IBM 拡張、NEC 選定 IBM 拡張、半角カナは Windows の CP932 と同じ対応で読み書きできる。
- 外字（0xF040〜0xF9FC ↔ U+E000〜U+E757）は、`encoding_rs` が読めるが書けないので、自前で書いて往復させる。
- 重複して登録された文字（NEC 選定 IBM 拡張 0xED40〜0xEEFC など、2 バイトの組み合わせのうち 398 通り）は、保存すると代表のバイト列（IBM 拡張 0xFA40〜 など）に寄る。文字は変わらないがバイト列は変わるので、`reencodes_exactly` で分かるようにしている。
- U+00A5（¥）、U+203E（‾）、U+2212（−）は、`encoding_rs` が 0x5C、0x7E、0x817C に書くが、読み直すと別の文字（\、~、－）になる。黙って化けさせないため、**変換できない文字として報告する**。

### 保存の流れ（通常ファイル）

1. UI: コマンド `file.save` が機能（`Save`）を検査し、`file::check_external_change` で外部変更を確かめ、本文の写しと `DocStamp` を作る。同じ文書の保存が進行中なら、新しい保存は始めない。
2. ワーカー: `file::save_text(SaveRequest { path, text, format, doc, expect, on_unmappable })`。
   - `expect`（`DiskExpectation::Unchanged(控え)`／`Absent`／`Any`）を確かめる。食い違えば何も書かずに `Error::ExternalChange`。
   - 同じディレクトリの一時ファイルへ、文字コードに変換しながら書く（BOM と UTF-16 のバイト順は明示的に処理する。改行は変換しない）。
   - 変換できない文字があれば、一時ファイルを捨てて `SaveOutcome::Unmappable { chars }`（文字・バイト位置・行・列の一覧。既定で 1000 件まで）を返す。**元ファイルには触れていない。** UI が確認し、利用者が承知したら `on_unmappable: OnUnmappable::Replace`（`?` に置換）で呼び直す。
   - 置換の直前にもう一度 `expect` を確かめ、`ReplaceMode::PreserveMetadata` で置換する。`SaveOutcome::Saved { doc, disk, bytes_written }` を返す。
3. UI: 文書が開いていれば `record_saved(保存した版, パス, 新しい FileStamp)`。対応する復元データ（保存した版以前）を削除する。失敗したら表示し（原因は `file::classify`）、元ファイルと編集中の内容はそのまま残す。
4. 外部変更を知ったうえで上書きする・別名で保存する場合は、UI が利用者に確認してから `DiskExpectation::Any` を渡す。

AI による保存（次の版）も同じコマンドを通る。巨大モードと、外部変更を検知した文書は `Save` の機能がないので拒否される。

## 8. 設定（`appdata::settings`）

- 形式は **JSON**（`settings.json`、UTF-8）。`serde_json` の `Value` を手で読み書きし、`serde` の derive は使わない（ADR-1、ADR-10）。

```json
{
  "version": 1,
  "theme": "system",
  "keys": { "clipboard.new_tab": "Ctrl+Alt+V", "file.save": "" },
  "mcp": { "enabled": false }
}
```

- `keys` はコマンド名 → キー表記。ここにないコマンドは定義の既定値を使う。空文字列は「割り当てなし」。実際のキーは `Settings::key_for(def)` で決める。
- **知らない項目は保持して書き戻す**（新しい版が書いた設定を古い版が消さない）。設定項目を足すタスクは `Settings` に項目と読み書きを足す（統括に報告）。
- ファイルがなければ既定値で動く。ファイルは設定を変更して保存するときに初めて作る。
- 読めない場合（`Error::Corrupt`）は既定値で起動して知らせ、**利用者が設定を変更するまでファイルを上書きしない**。
- 保存は「ロック → 読み直す → 変更する項目だけ差し替える → 原子的に置換」。別ウィンドウの変更を失わないため。
- 起動時に読むのはこのファイル 1 つだけ（小さな JSON の解析）。

## 9. 遅延初期化

通常の起動で行うのは、ウィンドウの作成、Scintilla の登録、設定ファイルの読み込み、コマンドの登録（静的な定義と関数の登録だけ）、グローバルショートカットの登録、引数で指定されたファイルを開くことまで。次のものは**初めて使う操作まで何もしない**。

| 対象 | 初期化の時機 | 起動時にしてはいけないこと |
|---|---|---|
| WebView2 | プレビューを初めて開く操作 | レジストリでのランタイムの確認、DLL の読み込み、環境の生成、関連プロセスの起動 |
| MCP（次の版） | 設定で有効にした時点（有効が保存されていれば起動後） | 無効のときのパイプ作成、スレッド起動、認可ファイルの読み込み、JSON-RPC の処理 |
| メモ一覧 | 小窓を初めて開く操作 | `memos\` の走査・読み込み、ディレクトリの作成 |
| Grep・検索 | 初めて検索を実行する操作 | 正規表現の構築、検索用の画面の作成 |
| ワーカースレッド | 最初のバックグラウンド処理 | スレッドの事前起動 |
| 復元データ | 起動直後に `recovery\` があるかだけを確かめ、なければ何もしない。あれば走査はワーカーで行う | UI スレッドでの読み込み、ディレクトリの作成 |
| 巨大ファイルの索引 | 巨大ファイルを開いたとき（先頭の表示は索引を待たない） | – |

遅延初期化する部品は、`ui::app` が `Option<…>`（または `OnceCell`）で持ち、最初の利用で作る。グローバルな静的初期化に重い処理を置かない。

## 10. WebView2

- **`WebView2Loader.dll` を使わない。** GNU ターゲットでは `webview2-com-sys` がこの DLL への依存を持ち込むので、`webview2-com-sys` のローダー関数（`CreateCoreWebView2EnvironmentWithOptions` など）は呼ばない。単体 EXE 検査（`scripts/check-imports.sh`）で検出される。
- COM のインターフェースの型定義（`webview2-com` の `Microsoft::Web::WebView2::Win32`）は使ってよい。ただし、その crate を足すことで EXE に DLL の依存が増えないことを `check-imports.sh` で確かめる。増える場合は、必要なインターフェースだけを `platform::webview2_loader` に自前で定義する。
- 代わりに `platform::webview2_loader`（タスク H）に自前のローダーを置く:
  1. インストール済みランタイムをレジストリで探す。EdgeUpdate の Clients キー `{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}` の `pv`（版）と `location` を、HKLM の `SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients`、HKLM の `SOFTWARE\Microsoft\EdgeUpdate\Clients`、HKCU の同じキーの順に調べる。
  2. `<runtime>\EBWebView\x64\EmbeddedBrowserWebView.dll` を `LoadLibraryW` で読み込む。
  3. その DLL の `CreateWebViewEnvironmentWithOptionsInternal` を `GetProcAddress` で取り、呼ぶ。
- ランタイムが見つからなければ `Error::Unavailable` を返し、プレビューを開いた時点で案内する。通常編集は続けられる。
- プレビューを初めて開く操作まで、上の 1〜3 を含め一切実行しない。
- ユーザーデータフォルダーは `%LOCALAPPDATA%\Linote` の下に置く（場所はタスク H が決めてここに追記する）。
- スクリプトと外部リソースの読み込みは無効にする。HTML 側（`markdown`）の Content-Security-Policy と、WebView2 側の設定・ナビゲーションの拒否の両方で防ぐ（R3 でレビュー）。

## 11. 将来の MCP（要件 C）

初版は**構成だけ**を備え、通信・認可は実装しない。

- 型の置き場所は `src/mcp/`。現在あるのは中継口の型だけ: `RelayRequest`（コマンドの一覧／呼び出し）、`CommandCall`（外部から届いた呼び出し。`into_invocation` で `Actor::Ai` の `Invocation` にする）、`RelayResponse`、`RelayError`（`Error::code` と、版の食い違いなら最新の版）。
- 次の版で足すもの（P1〜P3）: `--mcp-stdio` の入口（`main.rs` で GUI を初期化する前に分岐）、JSON-RPC の処理、名前付きパイプの中継、認可、操作ごとの確認、操作記録。
- 初版の設計で備えていること:
  - 全操作が版の照合つきのコマンドに一本化されている（§5）。AI の書き込みは前提の版が必須で、登録表が検査する。
  - コマンドの実行者を記録できる（`Actor`、`CommandRecord`、`DocumentState::last_actor`）。
  - コマンドの定義が機械可読（`CommandDef::describe`）。AI に必要な許可範囲（`ai_scope`: 読み取り／書き込み）と、操作ごとの確認の要否（`needs_confirmation_from_ai`: `Effect::Disk`）を定義から求められる。
  - 中核処理（`text`、`file`、`memo`、`recovery`、`search`、`bigfile`）が GUI から切り離されており、MCP プロセスからも呼べる。
  - メモと復元データは別プロセスからの同時アクセスで壊れない（§7）。
  - 長時間処理は中止・進捗・上限を共通の仕組みで扱う（§4）。
- 守ること: **MCP の処理を通常の GUI 起動経路から呼ばない。** `mcp` モジュールを `ui` から参照するのは、MCP を有効にした後の経路だけにする。検索結果など AI に返しうる結果は、JSON にできる構造体にしておく。

## 12. 依存 crate

| crate | 用途 | 状態 |
|---|---|---|
| `windows` | Win32（`ui`、`platform` のみ） | 既存 |
| `encoding_rs` | 文字コード変換 | 既存 |
| `regex` | 正規表現 | 既存 |
| `cc`（ビルド時） | Scintilla・Lexilla のコンパイル | 既存 |
| `serde_json` | コマンドの引数・結果、設定、メモ・復元のメタデータ、将来の MCP | **C0 で追加** |
| `pulldown-cmark`、`grep-searcher` など | Markdown、Grep | 各タスクが必要になった時点で依頼 |

`serde_json` を足した理由: コマンドの引数と結果を機械可読にし（要件 C）、将来の MCP（JSON-RPC）にそのまま載せるには JSON の値の型が要る。設定・メモ・復元のメタデータにも同じものを使えば、日本語やパスのエスケープを手書きの形式で誤る危険がない（データを失わないことを優先）。純粋な Rust で DLL の依存を増やさず、追加で入るのは `serde_core`、`itoa`、`memchr`、`zmij` だけ。`serde` の derive（proc-macro）は使わないので、ビルド時間への影響は小さい。追加後もリリースビルドの `check-imports.sh` が通ることを確認済み。

依存を足すときの条件: 純粋な Rust であること（別 DLL を要しない）、`check-imports.sh` が通ること、起動経路で重い初期化をしないこと。

## 13. 設計判断の記録

### ADR-1: JSON の値の型として `serde_json` を採用し、derive は使わない

- 状況: コマンドの引数・結果、設定、将来の MCP で、機械可読な値の表現が要る。
- 判断: `serde_json::Value` を共通の値の型にする（`command::Value`）。構造体との相互変換は手で書く。
- 理由: 独自形式は、エスケープの誤りがデータの破損につながる。derive は proc-macro（`syn`）でビルドが重くなり、初版の構造体の数では割に合わない。
- 見直す条件: 手書きの変換が増えて誤りの元になったら derive を検討する。

### ADR-2: コマンドは文字列の名前と登録表で表す

- 判断: `CommandId` は静的な文字列。全コマンドを列挙した `enum` は作らない。登録表はアプリの状態の型 `H` で総称化する。
- 理由: 並行して実装するタスクが共有の `enum` を奪い合わない。名前がそのまま設定のキーと MCP の操作名になる。定義（静的データ）と処理を分けたので、GUI なしで定義を一覧できる。
- 代償: 名前の打ち間違いはコンパイルで分からない。各モジュールが `CommandDef` を `const` で公開し、呼ぶ側はその定数の `id` を使うことで避ける。

### ADR-3: 版は変更のたびに進め、Undo でも戻さない

- 判断: `Version` は単調増加。未保存かどうかは保存済みの版との比較で決める。
- 理由: 「同じ版なら同じ内容」が常に成り立ち、バックグラウンドの結果や AI の書き込みの照合が単純で安全になる。内容のハッシュは 100 MiB 級で重い。

### ADR-4: 1 プロセス 1 メインウィンドウ

- 判断: 複数のウィンドウは複数のプロセスで実現する。プロセス間で文書の状態は共有しない。
- 理由: 要件の「複数ウィンドウから扱っても上書きされない」と、次の版の MCP プロセスは、どちらもファイル上のロック・原子的置換・版の照合で解決する必要がある。同じ仕組みに一本化すれば、プロセス内の共有状態と排他を別に設計しなくて済む。一つのプロセスが落ちても他のウィンドウに影響しない。
- 代償: グローバルショートカットは最初に登録したプロセスが持つ（他は競合として表示する）。プレビューの別ウィンドウは同じプロセスの子ウィンドウで、これには当たらない。

### ADR-5: バックグラウンド実行は自前の小さなスレッドの集まり

- 判断: `std::thread` と `std::sync` だけで `WorkerPool` と UI への通路を実装する。`tokio`、`rayon` などは使わない。
- 理由: 起動時間・常駐メモリ・EXE の大きさ。必要なのは「数本のスレッドで処理し、結果を UI に投げる」ことだけ。
- 見直す条件: Grep の並列化で不足したら、その部分だけで検討する。

### ADR-6: 本文は Scintilla に置き、Rust 側は必要なときだけ写しを作る

- 判断: 常時の全文コピーを持たない（要件）。保存・復元・変換の入力は、UI スレッドで作る一時的な写しとする。
- 代償: 100 MiB 級では写しの作成に時間とメモリがかかる（一時的に本文と同じ大きさ）。復元の書き出し間隔と合わせて実測で調整する。

### ADR-7: 位置は UTF-8 のバイト位置

- 判断: 本文の位置は UTF-8 のバイト数（`u64`）。文字数や UTF-16 の位置は境界（画面の列表示、Win32 の API）でだけ変換する。
- 理由: Scintilla の位置、`regex` の一致位置、ファイル上の位置（巨大モード）と一致し、変換の誤りと計算量を避けられる。

### ADR-8: WebView2 は自前のローダーで読み込む

- 判断: §10 のとおり。`WebView2Loader.dll` も、それに依存する crate のローダー関数も使わない。
- 理由: 単体 EXE（要件 A）。ランタイム本体は Windows 11 の標準部品として前提にできる。
- 未確定: 事前検証 S2 の結果で細部（レジストリの値の名前、関数の署名）を確定し、§10 に追記する。

### ADR-9: エラーは一つの列挙型

- 判断: `core::Error` を共通の型にし、`anyhow`／`thiserror` は使わない。
- 理由: 画面での表示と将来の MCP 応答で、エラーの種類（特に版の食い違いと最新の版）を型で区別する必要がある。依存も増やさない。

### ADR-10: 設定は JSON、未知の項目は保持

- 判断: §8 のとおり。
- 理由: 値の型は ADR-1 で既にある。利用者が手で編集でき、版が混在しても設定を失わない。

### ADR-11: 外部変更は「上書き保存の機能を外す」ことで表す

- 判断: 外部変更を検知した文書は `Save` の機能を失い、利用者が解消するまで上書き保存できない。
- 理由: 利用者の操作と AI の操作の両方を、コマンドの共通の検査だけで止められる。個々のコマンドに判定を書かなくて済む。

### ADR-12: メモは復元の対象にしない

- 判断: メモは `Recover` を持たず、`AutoSave` を持つ。
- 理由: メモは入力が落ち着いた時点で保存領域に自動保存されるので、それ自体が強制終了への備えになる。復元データと二重に持つと、どちらが新しいかの照合が要る。

### ADR-13: 利用者のファイルの上書きは `ReplaceFileW`、中核での kernel32 直接宣言を `file/sys.rs` に限り認める

- 利用者のファイル（`save_text`）は ACL・属性・作成日時・代替データストリームを失わないことを優先し、`ReplaceFileW`（`AtomicOptions::PreserveMetadata`）で置換する。置換の途中で一瞬保存先の名前がなくなる可能性はあるが、失敗時は元ファイルが残る。メモ・復元・設定は `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`（`Rename`）。
- `std` では `WRITE_THROUGH` と属性の引き継ぎができないため、GUI 非依存の中核のうち `src/file/sys.rs` に限り、`windows` crate を使わずに kernel32 の関数を `extern "system"` で直接宣言してよい。

### ADR-14: `FileStamp` はファイル ID を持たない（既知の制限）

外部変更の検知は長さと更新時刻で行う。同じ長さのまま更新時刻の刻みの中で書き換えられた場合は検知できない。必要になったら `FileStamp` に `id: Option<…>` を追加する（既存の構造体リテラルの修正を伴う）。R1 で再検討する。
