# ksplit 設計書

対象: `docs/requirements.md`（ksplit 要件定義書、Draft）に基づく初期リリースの設計です。
要件書で未決としている項目のうち、実装に必要なものはここで決め、該当箇所に「設計判断」として理由を添えています。要件書と食い違う箇所は「要件書からの差分」として明示します。

## 構成

| ファイル | 内容 |
| --- | --- |
| [01-architecture.md](01-architecture.md) | クレート構成、モジュール構成、依存クレート、処理全体のデータフロー、エラーと終了コードの対応 |
| [02-csv-reader.md](02-csv-reader.md) | バイト単位の CSV レコード読み込み（`RecordSource` / `RecordScanner`）、BOM、レコード長上限、オフセット管理、seek |
| [03-key-and-router.md](03-key-and-router.md) | キー列の抽出・デコード・型変換、`KeyValue` の順序、`Router`（区間判定）、区間の検証、出力名の生成とサニタイズ |
| [04-pipeline-and-sinks.md](04-pipeline-and-sinks.md) | 単一パスのパイプライン、`Sink` の差し替え（plan / split / profile）、出力ファイル管理、進捗、キャンセル、invalid ガード |
| [05-checkpoint.md](05-checkpoint.md) | チェックポイントファイルの形式、設定ハッシュ、永続化手順（flush / fsync / rename）、再開手順と拒否条件、`status` |
| [06-analysis.md](06-analysis.md) | inspect（下見と型候補）、profile（正確な統計、リザーバサンプリング、ヒストグラム、境界値提案） |
| [07-cli.md](07-cli.md) | CLI の構造、引数からライブラリ設定への変換、`inspect` の3段階、SIGINT、進捗表示、JSON 出力 |
| [08-testing.md](08-testing.md) | テスト戦略（ユニット、フィクスチャ、プロパティ、再開、性能）、CI |

## 設計の前提

- 実装言語は Rust（edition 2021、安定版ツールチェイン）。非同期ランタイムには依存しません（NF-07、ライブラリ API の方針）
- ライブラリが満たすべき契約は要件書「ライブラリ公開 API 案」の形を基準とし、差分はこの設計書で明示します
- 処理の正しさを決める中核（レコード境界の検出、キーの型変換、区間判定、チェックポイント）は外部クレートに委ねず自前で持ち、デコードや日時解析のような周辺は実績のあるクレートを使います

## 要件とのトレーサビリティ

| 要件 ID | 主な設計箇所 |
| --- | --- |
| FR-01〜06（入力） | 02-csv-reader |
| FR-10〜17（キー） | 03-key-and-router「キーの抽出と型変換」 |
| FR-20〜25（範囲） | 03-key-and-router「区間と Router」 |
| FR-30〜36（出力） | 04-pipeline-and-sinks「FileSink」、03-key-and-router「出力名」 |
| FR-40〜45（モード・不正行） | 04-pipeline-and-sinks「RouterConsumer」「invalid ガード」 |
| FR-50〜52（レポート） | 04-pipeline-and-sinks「Report」、07-cli「進捗表示」「JSON 出力」 |
| FR-60〜67（解析・補助） | 06-analysis、05-checkpoint「status」 |
| FR-70〜73（実行制御） | 04-pipeline-and-sinks「キャンセルと進捗」、07-cli「SIGINT」 |
| EN-01〜09 | 02-csv-reader「エンコーディングとバイト単位解析」、03-key-and-router「デコード」 |
| RS-01〜13 | 05-checkpoint |
| NF-01〜10 | 01-architecture「メモリ見積り」、04、08-testing |

## 要件書からの差分（一覧）

各ファイルの該当箇所に理由を書いています。ここでは一覧だけ示します。

| 箇所 | 要件書 | 設計 |
| --- | --- | --- |
| RS-01 の初回チェックポイントのオフセット | 入力オフセット 0 | 先頭データレコードの開始位置（ヘッダ行と BOM の直後）。ヘッダ行は再開時に先頭から読み直す |
| `KeyType::DateTime.default_tz` の型 | `Option<Tz>` | 自前の `TimeZoneSpec`（IANA 名と固定オフセットの両方を表す） |
| `_invalid.csv` の生成条件 | 明記なし | strict でなければ常に生成する（FR-35 と同じ理由） |
| 改行コード | 「改行」とのみ記載 | レコード終端は LF（直前の CR は含めて出力）。CR 単独の終端は非対応 |
| 2回目の SIGINT の終了コード | 明記なし | 5（1回目と同じ） |
| 公開 API | inspect / profile / plan / split / read_checkpoint の5関数 | `plan_with_profile` を追加。CLI の `inspect` がキー指定と区間指定の両方を受けたとき、分布と区間件数を1パスで集計するため |
| `RawRecord` | `{ bytes, key }` | `{ bytes, fields, number, offset }`。inspect が全列を必要とするため、全フィールドの開始オフセットを持つ |
| `SplitConfig::validate()` | `Result<(), ConfigError>` | `Result<Vec<Warning>, ConfigError>`。NF-09 の警告をエラーと区別して返す |
