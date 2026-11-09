# 07. CLI

## 結論

- CLI は clap（derive）で引数を受け、`cli/convert.rs` でライブラリの設定型に変換し、ライブラリの関数を呼んで結果を整形するだけです。判定ロジックは持ちません
- `inspect` はキー指定と区間指定の有無で `inspect` / `profile` / `plan` を呼び分けます。全件読み込みに切り替わるときは開始前に標準エラー出力へ明示します
- SIGINT は `ctrlc` で受け、1回目は `CancelToken::cancel()`、2回目は即時 `process::exit(5)` です

## 引数定義

```rust
#[derive(Parser)]
#[command(name = "ksplit", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Inspect(InspectArgs),
    Split(SplitArgs),
    Status(StatusArgs),
}

#[derive(Args)]
struct CsvOpts {
    #[arg(long, default_value = "utf-8", value_parser = ["utf-8", "shift_jis"])]
    encoding: String,
    #[arg(long, default_value = ",")]
    delimiter: String,
    #[arg(long)]
    no_header: bool,
    #[arg(long, default_value = "16MiB", value_parser = parse_size)]
    max_record_size: u64,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    quiet: bool,
}

#[derive(Args)]
struct KeyOpts {
    #[arg(long)]
    key: Option<String>,
    #[arg(long, value_enum)]
    r#type: Option<KeyTypeArg>,
    #[arg(long)]
    scale: Option<u32>,
    #[arg(long)]
    format: Option<String>,
    #[arg(long)]
    tz: Option<String>,
    #[arg(long)]
    no_trim: bool,
    #[arg(long, default_value_t = 10_000, conflicts_with = "no_invalid_guard")]
    invalid_guard: u64,
    #[arg(long)]
    no_invalid_guard: bool,
}

#[derive(Args)]
struct RangeOpts {
    #[arg(long, value_delimiter = ',', conflicts_with = "range")]
    bounds: Option<Vec<String>>,
    #[arg(long = "range", value_name = "NAME=LO..HI")]
    range: Vec<String>,
    #[arg(long, value_enum, default_value_t = ModeArg::Split)]
    mode: ModeArg,
}

#[derive(Args)]
struct InspectArgs {
    input: String,
    #[arg(long, default_value_t = 20)]
    sample: usize,
    #[arg(long, default_value_t = 20)]
    bins: usize,
    #[arg(long, default_value_t = 100_000)]
    sample_size: usize,
    #[arg(long)]
    suggest: Option<usize>,
    #[arg(long)]
    strict: bool,
    #[command(flatten)] csv: CsvOpts,
    #[command(flatten)] key: KeyOpts,
    #[command(flatten)] range: RangeOpts,
}

#[derive(Args)]
struct SplitArgs {
    input: String,
    #[arg(short, long)]
    out_dir: PathBuf,
    #[arg(long)]
    strict: bool,
    #[arg(long, conflicts_with = "overwrite")]
    resume: bool,
    #[arg(long)]
    overwrite: bool,
    #[arg(long, default_value = "256MiB", value_parser = parse_size)]
    checkpoint_interval: u64,
    #[arg(long, default_value = "64KiB", value_parser = parse_size)]
    write_buffer: u64,
    #[arg(long)]
    report: Option<PathBuf>,
    #[command(flatten)] csv: CsvOpts,
    #[command(flatten)] key: KeyOpts,
    #[command(flatten)] range: RangeOpts,
}

#[derive(Args)]
struct StatusArgs {
    out_dir: PathBuf,
    #[arg(long)]
    json: bool,
}
```

`split` では `--key` と `--type` と区間指定が必須ですが、`inspect` と同じ `KeyOpts` / `RangeOpts` を共有するため clap の `required` では表現せず、`convert.rs` で検査して `ConfigError` にします（終了コード 1）。「`inspect` の行の `inspect` を `split` に変えて `-o` を足せばそのまま動く」（要件書 CLI 仕様）ためには、両者が同じオプション集合を受ける必要があります。

