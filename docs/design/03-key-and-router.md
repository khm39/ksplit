# 03. キーの抽出・型変換と区間判定

## 結論

- キー列は「生フィールド → unquote → デコード → trim → `KeyType::parse`」の順で `KeyValue` に変換し、どの段階で失敗しても `invalid` に分類します
- `KeyValue` は同じ型同士でのみ順序を持ち、`Router` が構築時に型の一致を保証します
- `Router` はソート済み区間に対する `partition_point` による二分探索で、区間の重複・型不一致・名前衝突は構築時に検出します

## キーの抽出と型変換

### 列の解決

```rust
pub enum KeySelector {
    Name(String),
    Index(usize),
}

impl KeySelector {
    pub(crate) fn resolve(&self, header: Option<&[String]>) -> Result<usize, ConfigError>;
}
```

- `Name` はヘッダ必須。ヘッダの列名と完全一致（大文字小文字を区別、trim なし）で探し、見つからなければ `ConfigError::KeyColumnNotFound { name, available }`。同名列が複数あれば `ConfigError::AmbiguousKeyColumn`
- `Index` はヘッダありなら `index < header.len()` を検証し、超えていれば `KeyColumnNotFound`（FR-23）。ヘッダなしなら検証せず、各レコードで列が足りなければその行を invalid にします（FR-44）

### 抽出パイプライン

```rust
pub(crate) struct KeyExtractor {
    column: usize,
    key_type: KeyType,
    trim: bool,
    decoder: Decoder,
    unquoted: Vec<u8>,
    decoded: String,
}

pub(crate) enum KeyOutcome {
    Valid(KeyValue),
    Invalid(InvalidReason),
}

pub(crate) enum InvalidReason {
    MissingColumn,
    Empty,
    BadEncoding,
    Parse(ParseError),
}

impl KeyExtractor {
    pub(crate) fn extract(&mut self, record: &RawRecord<'_>) -> KeyOutcome;
}
```

`extract` の手順は次のとおりです。

1. `record.field(column)` が `None` なら `MissingColumn`
2. フィールドが最終列で末尾が `\r` なら1バイト除きます（02 の CR の扱い）
3. フィールドが `"` で始まれば unquote します。先頭と末尾の `"` を除き、`""` を `"` に置換して `self.unquoted` に書きます。引用符なしならコピーせず生バイト列をそのまま使います
4. デコードします。UTF-8 なら `std::str::from_utf8`、Shift_JIS なら `encoding_rs::SHIFT_JIS.decode_without_bom_handling_and_without_replacement`。失敗すれば `BadEncoding`（EN-07）。結果が借用で済む場合（UTF-8、または Shift_JIS で ASCII のみ）はコピーしません
5. `trim` が有効なら ASCII 空白（0x20）とタブ（0x09）を前後から除きます（FR-17）。全角空白や改行は除きません
6. 空文字列なら `Empty`
7. `key_type.parse(text)` の結果を返します

trim と unquote は比較用の作業バッファ上で行い、`record.bytes` には触れません（FR-33）。

### KeyType と KeyValue

```rust
pub enum KeyType {
    Int,
    Decimal { scale: u32 },
    Date { format: String },
    DateTime { format: String, default_tz: Option<TimeZoneSpec> },
    String,
}

pub enum TimeZoneSpec {
    Named(chrono_tz::Tz),
    Fixed(chrono::FixedOffset),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyValue {
    Int(i64),
    Decimal(i128),
    Date(chrono::NaiveDate),
    DateTime(chrono::DateTime<chrono::Utc>),
    String(String),
}
```

要件書の `default_tz: Option<Tz>` は、IANA 名（`Asia/Tokyo`）と固定オフセット（`+09:00`）の両方を受けるため `TimeZoneSpec` にしています（README「要件書からの差分」）。

`KeyValue` には `PartialOrd` だけを実装し、異なるバリアント同士は `None` を返します。`Ord` を実装しないのは、型違いの比較をコンパイル時ではなく `Router::new` での検証に寄せているためで、`route` 内では同じ型であることが保証された上で `partial_cmp(...).expect(...)` します。

### 各型の変換規則

