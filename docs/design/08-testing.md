# 08. テスト戦略

## 結論

- 正しさの中核（スキャナ、型変換、Router、チェックポイント）はユニットテストとプロパティテストで固め、`csv` クレートをスキャナのオラクルにします
- 再開の正しさは「強制中断 → 再開」の出力が一気通し実行とバイト一致することで判定します（NF-04 の決定性が根拠）
- 性能テストは CI では走らせず、生成スクリプトとローカル手順として用意します

## 階層

| 階層 | 場所 | 対象 | 実行 |
| --- | --- | --- | --- |
| ユニット | 各モジュールの `#[cfg(test)]` | スキャナの状態遷移、型変換の境界、Router の判定、サニタイズ、設定ハッシュ | `cargo test` |
| プロパティ | `tests/prop_*.rs`（proptest） | スキャナ vs `csv` クレート、Router vs 線形探索、decimal の往復 | `cargo test` |
| フィクスチャ | `tests/fixtures/*.csv` + `tests/fixtures_*.rs` | 要件書 NF-10 の列挙ケース | `cargo test` |
| CLI 結合 | `tests/cli_*.rs`（assert_cmd） | サブコマンド、終了コード、JSON 出力、`--resume` の拒否 | `cargo test` |
| 再開 | `tests/resume.rs` | キャンセル再開、SIGKILL 再開（Linux のみ）、バイト一致 | `cargo test` |
| 性能 | `scripts/gen_csv.py`、`scripts/bench.sh` | GB 級の入力でのスループット | 手動 |

## ユニットテスト

### scanner

- 各状態遷移を1ケースずつ（引用符内の区切り・改行、`""` エスケープ、閉じ引用符後の不正バイト、LF 終端、CRLF 終端、終端なしの EOF）
- バッファ境界をまたぐケース: 同じ入力を 1〜入力長の全てのチャンクサイズで供給し、結果が同一であることを確認する。`CsvSource` の `copy_within` と容量拡張の経路がここで通る
- `max_record_bytes` 超過: 引用符を閉じ忘れた入力で `RecordTooLarge { unclosed_quote: true }`、単に長い行で `unclosed_quote: false`

### key

- int: `0`、`-0`、`+1`（拒否）、`1,000`（拒否）、`１２３`（拒否）、`9223372036854775807`、`9223372036854775808`（拒否）、` 12 `（trim で受理、`--no-trim` で拒否）
- decimal: scale 2 で `1`、`1.5`、`1.50`、`1.500`（拒否）、`-0.01`、`.5`（拒否）、`1.`（拒否）
- date / datetime: 書式一致、末尾の余分な文字（拒否）、オフセット有無と `default_tz`、`None` のときの拒否、IANA 名と固定オフセットで同じ瞬間になること
- string: Shift_JIS と UTF-8 で同じ文字列が同じ `KeyValue` になること、コードポイント順の比較
- デコード失敗（不正な Shift_JIS バイト列、不正な UTF-8）が `BadEncoding` になること

### range

- `Router::new` の各 `ConfigError`（重複、空区間、型不一致、名前衝突、予約名）
- `route` の境界: `lo` ちょうどは含む、`hi` ちょうどは含まない、隙間、両端の `Unbounded`
- `from_bounds` の名前生成と、各型の正規形
- サニタイズ: 禁止文字、Windows 予約語（`con`、`Con.csv`）、末尾のドット、大文字小文字違いの衝突

### checkpoint

- 設定ハッシュが「含める項目」の変化で変わり、「含めない項目」（バッファサイズなど）の変化で変わらないこと
- JSON の往復（serialize → deserialize）
- `schema_version` 不一致、`tool_version` 不一致、各 `InputMismatch`、`OutputTruncated`、`OutputMissing` の拒否

## プロパティテスト

### スキャナ vs `csv` クレート

任意のフィールド値（引用符、区切り、改行、Shift_JIS の 0x5C を含むバイト列）から CSV を生成し、自前スキャナのレコード数・各レコードのフィールド数・キー列の unquote 結果が、`csv::ReaderBuilder` を `flexible(true)` で読んだ結果と一致することを確認します。CR 単独の終端は `csv` クレートと挙動が異なるため、生成する値から単独 CR を除きます。

### Router vs 線形探索

任意の区間集合（重複なしに整形）と任意のキー値に対し、`route` の結果が全区間を順に `lo <= key < hi` で調べた結果と一致すること。