### サイズ引数

`parse_size` は `16MiB`、`256MiB`、`64KiB`、`1GiB` と、単位なしのバイト数、`16M`（MiB として扱う）を受けます。10 進接頭辞（MB）は 2 進と同じ扱いにします。曖昧さを避けるためヘルプには `MiB` 表記のみを載せます。

## 引数からライブラリ設定への変換（convert.rs）

| 引数 | 変換 | エラー（終了コード 1） |
| --- | --- | --- |
| `--delimiter` | 1 文字の ASCII → `u8`。`\t` は特別に許可 | 2 文字以上、非 ASCII、`"`、改行 |
| `--encoding` | `Encoding::Utf8` / `Encoding::ShiftJis` | |
| `--key` | `#` 始まりなら `Index(n)`、それ以外は `Name`。`--no-header` 時は `Index` のみ | `--no-header` と列名 |
| `--type` + `--scale` / `--format` / `--tz` | `KeyType` を組み立てる。`decimal` に `--scale` 必須、`date`/`datetime` に `--format` 必須。`--tz` は IANA 名を `str::parse::<chrono_tz::Tz>()`、`±HH:MM` を `FixedOffset` に | 必須オプション欠落、不要なオプションの指定、不正な書式・TZ |
| `--bounds` | 各値を `KeyType::parse`（trim あり）し `Range::from_bounds` | 変換不可、重複 |
| `--range NAME=LO..HI` | `=` で名前と範囲に分け、`..` で lo と hi に分ける。空側は `Unbounded` | `=` や `..` がない、名前が空 |
| `--invalid-guard` / `--no-invalid-guard` | `Some(InvalidGuard { window_records })` / `None` | |
| `--resume` / `--overwrite` | `ExistingOutput::{Resume, Overwrite}`、どちらもなければ `Fail` | |
| `INPUT` | `-` なら `Input::Reader(Box::new(io::stdin().lock()))`、それ以外は `Input::Path` | `-` と `--resume` の併用（ライブラリの `validate()` でも検出） |

`--range` の値側に `..` が含まれる文字列型のキー（例: `a..b`）は区切りと衝突しますが、最初の `..` で分割する規則に固定し、ヘルプに記載します。

## inspect の3段階

```rust
fn run_inspect(args: InspectArgs) -> Result<Output, Error> {
    match (key_specified(&args.key), range_specified(&args.range)) {
        (false, false) => Output::Inspection(ksplit::inspect(cfg)?),
        (false, true)  => Err(ConfigError::RangeWithoutKey.into()),
        (true, false)  => { announce_full_scan(&args); Output::Profile(ksplit::profile(cfg, &cancel, progress)?) }
        (true, true)   => { announce_full_scan(&args); Output::Plan(ksplit::plan(&mut cfg, &cancel, progress)?) }
    }
}
```

`announce_full_scan` は `--quiet` でなければ「キー指定があるため全件を読みます」を標準エラー出力に書きます。

キー指定と区間指定の両方があるときに分布（profile）と区間件数（plan）を同じパスで集計するには、ライブラリ側に両方を受ける関数が必要です。`profile` と `plan` を別々に呼ぶと入力を2回読むことになります。これを避けるため、ライブラリに `pub fn plan_with_profile(config: &mut SplitConfig, profile: ProfileOptions, ...) -> Result<(Report, Profile), Error>` を追加し、内部では `RouterConsumer<CountSink>` と `StatsCollector` の両方に `consume` を回す `TeeConsumer` を使います。要件書の公開 API 案には無い関数ですが、「同じパスで集計する」という CLI 仕様を満たすために必要です。

## split の実行

