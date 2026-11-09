# 01. 全体アーキテクチャ

## 結論

- 単一パッケージ `ksplit` に lib と bin を同居させ、CLI 専用の依存は `cli` フィーチャ（既定で有効）の背後に置きます
- ライブラリは「レコード読み込み → キー抽出 → 区間判定 → Sink」の1本のパイプラインを持ち、profile / plan / split は Sink の差し替えだけで実現します（FR-66 を構造で保証）
- 正しさを左右する部分（レコード境界、型変換、区間判定、チェックポイント）は自前実装、デコードと日時解析は外部クレートに委ねます

## クレート構成

```toml
[package]
name = "ksplit"
edition = "2021"

[lib]
name = "ksplit"
path = "src/lib.rs"

[[bin]]
name = "ksplit"
path = "src/main.rs"
required-features = ["cli"]

[features]
default = ["cli"]
cli = ["dep:clap", "dep:ctrlc"]

[dependencies]
memchr = "2"
encoding_rs = "0.8"
chrono = { version = "0.4", default-features = false, features = ["std", "clock"] }
chrono-tz = "0.9"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
rand = "0.8"
thiserror = "1"
clap = { version = "4", features = ["derive"], optional = true }
ctrlc = { version = "3", optional = true }

[dev-dependencies]
csv = "1"
proptest = "1"
assert_cmd = "2"
predicates = "3"
tempfile = "3"
```

### 設計判断: 単一パッケージ + フィーチャ

ワークスペースで `ksplit`（lib）と `ksplit-cli`（bin 名 `ksplit`）に分ける案と比較しました。単一パッケージなら `cargo install ksplit` だけで CLI が入り、ライブラリ利用者は `default-features = false` で clap などを外せます。分ける利点はビルド時間の分離だけで、初期規模では見合いません。

### 依存クレートの役割

| クレート | 用途 | 選定理由 |
| --- | --- | --- |
| memchr | 区切り・引用符・改行の高速探索 | バイト単位スキャンの律速部分。SIMD 実装を自前で書かない |
| encoding_rs | Shift_JIS（CP932）→ UTF-8 のデコード | WHATWG の `shift_jis` は Windows-31J 相当の拡張（NEC 特殊文字、IBM 拡張）を含む。不正バイトを置換せず失敗として返す API がある（EN-07） |
| chrono / chrono-tz | strftime 書式の解析、IANA タイムゾーン | 書式が値全体に一致しない場合はエラーになる（FR-14）。書式項目を列挙できるので `%z` の有無を判定できる（FR-15） |
| serde / serde_json | チェックポイント、レポート、`--json` 出力 | `schema_version` 付きの安定した構造を出す（FR-51） |
| sha2 | 設定ハッシュ | 衝突耐性より「同じ設定なら同じ値」が要件。標準的で十分 |
| rand | リザーバサンプリング | 乱数品質は不問。再現性のためシードを固定できるようにする |
| thiserror | エラー型の定義 | 構造化エラー（NF-08）を定型で書く |
| csv（dev） | 自前スキャナのオラクル | レコード境界とフィールド分割の結果を突き合わせる（08-testing） |

## モジュール構成

```
src/
  lib.rs            公開 API の再エクスポート
  error.rs          Error / ConfigError / DataErrorKind / ResumeRejection
  config.rs         Input, CsvDialect, Encoding, KeySpec, SplitConfig, validate()
  cancel.rs         CancelToken, Progress
  scanner.rs        RecordScanner: バイト列からレコード境界とフィールド境界を検出
  source.rs         RecordSource トレイト、CsvSource（ファイル/Reader、BOM、ヘッダ、seek）
  key/
    mod.rs          KeySelector, KeyType, KeyValue, Ord の実装
    decode.rs       キー列の unquote とデコード（UTF-8 / Shift_JIS）
    parse.rs        int / decimal / date / datetime / string の変換
    tz.rs           TimeZoneSpec
  range.rs          Range, Bound, Router, 境界値からの区間生成、出力名のサニタイズ
  pipeline.rs       run(): 読み込み → キー → Consumer、進捗、キャンセル、invalid ガード、チェックポイント間隔
  consumer/
    mod.rs          RecordConsumer トレイト、KeyOutcome
    router.rs       RouterConsumer<S: Sink>（split / plan 共通の振り分け）
    stats.rs        StatsCollector（profile）
  sink/
    mod.rs          Sink トレイト、Destination
    count.rs        CountSink（plan）
    file.rs         FileSink（split）: 出力ファイル群、バッファ、バイト長追跡
  checkpoint.rs     Checkpoint 構造体、書き込み、読み込み、再開検証、設定ハッシュ
  report.rs         Report, RangeResult, Profile, Inspection, CheckpointStatus と JSON 表現
  ops/
    inspect.rs      inspect()
    profile.rs      profile()
    split.rs        plan(), split()
    status.rs       read_checkpoint()
  main.rs           CLI エントリ（cli フィーチャ）
  cli/
    args.rs         clap 定義
    convert.rs      引数 → ライブラリ設定
    run.rs          サブコマンド実行、終了コード
    progress.rs     標準エラー出力への進捗表示
    signal.rs       SIGINT → CancelToken
```

