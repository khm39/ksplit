# 05. チェックポイントと再開

## 結論

- チェックポイントは出力ディレクトリ直下の `.ksplit-checkpoint.json` 1ファイルで、入力オフセット・各出力のバイト長・件数・設定ハッシュ・入力の同一性情報を持ちます
- 保存は「全出力 flush → 各出力 fsync → 一時ファイルへ書いて fsync → rename → ディレクトリ fsync」の順で、どの時点で落ちても「チェックポイントが指す状態まで出力が永続化されている」ことが成り立ちます
- 再開は truncate + seek です。設定不一致・入力変更・出力の欠損はすべて拒否し、抑えフラグは作りません

## ファイル形式

```json
{
  "schema_version": 1,
  "tool_version": "0.1.0",
  "config_hash": "sha256:3b1f…",
  "input": {
    "path": "/data/orders.csv",
    "size": 10737418240,
    "mtime_unix_nanos": 1762650000123456789
  },
  "input_offset": 2415919104,
  "records_done": 12345678,
  "outputs": [
    { "role": "range", "name": "lt_2024-01-01", "file": "lt_2024-01-01.csv", "bytes": 104857600, "records": 400000 },
    { "role": "range", "name": "2024-01-01_2024-04-01", "file": "2024-01-01_2024-04-01.csv", "bytes": 209715200, "records": 800000 },
    { "role": "unmatched", "name": "_unmatched", "file": "_unmatched.csv", "bytes": 120, "records": 0 },
    { "role": "invalid", "name": "_invalid", "file": "_invalid.csv", "bytes": 5120, "records": 17 }
  ],
  "saved_at": "2026-11-09T07:30:00Z"
}
```

```rust
#[derive(Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub schema_version: u32,
    pub tool_version: String,
    pub config_hash: String,
    pub input: InputIdentity,
    pub input_offset: u64,
    pub records_done: u64,
    pub outputs: Vec<OutputEntry>,
    pub saved_at: String,            // RFC 3339 UTC
}

#[derive(Serialize, Deserialize)]
pub(crate) struct InputIdentity {
    pub path: PathBuf,               // 絶対パス（canonicalize 済み）
    pub size: u64,
    pub mtime_unix_nanos: i128,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct OutputEntry {
    pub role: OutputRole,            // range | unmatched | invalid
    pub name: String,
    pub file: String,                // ディレクトリからの相対ファイル名
    pub bytes: u64,
    pub records: u64,
}
```

`outputs` は `Router::ranges()` と同じ順序で range を並べ、その後に unmatched、invalid を置きます。区間ごとの行数（RS-02）は各エントリの `records` です。`unmatched` と `invalid` のエントリは、そのファイルを作らない設定（extract モード、strict）では省きます。

`mtime_unix_nanos` は `SystemTime` を UNIX エポックからのナノ秒にしたもので、ファイルシステムによる精度の違い（ext4 はナノ秒、FAT は 2 秒）はそのまま保存し、比較も保存値との完全一致で行います。同じファイルシステム上で変更がなければ値は変わりません。

## 設定ハッシュ（RS-02）

```rust
#[derive(Serialize)]
struct HashedConfig<'a> {
    hash_version: u32,                // 1
    delimiter: u8,
    has_header: bool,
    max_record_bytes: usize,
    encoding: &'a Encoding,
    key_selector: &'a KeySelector,
    key_type: &'a KeyType,            // DateTime の default_tz は IANA 名または "+09:00" の文字列
    trim: bool,
    ranges: Vec<HashedRange>,         // ソート後の順序、名前は サニタイズ後、境界値は正規形の文字列
    mode: &'a Mode,
    on_invalid: &'a OnInvalid,
}
```

`serde_json::to_vec` でシリアライズし、SHA-256 の 16 進表現に `sha256:` を付けます。`serde_json` は構造体のフィールドを定義順に出力し、`Map` を使っていないので出力は決定的です。境界値は `KeyValue` の内部表現ではなく正規形の文字列（03 の表）にします。内部表現（`i128` や `NaiveDate`）の serde 表現がクレートのバージョンで変わっても、ハッシュが変わらないようにするためです。

含めないもの: 入力パス（`input` で別に照合）、出力ディレクトリ（チェックポイントの置き場所そのもの）、バッファサイズ、チェックポイント間隔、進捗間隔、レポート出力先、invalid ガードの件数。これらは出力内容を変えません。

`tool_version` は `env!("CARGO_PKG_VERSION")` で、不一致なら拒否します（再開手順 1）。パッチバージョン違いでも拒否するのは、出力に影響するバグ修正が入った可能性を排除できないためです。緩めるなら「メジャー・マイナー一致」ですが、初期は厳格側に倒します。

## 保存手順（RS-01、RS-04、RS-05）

`RouterConsumer<FileSink>::checkpoint(input_offset, records)` は次を順に行います。

1. `sink.persist()`: 全出力の `BufWriter::flush()` → 各 `File::sync_all()`。返ってきたバイト長で `outputs[*].bytes` を埋める
2. `Checkpoint` を組み立て、`serde_json::to_vec_pretty` で `.ksplit-checkpoint.json.tmp` に書く（`create` + `truncate`）
3. その一時ファイルを `sync_all()`
4. `fs::rename(tmp, ".ksplit-checkpoint.json")`
5. 出力ディレクトリを `File::open(dir)?.sync_all()`（Unix のみ。Windows ではディレクトリハンドルの fsync ができないので省略し、NF-06 のとおりベストエフォート）

