# 06. 解析機能（inspect / profile）

## 結論

- `inspect` は先頭 `sample_rows` 件だけを読み、全列をデコードして列情報と型候補を返します。入力の大きさに関わらず読み込み量は一定です
- `profile` は split と同じパイプラインに `StatsCollector` を差し、正確な件数・min・max と、リザーバサンプルに基づく近似のヒストグラム・境界値提案を返します
- `plan` は 04 の `RouterConsumer<CountSink>` そのもので、ここでは扱いません

## inspect（FR-60、FR-61）

```rust
pub struct InspectConfig {
    pub input: Input,
    pub dialect: CsvDialect,
    pub encoding: Encoding,
    pub sample_rows: usize,          // 既定 20
}

pub struct Inspection {
    pub schema_version: u32,
    pub has_bom: bool,
    pub line_ending: LineEnding,     // Lf | CrLf | Unknown
    pub column_count: usize,         // ヘッダの列数、ヘッダなしなら先頭レコードの列数
    pub columns: Vec<ColumnInfo>,
    pub sampled_records: usize,
}

pub struct ColumnInfo {
    pub index: usize,
    pub name: Option<String>,
    pub samples: Vec<String>,
    pub type_candidates: Vec<KeyType>,
}

pub fn inspect(config: InspectConfig) -> Result<Inspection, Error>;
```

手順は次のとおりです。

1. `CsvSource::open` で BOM とヘッダを読む
2. 先頭 `sample_rows` 件のレコードを `next_record` で読み、各フィールドを unquote → デコードして `samples` に積む。デコードに失敗したフィールドは `samples` に含めず、別途 `undecodable: usize` を列ごとに数える（CLI は「n 件デコード不可」と表示する）
3. 最初のレコードの終端から改行コードを判定する（LF 直前が CR なら `CrLf`）。レコードが1件もなければ `Unknown`
4. 各列について型候補を判定する

inspect では `max_record_bytes` もそのまま効きます。先頭 20 件の読み込みで `RecordTooLarge` になるなら、split でも同じ場所で止まるので、下見の段階で気づけます。

### 型候補の判定（FR-61）

列のサンプル値（trim 後、空文字列は除外）がすべて次のいずれかに変換できれば候補とします。空でないサンプルが 0 件なら候補なしです。

| 候補 | 判定 |
| --- | --- |
| `Int` | すべて `-?[0-9]+` に一致し `i64` に収まる |
| `Decimal { scale }` | すべて `-?[0-9]+(\.[0-9]+)?` に一致。`scale` はサンプル中の最大小数桁数。`Int` が候補なら `Decimal { scale: 0 }` は出さない |
| `Date { format }` | 次のいずれかの書式ですべて解析できる: `%Y-%m-%d`、`%Y/%m/%d`、`%Y%m%d` |
| `DateTime { format, default_tz: None }` | 次のいずれか: `%Y-%m-%d %H:%M:%S`、`%Y-%m-%dT%H:%M:%S`、`%Y-%m-%dT%H:%M:%S%z`、`%Y-%m-%dT%H:%M:%S%:z`、`%Y/%m/%d %H:%M:%S` |

`String` は常に成立するので候補には含めません。候補は提案であり、split の `--type` は引き続き明示です（FR-11）。`%Y%m%d` と `Int` は同時に候補になりえます（`20240101` はどちらにも読める）。両方返し、どちらを使うかは利用者が決めます。

## profile（FR-62〜64）

```rust
pub struct ProfileConfig {
    pub input: Input,
    pub dialect: CsvDialect,
    pub encoding: Encoding,
    pub key: KeySpec,
    pub bins: usize,                 // 既定 20
    pub sample_size: usize,          // 既定 100,000
    pub suggest: Option<usize>,
    pub seed: Option<u64>,           // リザーバの乱数シード。None なら OS 乱数
    pub progress_interval_bytes: u64,
}

pub struct Profile {
    pub schema_version: u32,
    pub records: u64,
    pub invalid: u64,
    pub min: Option<KeyValue>,
    pub max: Option<KeyValue>,
    pub histogram: Option<Histogram>,
    pub suggested_bounds: Option<Vec<KeyValue>>,
    pub bytes_read: u64,
    pub elapsed: Duration,
}

pub struct Histogram {
    pub approximate: bool,           // 常に true
    pub sampled: u64,                // リザーバに入った件数（valid 件数が sample_size 以下なら全件）
    pub bins: Vec<HistogramBin>,
}

pub struct HistogramBin {
    pub lo: KeyValue,
    pub hi: KeyValue,                // 最後のビンだけ max を含む（閉区間）
    pub estimated_count: u64,
}
```

### StatsCollector

```rust
pub(crate) struct StatsCollector {
    valid: u64,
    invalid: u64,
    min: Option<KeyValue>,
    max: Option<KeyValue>,
    reservoir: Option<Reservoir>,    // string 型では None
}

struct Reservoir {
    capacity: usize,
    items: Vec<KeyValue>,
    seen: u64,
    rng: rand::rngs::StdRng,
}
```

`consume` は `Valid(v)` なら件数・min・max を更新し、リザーバに `offer(v)` します。`Invalid` なら `invalid` を加算します。リザーバは Algorithm R（`seen < capacity` なら追加、以後は `rng.gen_range(0..=seen)` が `capacity` 未満ならその位置を置換）です。1 レコードあたり乱数 1 回で、Algorithm L のスキップ計算より単純で、1,000 万件でも乱数生成はボトルネックになりません。

`min`、`max`、`records`、`invalid` は全件を見るので正確です。string 型では `min` と `max` は返し、リザーバは作りません（要件書の決定どおり。文字列のヒストグラムは意味を持たず、サンプルのメモリも文字列長に比例するため）。

### ヒストグラムの構築

全件を読み終えた後、正確な `min` と `max` からビン幅を決め、リザーバの各値をビンに落とします。

1. `min == max` ならビン1つ
2. 値を整数のスカラーに写す: `Int` → `i128`、`Decimal` → `i128`（内部表現）、`Date` → エポックからの日数、`DateTime` → エポックからの秒
3. `width = ceil((max - min + 1) / bins)`。`bins` 個のビン `[min + i*width, min + (i+1)*width)` を作り、最後のビンの `hi` は `max`（閉）に丸める
4. 各サンプルのビン番号は `(v - min) / width`
5. `estimated_count = ビン内サンプル数 × valid / sampled`（`sampled == valid` なら正確値だが `approximate` は常に `true` のまま）

スカラーからの逆変換（ビン境界を `KeyValue` に戻す）は型ごとに用意し、`Decimal` は同じ `scale` を使います。

### 境界値の提案（FR-64）

`suggest = Some(n)` なら、リザーバをソートし、`k = 1..n` について `items[floor(k * len / n)]` を取り、重複を除いて返します。同じ値が大量にある場合は `n-1` 個より少なくなります。`sampled < n` なら `None` です。結果は近似であり、CLI は「サンプル 100,000 件からの推定」と表示します（FR-64）。

提案値をそのまま `--bounds` に渡せるよう、正規形の文字列表現（03 の表）で出力します。

### 設計判断: 単一パスとリザーバサンプリング

要件書の決定事項のとおりです。等件数区間の境界を正確に出すには t-digest などの分位点スケッチが必要ですが、用途が「区切り位置の当たりを付ける」ことなので、10 万件のサンプルで十分です。サンプル数と乱数シードを設定可能にしてあるので、再現性が必要なテストでは固定できます。