依存の向きは上から下へ一方向です。`scanner` と `key` と `range` は互いに依存せず、`pipeline` が束ねます。`cli/` はライブラリの公開 API だけを使い、`pub(crate)` には触れません。

## データフロー

```
          Input (Path | Reader)
                 │
                 ▼
   ┌─────────────────────────┐
   │ CsvSource               │  BOM 検出、ヘッダ行の保持、レコード境界の検出
   │  └ RecordScanner        │  → RawRecord { bytes, fields(境界オフセット) }
   └─────────────────────────┘
                 │ RawRecord
                 ▼
   ┌─────────────────────────┐
   │ KeyExtractor            │  キー列のフィールドを unquote → decode → trim → KeyType::parse
   └─────────────────────────┘  → KeyOutcome::Valid(KeyValue) | Invalid(reason)
                 │
                 ▼
   ┌─────────────────────────┐
   │ RecordConsumer          │
   │  RouterConsumer<Sink>   │  Router::route → Destination → Sink::accept
   │    ├ CountSink          │  plan: 件数のみ
   │    └ FileSink           │  split: ファイル書き込み、バイト長追跡
   │  StatsCollector         │  profile: min/max/件数/リザーバ
   └─────────────────────────┘
                 │
     進捗コールバック / チェックポイント（FileSink のみ） / CancelToken 確認
```

1レコードあたりの処理は「スキャン（memchr）→ キー列 1 フィールドだけのデコード → 二分探索 → バッファへの memcpy」で、ヒープ確保はキー列の文字列デコード時の作業バッファ再利用のみです。他の列はデコードもコピーもしません（EN-06、FR-33）。

## エラーと終了コードの対応

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),
    #[error("data error at record {record} (byte offset {byte_offset}): {kind}")]
    Data { record: u64, byte_offset: u64, column: Option<usize>, kind: DataErrorKind },
    #[error("I/O error: {source}")]
    Io { path: Option<PathBuf>, #[source] source: std::io::Error },
    #[error("resume rejected: {0}")]
    ResumeRejected(ResumeRejection),
    #[error("cancelled (checkpoint written: {checkpoint_written})")]
    Cancelled { checkpoint_written: bool },
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Config(_) => 1,
            Error::Data { .. } => 2,
            Error::Io { .. } => 3,
            Error::ResumeRejected(_) => 4,
            Error::Cancelled { .. } => 5,
        }
    }
}
```

`exit_code()` はライブラリ側に置きます。GUI など他のフロントエンドが CLI と同じ分類を使えるようにするためです。`Data` のレコード番号はヘッダを除いた 1 始まり、バイトオフセットはファイル先頭からの絶対位置です（NF-08）。

`DataErrorKind` は要件書の4種（`Malformed`、`RecordTooLarge`、`InvalidKey`、`InvalidGuardTripped`）に、`RecordTooLarge` 用の補助情報として `unclosed_quote: bool`（レコードが引用符内の状態で上限に達した）を持たせます。CLI はこれを見て「引用符が閉じられていない可能性」を表示します（FR-06）。

## メモリ見積り（NF-01）

| 要素 | 既定値 | 備考 |
| --- | --- | --- |
| 読み込みバッファ | 1 MiB | レコードがまたがるときのみ `max_record_bytes`（16 MiB）まで成長 |
| キー列の作業バッファ | 数 KiB | unquote 用とデコード用の2本、再利用 |
| 書き込みバッファ | 64 KiB × 出力ファイル数 | 1,000 区間で約 64 MiB。1 GiB 超は警告（NF-09） |
| profile のリザーバ | 100,000 × `KeyValue` | string 型では文字列長に比例。string はヒストグラムを作らないのでサンプルも取らない |
| Router | 区間数 × 境界値 | 数千区間でも数百 KiB |

10 GB 入力でも常駐メモリは数十 MB に収まります。入力サイズに比例する構造は持ちません。

## スレッドモデル

ライブラリは呼び出しスレッドで同期的に動きます。内部でスレッドは立てません。読み込みと書き込みを別スレッドにする案は、ディスク I/O 律速（NF-03）の環境では効果が薄く、チェックポイントの整合性（RS-03、RS-04）を保つための同期が増えるため、初期スコープでは採りません。パイプライン内の `Consumer` と `Sink` の境界はそのまま将来のスレッド分割点になります。

全ての公開型は `Send` です。`Input::Reader(Box<dyn Read + Send>)` としているのは、GUI が設定を組み立てた後にバックグラウンドスレッドへ move できるようにするためです。`progress` コールバックは呼び出しスレッド上で呼ばれるので `Send` 境界は付けません。GUI は `std::sync::mpsc::Sender` をクロージャに捕捉し、UI スレッドへ送れば十分です。