| 型 | 受理する形 | 規則 |
| --- | --- | --- |
| int | `-?[0-9]+` に全体一致 | `i64::from_str_radix` 相当。`+`、桁区切り、全角数字、指数、範囲外は `ParseError`（FR-12） |
| decimal | `-?[0-9]+(\.[0-9]+)?` に全体一致 | 整数部と小数部を分け、小数部が `scale` 桁より長ければ `ParseError::ScaleExceeded`、短ければ 0 埋め。`整数部 × 10^scale + 小数部` を `i128` で計算し、オーバーフローは `ParseError`（FR-13） |
| date | `format` に全体一致 | `NaiveDate::parse_from_str(text, format)`。chrono は末尾に未消費の文字が残ると `TooLong` エラーを返すので、全体一致は chrono 側で保証されます（FR-14） |
| datetime | `format` に全体一致 | 下記 |
| string | 任意 | デコード済みの `String` をそのまま保持。比較は `str` の `Ord`（コードポイント順と一致、FR-16） |

datetime は `format` にオフセット指定子が含まれるかで分岐します。

```rust
fn format_has_offset(format: &str) -> bool {
    use chrono::format::{Fixed, Item, StrftimeItems};
    StrftimeItems::new(format).any(|item| matches!(
        item,
        Item::Fixed(Fixed::TimezoneOffset)
            | Item::Fixed(Fixed::TimezoneOffsetColon)
            | Item::Fixed(Fixed::TimezoneOffsetZ)
            | Item::Fixed(Fixed::TimezoneOffsetColonZ)
            | Item::Fixed(Fixed::TimezoneOffsetDoubleColon)
            | Item::Fixed(Fixed::TimezoneOffsetTripleColon)
    ))
}
```

`Fixed::TimezoneOffsetDoubleColon`（`%::z`）と `TimezoneOffsetTripleColon`（`%:::z`）は chrono 0.4.3x で存在する項目です。採用する chrono のバージョンで `Fixed` の列挙子を確認し、存在しないものは外します。この判定が漏れると「オフセット付き書式なのに `default_tz` を要求する」誤動作になるので、08 の datetime テストに `%::z` のケースを含めます。

- オフセット指定子、または RFC 2822 / RFC 3339（`%+`）が含まれる場合は、値自身のオフセットで解析し UTC に変換します
- `%s`（Unix 時刻）が含まれる場合は UTC の瞬間として扱います。日時フィールドが併記されていれば、そのフィールドを `default_tz`（未指定なら UTC）のローカル時刻とみなしてタイムスタンプと照合し、食い違えば invalid にします
- どちらも含まれない場合はローカル時刻として解析し、`default_tz` で解釈します。`default_tz` が `None` なら `ParseError::NoTimeZone`（FR-15）
- date 型では `%s` を受け付けません。タイムゾーンなしには暦日が決まらないため、`datetime` と `--tz` の利用を案内します
- 書式が暦日（および datetime では瞬間）を生成できるかは、固定の基準時刻を書式で整形し同じ経路で読み戻すプローブで判定し、生成できなければ `ConfigError::IncompleteFormat` として処理開始前に止めます
- 判定は `KeyType` 構築時に1回だけ行い、結果を保持します。書式文字列自体が不正（未知の指定子）なら `ConfigError::InvalidFormat` として処理開始前に止めます。`StrftimeItems` は不正な指定子を `Item::Error` として返すので、構築時に全項目を走査して検出します

IANA タイムゾーンでローカル時刻を解釈するとき、夏時間の切り替えで曖昧または存在しない時刻になることがあります。`Tz::from_local_datetime` の結果が `Ambiguous` なら早い方の時刻、`None`（存在しない時刻）なら `ParseError::NonexistentLocalTime` として invalid にします。日本時間では発生しませんが、挙動を定義しておかないとテストできないためです。

### 設計判断: decimal を i128 のスケール付き整数で持つ

`scale` は最大 38 桁まで受け付けます（`10^38 < 2^127`）。実用上は金額の 0〜4 桁が中心で、f64 を使わないのは要件書の決定どおりです。境界値も同じ `scale` で変換するので、`--bounds 0.5` と入力の `0.50` は同じ `Decimal(50)`（scale 2）になります。

## 区間と Router

### 型

```rust
pub struct Range {
    pub name: String,
    pub lo: Bound,
    pub hi: Bound,
}

pub enum Bound {
    Unbounded,
    Value(KeyValue),
}

pub struct Router {
    ranges: Vec<Range>,        // lo 昇順
    los: Vec<Bound>,           // 二分探索用に lo だけ取り出した配列
}

pub enum Routed {
    Range(usize),              // ranges() のインデックス
    Unmatched,
}
```