### decimal の往復

任意の `i128` と `scale` から正規形の文字列を作り、`parse` して同じ値に戻ること。

## フィクスチャ（NF-10）

`tests/fixtures/` に小さな CSV を置き、期待される出力ファイル群を `tests/fixtures/expected/<case>/` に置いてバイト比較します。

| ファイル | 内容 |
| --- | --- |
| `boundary.csv` | 境界値ちょうどの行（`lo` と `hi` の両方） |
| `quoted_newline.csv` | 引用符内の改行、`""` エスケープ、引用符内の区切り |
| `bom_utf8.csv` | BOM 付き UTF-8。出力にも BOM が付くこと |
| `sjis_0x5c.csv` | Shift_JIS で「表」「能」「ソ」を含む値とヘッダ。キー列が日本語列名 |
| `whitespace_key.csv` | 前後に空白・タブのあるキー値 |
| `missing_key.csv` | 列数が足りない行、空のキー、型変換できないキー |
| `crlf.csv` | CRLF 終端。出力が CRLF のまま保持されること |
| `no_trailing_newline.csv` | 最後の行に改行がない |
| `empty_range.csv` | 1行も該当しない区間があり、そのファイルがヘッダのみで生成されること |
| `header_only.csv` | ヘッダだけ。全出力がヘッダのみ |

各フィクスチャについて split（両モード）と plan を走らせ、plan の件数が split の件数と一致すること（FR-66）も同じテストで確認します。

## CLI 結合テスト

`assert_cmd` でバイナリを起動し、次を確認します。

- 各終了コード（1〜5）が対応するエラーで返ること。5 は `tests/resume.rs` で扱う
- `inspect` の3段階が引数で切り替わること、全件読み込みの告知が標準エラー出力に出ること、`--quiet` で消えること
- `--json` の出力が標準出力のみで、`serde_json` でパースでき `schema_version` を含むこと
- `--resume` と `-` の併用、`--resume` と `--overwrite` の併用、区間指定のみ、`--no-header` と列名の各エラー
- 既存出力がある状態で `--resume` も `--overwrite` もない場合の終了コード 1 と、チェックポイントだけがある場合の案内文

## 再開テスト

### ライブラリレベル（決定的）

1. 生成した中規模の CSV（数十 MB、`checkpoint_interval_bytes` を 1 MiB に設定）で `split` を一気通しで実行し、出力ディレクトリ A を得る
2. 同じ入力で、進捗コールバック内で N 回目に `cancel()` を呼ぶ `split` を実行し、`Error::Cancelled { checkpoint_written: true }` を確認する
3. `ExistingOutput::Resume` で再実行し、`Report.resumed_from` が `Some` であること、出力ディレクトリ B の全ファイルが A とバイト一致すること、チェックポイントファイルが消えていることを確認する
4. N を複数の値（初回チェックポイント直後、中盤、最終レコード直前）で繰り返す

### プロセスレベル（Linux のみ）

子プロセスとして `ksplit split` を起動し、一定時間後に `SIGKILL` を送って再開します。fsync 済みの出力とチェックポイントの整合性（rename 前後のどちらで落ちても再開できること）を確認します。タイミング依存のため、繰り返し回数を設定可能にし、CI では少ない回数で走らせます。

### 再開の拒否

チェックポイントを書いた後に、入力ファイルの mtime を変える、サイズを変える、設定（区間）を変える、出力ファイルを記録長未満に truncate する、の各操作をしてから `--resume` し、終了コード 4 と対応するメッセージを確認します。

## 性能テスト（NF-03）

`scripts/gen_csv.py` は列数、行数、キー列の型と分布、引用符付きフィールドの割合、エンコーディングを引数に取り、指定サイズの CSV を生成します。`scripts/bench.sh` は 1 GiB と 10 GiB の入力で `split` を実行し、スループットを表示します。

目標はローカル SSD で 200 MB/s 以上です。下回る場合に疑う順序は、スキャナの `memchr` が効いているか（引用符付きフィールドが多いと `Quoted` 状態の `memchr` 1 バイト探索が増える）、書き込みバッファのサイズ、`persist` の頻度です。`perf` や `cargo flamegraph` で確認します。

## CI

GitHub Actions で Linux と macOS の `cargo test`、`cargo clippy -- -D warnings`、`cargo fmt --check` を走らせます。Windows はビルド（`cargo build`）のみで、テストは走らせません（NF-06）。性能テストは含めません。