1. 引数を `SplitConfig` に変換し、`validate()` を呼ぶ
2. 区間数 × `write_buffer` が 1 GiB を超えていれば警告を標準エラー出力に書く（NF-09）。`validate()` が `Vec<Warning>` を返す形にし、CLI が表示します
3. `CancelToken` を作り、SIGINT ハンドラを登録する
4. `ksplit::split(cfg, &cancel, progress)` を呼ぶ
5. 結果の `Report` を標準エラー出力に表形式で表示し、`--report` があれば JSON で保存、`--json` なら標準出力に JSON を書く
6. `Error` を `exit_code()` に変換して終了する

## SIGINT の扱い

```rust
pub(crate) fn install(cancel: CancelToken) {
    let count = Arc::new(AtomicU8::new(0));
    ctrlc::set_handler(move || {
        let n = count.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            cancel.cancel();
            eprintln!("\ninterrupted: writing checkpoint, press Ctrl+C again to abort immediately");
        } else {
            std::process::exit(5);
        }
    }).expect("install SIGINT handler");
}
```

1回目はトークンを立てるだけで、パイプラインがレコード境界で検知してチェックポイントを書き、`Error::Cancelled` で戻ります。CLI はこれを終了コード 5 にします。2回目は `process::exit(5)` で即時終了します。この場合チェックポイントは書かれませんが、直前のチェックポイントからの再開は可能です（FR-72）。終了コードは要件書に明記がないため、1回目と同じ 5 にしています（README「要件書からの差分」）。

`inspect` でも同じハンドラを使いますが、チェックポイントがないので `Cancelled { checkpoint_written: false }` で終了コード 5 です。`status` は瞬時に終わるのでハンドラを登録しません。

ハンドラ内で `eprintln!` を使っていますが、シグナルハンドラとして安全かどうかは `ctrlc` の実装（専用スレッドで動く）に依存します。`ctrlc` 3.x は専用スレッドからハンドラを呼ぶため、通常の Rust コードを書いて問題ありません。

## 進捗表示

```rust
pub(crate) struct ProgressPrinter {
    last_draw: Instant,
    is_tty: bool,
    quiet: bool,
}
```

- ライブラリからのコールバック（16 MiB ごと）を受け、前回描画から 200 ms 以上経っていれば描画します
- 標準エラー出力が端末なら `\r` で同じ行を書き換え、`読み込み 2.4 GiB / 10.0 GiB (24%)  12,345,678 行  180 MB/s` の形です。`total_bytes` が `None`（標準入力）なら割合と全体を省きます（FR-52）
- 端末でなければ `\r` を使わず改行区切りで、間隔を 5 秒に広げます
- `--quiet` なら何も書きません。`--json` 時も進捗は標準エラー出力なので干渉しません

端末判定は `std::io::IsTerminal` を使います（Rust 1.70 以降の標準ライブラリ）。

## 結果の表示

### 人間向け（標準エラー出力）

```
split complete: 10,000,000 records, 10.0 GiB in 58.2s (176 MB/s)

  range                      records   file
  lt_2024-01-01            1,234,567   out/lt_2024-01-01.csv
  2024-01-01_2024-04-01    2,345,678   out/2024-01-01_2024-04-01.csv
  ...
  _unmatched                   1,234   out/_unmatched.csv
  _invalid                        17   out/_invalid.csv
```

### JSON（`--json`、`--report`）

`Report`、`Profile`、`Inspection`、`CheckpointStatus` を `serde_json::to_writer_pretty` で書きます。`KeyValue` は正規形の文字列（03 の表）でシリアライズします。`Duration` は秒の `f64` です。`--json` 時は標準出力に結果だけを書き、進捗と警告は標準エラー出力に出すので、`ksplit inspect ... --json | jq` がそのまま動きます。

### エラー

`Error` の `Display` をそのまま標準エラー出力に `error: ` 付きで書きます。`Data` エラーはレコード番号とバイトオフセットを含み（NF-08）、`RecordTooLarge { unclosed_quote: true }` なら「引用符が閉じられていない可能性があります。該当レコードの開始位置: <offset>」を追記します。`ResumeRejected` は保存値と現在値の両方を表示し、`InterruptedRunExists` は `--resume` または `--overwrite` の案内を添えます。