順序が重要なのは 1 → 4 の間です。出力の fsync が完了した後に rename されるので、チェックポイントが観測できる時点で、そこに書かれた長さまでの出力バイト列はディスクにあります（NF-05）。逆に、出力の fsync 後・rename 前に落ちれば、古いチェックポイントが残り、出力は古いチェックポイントの長さより長い状態になります。これは再開手順 3 の truncate が吸収します。

初回のチェックポイントは出力ファイルの作成とヘッダ書き込みの直後、1件目のレコードを `consume` する前に書きます（RS-01）。`input_offset` は先頭データレコードの開始位置（ヘッダ行末尾の次のバイト）です。要件書の「オフセット 0」からの差分は README に記載しています。これにより「出力ファイルはあるがチェックポイントがない」状態が作られるのは、`create_new` 直後の数ミリ秒だけになります。

以後は読み込みバイト数が `interval_bytes`（既定 256 MiB）進むごと（04 のループ）、およびキャンセル時（FR-71）に保存します。正常終了時は `finish` の後にチェックポイントファイルを削除します（RS-06）。削除は `remove_file` で、存在しなくてもエラーにしません。

### 設計判断: チェックポイント中は書き込みを止める

`persist` 中（flush と fsync）はパイプラインが止まります。256 MiB ごとに数十〜数百ミリ秒の停止で、10 GB で約 40 回なので総停止時間は数秒です。非同期化して隠すこともできますが、fsync 完了前に次のレコードを書くと「チェックポイントの長さまでは永続化済み」の保証が崩れるため、同期のまま置きます。

## 再開手順

`split` が `ExistingOutput::Resume` で呼ばれたときの流れです。

```
1. .ksplit-checkpoint.json を読む
   なし & 出力ファイルなし  → 警告を返し、最初から実行（RS-13）
   なし & 出力ファイルあり  → ResumeRejection::OutputsWithoutCheckpoint（終了コード 4、RS-13）
   あり & パース失敗        → ResumeRejection::CorruptCheckpoint
2. schema_version != 1                        → CorruptCheckpoint
   tool_version != 現在                       → VersionMismatch { saved, current }
   config_hash != 現在の設定のハッシュ         → ConfigMismatch
3. Input が Path でなければ                   → ConfigError::ResumeRequiresPath（RS-10、validate() で事前に検出）
   canonicalize(path) != saved.path            → InputMismatch { field: "path" }
   metadata.len() != saved.size                → InputMismatch { field: "size" }
   mtime != saved.mtime                        → InputMismatch { field: "mtime" }
4. 各 outputs エントリについて
   ファイルが存在しない                        → OutputMissing { file }
   len < saved.bytes                           → OutputTruncated { file, expected, actual }
   set_len(saved.bytes), seek(End)
   outputs に無い .csv が出力ディレクトリにある → 無視（別実行の残骸でも、設定ハッシュが一致している以上このファイル集合は確定している）
5. CsvSource を先頭から開き BOM とヘッダを読み、key の解決と検証を通した後 seek(saved.input_offset)
6. Counts を outputs[*].records、unmatched、invalid から復元
   records_done を復元し、invalid ガードの観測数もそこから開始
7. Report.resumed_from = Some(saved.input_offset) としてパイプラインを続行
```

拒否はすべて `Error::ResumeRejected`（終了コード 4）で、メッセージには保存値と現在値の両方を含めます。

### 設計判断: 入力の同一性はサイズと mtime のみ

要件書 RS-02 のとおりです。内容ハッシュを取ると全体を読む必要があり、再開の意味がなくなります。先頭数 MiB のハッシュを追加で持つ案は「同サイズ・同 mtime で先頭だけ同じ」ケースを防げず、効果が限定的なので採りません。

### `--overwrite` との関係

`ExistingOutput::Overwrite` は既存の出力ファイルとチェックポイントを削除してから最初から実行します。削除対象は「現在の設定から導かれるファイル名」と `.ksplit-checkpoint.json` だけで、出力ディレクトリ内の他のファイルには触れません。`Resume` と `Overwrite` は排他で、`validate()` が検査します。チェックポイントがあるのに `Fail`（既定）で呼ばれた場合は `ConfigError::InterruptedRunExists` で、前回が中断している旨とフラグの案内を返します（RS-12）。

## status（FR-67）

```rust
pub struct CheckpointStatus {
    pub schema_version: u32,
    pub tool_version: String,
    pub input_path: PathBuf,
    pub input_unchanged: bool,
    pub input_change: Option<String>,   // 変わっていれば何が変わったか
    pub bytes_done: u64,
    pub total_bytes: u64,
    pub records_done: u64,
    pub per_range: Vec<RangeResult>,
    pub unmatched: Option<u64>,
    pub invalid: Option<u64>,
    pub saved_at: String,
}

pub fn read_checkpoint(output_dir: &Path) -> Result<Option<CheckpointStatus>, Error>;
```

`read_checkpoint` はチェックポイントを読み、入力ファイルの現在のサイズと mtime を比較して `input_unchanged` を埋めます。入力ファイルが存在しなければ `input_unchanged = false`、`input_change = Some("missing")` です。設定ハッシュは現在の設定がないので照合しません。チェックポイントがなければ `Ok(None)`、壊れていれば `ResumeRejected(CorruptCheckpoint)` を返します。`total_bytes` は保存時の `input.size` です。