### 構築時の検証（FR-23）

`Router::new(key_type, ranges)` は次を順に検査し、最初の違反を `ConfigError` で返します。

1. 区間が 0 個なら `NoRanges`
2. 各 `Bound::Value` のバリアントが `key_type` と一致しなければ `BoundTypeMismatch`
3. `lo < hi` でなければ `EmptyRange`（`lo == hi` の半開区間は空）
4. `lo` 昇順にソートし、隣り合う区間で `prev.hi > next.lo` なら `OverlappingRanges { a, b }`。`Unbounded` の `lo` は最小、`hi` は最大として比較します。`lo: Unbounded` が2つあれば必ず重複になります
5. 区間名をサニタイズ（後述）した結果が衝突すれば `OutputNameCollision`、予約名 `_unmatched`、`_invalid` と一致すれば `ReservedOutputName`

`ranges()` はソート後の順序で返し、`Routed::Range(i)` はこの順序のインデックスです。レポートや出力ファイルの並びもこの順序に統一します。

### 判定

```rust
impl Router {
    pub fn route(&self, key: &KeyValue) -> Routed {
        let idx = self.los.partition_point(|lo| match lo {
            Bound::Unbounded => true,
            Bound::Value(v) => v <= key,
        });
        if idx == 0 {
            return Routed::Unmatched;
        }
        let candidate = &self.ranges[idx - 1];
        match &candidate.hi {
            Bound::Unbounded => Routed::Range(idx - 1),
            Bound::Value(hi) if key < hi => Routed::Range(idx - 1),
            _ => Routed::Unmatched,
        }
    }
}
```

`partition_point` は `lo <= key` を満たす区間の数を返すので、その直前の区間が「key 以下の lo を持つ最後の区間」です。区間は重複しないため、key が入りうる区間はこれ1つだけで、`hi` の判定で隙間（FR-24）を検出します。計算量は区間数 n に対して O(log n) です。

### 境界値からの区間生成（FR-22）

`Range::from_bounds(key_type, values: Vec<KeyValue>) -> Result<Vec<Range>, ConfigError>` は値を昇順にソートし、重複があれば `DuplicateBound` を返し、N 個の値から N+1 個の区間を作ります。

| 区間 | lo | hi | 名前 |
| --- | --- | --- | --- |
| 先頭 | Unbounded | v1 | `lt_<v1>` |
| i 番目 | vi | v(i+1) | `<vi>_<v(i+1)>` |
| 末尾 | vN | Unbounded | `ge_<vN>` |

名前に使う値の表記は「正規形」です。

| 型 | 正規形 | 例 |
| --- | --- | --- |
| int | 10 進 | `-5`、`1000` |
| decimal | 小数点以下 scale 桁 | scale 2 で `0.50` |
| date | `%Y-%m-%d` | `2024-01-01` |
| datetime | RFC 3339 の UTC、`:` を `-` に置換 | `2024-01-01T00-00-00Z` |
| string | そのまま（サニタイズに委ねる） | `tokyo` |

`--range` で名前を明示した場合はこの生成を通らず、指定名をサニタイズだけします。

### 出力名のサニタイズ（FR-31）

`sanitize_output_name(name: &str) -> Result<String, ConfigError>` は次を適用します。

1. `/`、`\`、`:`、`*`、`?`、`"`、`<`、`>`、`|` と制御文字（0x00–0x1F、0x7F）を `_` に置換
2. 先頭と末尾の空白とドットを除去（Windows では末尾のドット・空白が落とされるため）
3. 空になれば `EmptyOutputName`
4. 大文字小文字を無視して Windows 予約語（`CON`、`PRN`、`AUX`、`NUL`、`COM1`〜`COM9`、`LPT1`〜`LPT9`、拡張子付きも含む）に一致すれば先頭に `_` を付与
5. 先頭が `_` の名前は予約名との衝突を避けるため、`_unmatched`、`_invalid` との完全一致だけを拒否し、それ以外は許可

衝突検査（FR-23）は大文字小文字を無視して行います。macOS の既定と Windows のファイルシステムが大文字小文字を区別しないためで、Linux だけで通ってしまうと他環境で出力が上書きされます（NF-06）。

ファイル名は `<サニタイズ後の名前>.csv` で、`_unmatched.csv`、`_invalid.csv` も同じディレクトリ直下に置きます（FR-30、FR-40、FR-42）。
