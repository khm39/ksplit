# 04. パイプラインと Sink

## 結論

- `pipeline::run` が唯一の読み込みループで、profile / plan / split はこれに異なる `RecordConsumer` を渡すだけです
- split と plan は同じ `RouterConsumer<S: Sink>` を使い、`Sink` が `FileSink` か `CountSink` かだけが違います。判定ロジックが1本なので FR-66 の件数一致は構造的に成り立ちます
- チェックポイントの保存タイミングはパイプラインが決め、保存内容は `Sink` が提供します。レコード境界以外で保存されることはありません

## パイプライン

```rust
pub(crate) struct RunOptions {
    pub progress_interval_bytes: u64,
    pub checkpoint_interval_bytes: Option<u64>,
    pub invalid_guard: Option<InvalidGuard>,
    pub on_invalid: OnInvalid,
}

pub(crate) struct RunOutcome {
    pub records: u64,
    pub bytes_read: u64,
    pub elapsed: Duration,
}

pub(crate) fn run<C: RecordConsumer>(
    source: &mut dyn RecordSource,
    extractor: &mut KeyExtractor,
    consumer: &mut C,
    options: &RunOptions,
    cancel: &CancelToken,
    mut progress: impl FnMut(&Progress),
    start: ResumeState,
) -> Result<RunOutcome, Error>
```

ループ本体は次のとおりです。

```rust
let mut next_progress = start.bytes + options.progress_interval_bytes;
let mut next_checkpoint = options.checkpoint_interval_bytes.map(|i| start.bytes + i);
let mut guard = InvalidGuardState::new(options.invalid_guard, start.records);
let mut records = start.records;

while let Some(record) = source.next_record()? {
    records += 1;
    let outcome = extractor.extract(&record);
    if let KeyOutcome::Invalid(reason) = &outcome {
        if options.on_invalid == OnInvalid::Fail {
            return Err(Error::Data { record: record.number, byte_offset: record.offset,
                                     column: Some(extractor.column()), kind: DataErrorKind::InvalidKey });
        }
    }
    guard.observe(&outcome, &record)?;
    consumer.consume(&record, outcome)?;

    let pos = source.position();
    if pos >= next_progress {
        progress(&Progress { bytes_read: pos, total_bytes: source.total_bytes(), records });
        next_progress = pos + options.progress_interval_bytes;
    }
    if let Some(cp) = next_checkpoint.filter(|cp| pos >= *cp) {
        consumer.checkpoint(pos, records)?;
        next_checkpoint = Some(pos + options.checkpoint_interval_bytes.unwrap());
    }
    if cancel.is_cancelled() {
        let written = consumer.on_cancel(pos, records)?;
        return Err(Error::Cancelled { checkpoint_written: written });
    }
}
consumer.finish_input(source.position(), records)?;
```

- `records` は再開時に復元した件数から数え始めるので、`record.number`（ソースが seek 後に 1 から振り直す番号）とは別に持ちます。エラー報告には `records` を使います
- キャンセルと進捗とチェックポイントの判定はすべてレコード処理後に行うので、レコード境界以外で割り込むことはありません（RS-03、FR-70）
- `extract` で `Invalid` になった行も `consume` に渡します。`RouterConsumer` が `_invalid.csv` へ書き、`StatsCollector` が invalid 件数を数えます
- strict（`OnInvalid::Fail`）は `consume` の前に止まるので、`_invalid.csv` には何も書かれません（FR-43）

### invalid ガード（FR-45）

```rust
struct InvalidGuardState {
    window: Option<u64>,
    seen: u64,
    all_invalid: bool,
}
```

`observe` は先頭 `window` 件（既定 10,000、再開時は復元したレコード数から数えるので再開直後は判定しない）の間、1件でも `Valid` が来れば `all_invalid = false` にして以後何もしません。`window` 件目を処理した時点で `all_invalid` のままなら `Error::Data { kind: InvalidGuardTripped, record: window, .. }` を返します。入力が `window` 件未満で終わった場合は判定しません。`window` 件目の判定直前に `consume` は済んでいるため、ガードが発火した時点で `_invalid.csv` には 10,000 行が書かれていますが、終了コード 2 で停止するので出力ディレクトリはそのまま残り、ユーザが中身を見て原因（キー列や型の指定誤り）を確認できます。

## RecordConsumer

```rust
pub(crate) trait RecordConsumer {
    fn consume(&mut self, record: &RawRecord<'_>, key: KeyOutcome) -> Result<(), Error>;
    fn checkpoint(&mut self, input_offset: u64, records: u64) -> Result<(), Error>;
    fn on_cancel(&mut self, input_offset: u64, records: u64) -> Result<bool, Error>;
    fn finish_input(&mut self, input_offset: u64, records: u64) -> Result<(), Error>;
}
```

| 実装 | consume | checkpoint / on_cancel | 用途 |
| --- | --- | --- | --- |
| `RouterConsumer<FileSink>` | `Router::route` → ファイルへ書く | 全出力を flush + fsync し、チェックポイントを書く / `true` を返す | split |
| `RouterConsumer<CountSink>` | `Router::route` → 件数を加算 | 何もしない / `false` | plan |
| `StatsCollector` | min / max / 件数 / リザーバ更新 | 何もしない / `false` | profile |

### RouterConsumer

```rust
pub(crate) struct RouterConsumer<S: Sink> {
    router: Router,
    mode: Mode,
    sink: S,
    counts: Counts,   // per_range: Vec<u64>, unmatched, invalid
}

pub(crate) enum Destination {
    Range(usize),
    Unmatched,
    Invalid,
    Drop,
}
```

`consume` の振り分けは次の表のとおりです。

| key | mode | Destination |
| --- | --- | --- |
| `Valid` かつ `Routed::Range(i)` | 両方 | `Range(i)` |
| `Valid` かつ `Routed::Unmatched` | split | `Unmatched`（FR-40） |
| `Valid` かつ `Routed::Unmatched` | extract | `Drop`（FR-41） |
| `Invalid` | 両方 | `Invalid`（FR-42） |

`Drop` 以外は `sink.accept(record, destination)` を呼び、`counts` を更新します。`Drop` は件数にも含めません（extract で捨てた行数はレポートに出しません。要件書に項目がなく、必要なら plan で `--mode split` を試算すれば unmatched として数えられるためです）。

## Sink

```rust
pub(crate) trait Sink {
    fn accept(&mut self, record: &RawRecord<'_>, dest: Destination) -> Result<(), Error>;
    fn persist(&mut self) -> Result<Vec<OutputLength>, Error>;
    fn finish(&mut self) -> Result<Vec<Option<PathBuf>>, Error>;
}

pub(crate) struct OutputLength {
    pub dest: Destination,
    pub bytes: u64,
}
```

`CountSink` は `accept` で何もせず、`persist` は空、`finish` は全て `None` を返します（`RangeResult.path` が plan で `None` になる根拠）。

### FileSink（split）

```rust
pub(crate) struct FileSink {
    outputs: Vec<Output>,            // ranges と同じ順序
    unmatched: Option<Output>,       // split モードのみ
    invalid: Option<Output>,         // strict でなければ常に Some
    dir: PathBuf,
}

struct Output {
    path: PathBuf,
    writer: BufWriter<File>,
    bytes: u64,
}
```

- `open_fresh(dir, router, mode, on_invalid, header, bom, write_buffer_bytes)` は出力ディレクトリを `create_dir_all` し、各ファイルを `create_new` で作ります。既存ファイルがあれば `Io` ではなく `ConfigError::OutputExists` に変換して処理開始前に止めます（FR-36）。`ExistingOutput::Overwrite` のときだけ `create` + `truncate` を使います
- 作成直後に各ファイルへ BOM とヘッダ行を書きます（FR-32）。`has_header == false` なら BOM だけです。この時点で `bytes` はヘッダ長になります
- `accept` は `record.bytes` をそのまま `writer.write_all` し、`bytes += len` します（FR-33、FR-34）
- `persist` は全 `writer.flush()` の後、各 `File::sync_all()` を呼び、`bytes` の一覧を返します。チェックポイントの「各出力ファイルのバイト長」はこの値です。ファイルシステム上の長さを `metadata()` で読み直すことはしません（書いた長さと一致しているはずで、一致しないならそれ自体が I/O 異常です）
- `finish` は flush と `sync_all` をして全パスを返します。1行も書かれなかった区間のファイルもヘッダ付きで残ります（FR-35）

### 設計判断: `_invalid.csv` は strict でなければ常に作る

要件書は `_invalid.csv` の生成条件を定めていません。FR-35 と同じ理由（後続処理がファイルの存在を前提にできる）で、strict でない限り invalid 行が 0 件でもヘッダのみのファイルを作ります。`_unmatched.csv` は split モードでのみ作り、extract モードでは作りません（FR-41）。

### 再開時の open（05 と連携）

`open_resumed(dir, checkpoint, ...)` は各ファイルを `OpenOptions::new().write(true)` で開き、`metadata().len()` が記録長以上であることを確認して `set_len(recorded)` し、`seek(SeekFrom::End(0))` して `bytes = recorded` から再開します。記録長未満なら `ResumeRejection::OutputTruncated { file, expected, actual }` です。チェックポイントに記録されたファイルの集合と、現在の設定から導かれるファイルの集合が一致しない場合は設定ハッシュの段階で既に拒否されています。

## キャンセルと進捗

```rust
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self;
    pub fn cancel(&self) { self.0.store(true, Ordering::Relaxed) }
    pub fn is_cancelled(&self) -> bool { self.0.load(Ordering::Relaxed) }
}
```

`Relaxed` で十分です。キャンセルは「いずれ止まればよい」通知で、停止後の処理はパイプライン内の通常の制御フローで同期されるためです。

進捗コールバックは `progress_interval_bytes`（既定 16 MiB）ごとにレコード境界で呼ばれます（FR-73）。コールバック内で重い処理をしてもパイプラインが止まるだけで、整合性には影響しません。CLI 側は時間ベースの間引きを追加して描画回数を抑えます（07）。

## Report

```rust
#[derive(Serialize)]
pub struct Report {
    pub schema_version: u32,          // 1
    pub per_range: Vec<RangeResult>,
    pub unmatched: u64,
    pub invalid: u64,
    pub records: u64,
    pub bytes_read: u64,
    pub elapsed: Duration,            // JSON では秒（f64）
    pub resumed_from: Option<u64>,    // 再開した入力オフセット
}

#[derive(Serialize)]
pub struct RangeResult {
    pub name: String,
    pub path: Option<PathBuf>,
    pub records: u64,
}
```

plan と split は同じ `Report` を返し、違いは `path` の有無だけです（FR-66）。`records` は処理したレコード総数で、再開時はチェックポイントから復元した件数を含みます。`bytes_read` は入力先頭からの位置（`source.position()`）で、再開時も先頭からの値です。

JSON のフィールド名は `snake_case` で固定し、`schema_version` を先頭に置きます。構造を変える（フィールドの削除や意味の変更）ときだけ値を上げ、フィールドの追加では上げません（FR-51）。
