# 02. CSV レコード読み込み

## 結論

- CSV の構造解析はバイト単位の自前ステートマシン `RecordScanner` で行い、レコードの生バイト列とキー列のフィールド位置だけを返します。外部の CSV パーサは使いません
- レコード終端は引用符外の LF です。直前の CR はレコードの一部としてそのまま出力に含めます（バイト透過）
- 読み込みバッファは 1 MiB から始め、1 レコードがまたがる場合だけ `max_record_bytes` まで成長させます。超えた時点で `RecordTooLarge` で停止します

## 設計判断: 自前スキャナ

`csv` クレート（および下層の `csv_core`）はフィールドを unescape した結果を返すため、FR-33 のバイト透過に必要な「レコードの生バイト列」を別途管理する必要があります。また 1 レコードの長さ上限（FR-06）を持たず、引用符の閉じ忘れでメモリを使い切ります。`csv_core` なら入出力バッファを自前で持てるので実現は可能ですが、フィールドの unescape 済み出力も同時に生成するため、生バイト列と二重管理になります。必要なのは「レコードの終端はどこか」「キー列のフィールドはどこからどこまでか」の2点だけなので、その2点に特化したスキャナを自前で持つ方が単純で速く、上限制御も自然に入ります。`csv` クレートはテストのオラクルとして使います（08-testing）。

## RecordScanner

### 入力と出力

```rust
pub(crate) struct RecordScanner {
    delimiter: u8,
    state: State,
    field_starts: Vec<u32>,   // 現在のレコード内のフィールド開始オフセット（レコード先頭からの相対）
}

pub(crate) enum ScanResult {
    /// レコード終端を検出。`len` はレコード先頭からの長さ（終端の LF を含む）
    Record { len: usize },
    /// バッファ末尾まで読んだがレコードが終わっていない
    NeedMore,
    /// 引用符で閉じたフィールドの直後に区切り・改行以外のバイトがある
    Malformed { offset: usize },
}

impl RecordScanner {
    pub(crate) fn scan(&mut self, buf: &[u8], from: usize) -> ScanResult;
    pub(crate) fn fields(&self) -> &[u32];
    pub(crate) fn in_quotes(&self) -> bool;
    pub(crate) fn reset(&mut self);
}
```

`scan` は `buf[from..]` を走査し、レコード先頭を `buf[0]` とみなします。呼び出し側（`CsvSource`）は `NeedMore` のときバッファを補充して、続きの位置から再度 `scan` を呼びます。スキャナは状態（引用符の内外、フィールド境界）を保持しているので、バッファ補充をまたいでも先頭から走査し直しません。

### 状態遷移

```
状態            バイト          遷移
Unquoted      delimiter       フィールド境界を記録、Unquoted
Unquoted      '"' (フィールド先頭) Quoted
Unquoted      '"' (フィールド途中) リテラルとして扱う、Unquoted
Unquoted      LF              Record
Unquoted      その他          Unquoted
Quoted        '"'             QuoteSeen
Quoted        その他          Quoted（LF も含む。引用符内改行）
QuoteSeen     '"'             エスケープされた引用符、Quoted
QuoteSeen     delimiter       フィールド境界を記録、Unquoted
QuoteSeen     LF              Record
QuoteSeen     CR              CrAfterQuote（次が LF なら Record、それ以外は Malformed）
QuoteSeen     その他          Malformed
```

RFC 4180 の厳密解釈より1点だけ緩めています。引用符がフィールド途中に現れた場合（`a"b`）はリテラルとして受理します。`csv` クレートの既定と同じで、実データで頻出し、拒否すると使い物にならないためです。引用符で閉じたフィールドの直後に余計なバイトがある場合（`"a"b`）は FR-44 に従い `Malformed` で停止します。

### 走査の実装

`Unquoted` 状態では `memchr::memchr3(delimiter, b'"', b'\n', &buf[pos..])` で次の注目バイトまで飛び、`Quoted` 状態では `memchr::memchr(b'"', ...)` で飛びます。通常の CSV ではバイトの大半がこれらの間にあるため、1バイトずつの比較はほとんど発生しません。

CR は注目バイトにしません。CRLF の CR はレコードバイト列に含まれたまま出力されるので（バイト透過）、特別扱いする必要がないためです。唯一の影響はキー列が最終フィールドのときに値の末尾に `\r` が付くことで、これはキー抽出側（03）で末尾の `\r` を1つ除いてから処理します。

### 設計判断: CR 単独の終端は非対応

レコード終端を LF に限定すると、CR のみで改行する古い Mac 形式のファイルは全体が1レコードと解釈され、`max_record_bytes` で停止します。現在の生成元（Excel、各種 DB のエクスポート）はすべて LF か CRLF を出力するため対応しません。停止時のメッセージには「改行コードが CR のみの可能性」を含め、`inspect` の改行コード検出でも `Cr` を報告できるようにします。

## CsvSource

`RecordSource` トレイトの CSV 実装です。トレイトは要件書「将来の拡張点」の形のまま `pub(crate)` に留めます。

```rust
pub(crate) trait RecordSource {
    fn next_record(&mut self) -> Result<Option<RawRecord<'_>>, Error>;
    fn position(&self) -> u64;
    fn seek(&mut self, offset: u64) -> Result<(), Error>;
    fn header(&self) -> Option<&[u8]>;
    fn total_bytes(&self) -> Option<u64>;
}

pub(crate) struct RawRecord<'a> {
    pub bytes: &'a [u8],         // 終端の LF（と直前の CR）を含む生バイト列
    pub fields: &'a [u32],       // 各フィールドの開始オフセット
    pub number: u64,             // ヘッダを除く 1 始まりのレコード番号
    pub offset: u64,             // 入力先頭からの絶対バイトオフセット
}

impl<'a> RawRecord<'a> {
    /// キー列のフィールド（引用符を含む生バイト列）。列が無ければ None
    pub(crate) fn field(&self, index: usize) -> Option<&'a [u8]>;
}
```

要件書の `RawRecord { bytes, key }` から `fields` 全体を持つ形に変えています。`inspect` が全列のサンプルを必要とし（FR-60）、キー列の抽出もインデックス指定で統一できるためです。フィールド数は通常数十なので、開始オフセットの `Vec<u32>` を保持するコストは無視できます。

### 読み込みバッファ

```
buf: Vec<u8>（容量 1 MiB から開始）
[ 消費済み | 現在のレコード（途中） | 未走査 ]
  ^start    ^rec_start               ^scanned   ^filled
```

- `next_record` は `rec_start` から走査を始め、`Record { len }` が返れば `&buf[rec_start..rec_start+len]` を返し、`rec_start` を進めます
- `NeedMore` のときは、`rec_start > 0` なら残りを先頭へ `copy_within` で詰めてから `read` で補充します。`rec_start == 0` でバッファが満杯なら容量を2倍にします（上限は `max_record_bytes`）
- 現在のレコード長が `max_record_bytes` を超えた時点で `Error::Data { kind: RecordTooLarge { unclosed_quote: scanner.in_quotes() } }` を返します

補充のために `copy_within` が動くのは「レコードがバッファ末尾にまたがったとき」だけで、1 MiB のバッファなら平均レコード長 200 バイトに対して 5,000 レコードに1回です。コピー量は高々1レコード分なので、総コピー量は入力サイズに対して無視できます。

### EOF の扱い

最後のレコードが LF で終わらない場合、`read` が 0 を返した時点で `Unquoted` または `QuoteSeen` 状態なら、残りのバイト列をレコードとして返します（終端なしのまま出力に書きます。バイト透過のため改行を補いません）。`Quoted` 状態なら引用符の閉じ忘れとして `Malformed` を返します。空ファイル、またはヘッダのみのファイルは正常（レコード 0 件）です。

## BOM とヘッダ

`CsvSource::open` は次の順で先頭を処理します。

1. 先頭 3 バイトが `EF BB BF` なら BOM ありとして記録し、ヘッダ列名の解釈から除外します（EN-03）。Shift_JIS 指定時に BOM があればエラーにはせず、そのまま BOM として扱います（UTF-8 ファイルを誤って Shift_JIS 指定した可能性が高いので、警告を返します）
2. `has_header` なら最初のレコードをヘッダとして読み、`header()` で返せるよう保持します。保持するバイト列には BOM を含めます（出力ファイル先頭にそのまま複製するため、FR-32）
3. ヘッダの各フィールドを unquote しデコードして列名リストを作ります。列名の解決と列数の検証（FR-23）は `KeySelector::resolve(&header_names)` が行います（03）

`has_header == false` のときヘッダはなく、`header()` は `None` です。出力ファイルにはヘッダを書かず、BOM があれば BOM だけを書きます。

## 位置と seek

`position()` は「次に返すレコードの開始オフセット」です。`next_record` が返したレコードの `offset + bytes.len()` と常に一致します。チェックポイントにはこの値を記録します（RS-03）。レコード境界以外の値になることはありません。

`seek(offset)` は `Input::Path` のときだけ有効で、ファイルを `SeekFrom::Start(offset)` に移動し、バッファとスキャナ状態を破棄します。`Input::Reader` では `ConfigError::ResumeRequiresPath` を返します（RS-10）。再開時はまずファイル先頭から BOM とヘッダを通常どおり読み（列の解決と検証のため）、その後 `seek` でチェックポイント位置へ移ります。このためチェックポイントの入力オフセットは常に先頭データレコード以降の値であり、RS-01 の「オフセット 0」は「先頭データレコードの開始位置」と読み替えます（README「要件書からの差分」）。

`total_bytes()` は `Input::Path` のときファイルサイズ、`Input::Reader` のとき `None` です（FR-52）。

## エンコーディングとバイト単位解析

スキャナが注目するバイトは区切り文字、`"`（0x22）、LF（0x0A）の3種です。Shift_JIS（CP932）の2バイト目は 0x40–0x7E と 0x80–0xFC の範囲で、0x22 と 0x0A は含まれません。区切り文字が 0x40 未満（カンマ 0x2C、タブ 0x09、セミコロン 0x3B）であれば2バイト目と衝突しないため、バイト単位の走査が安全です（EN-04）。

区切り文字が 0x40 以上で入力が Shift_JIS の場合は `SplitConfig::validate()` が `ConfigError::DelimiterUnsafeForEncoding` を返します（EN-05）。UTF-8 では継続バイトが 0x80 以上なので、ASCII 範囲の区切り文字なら衝突しません。区切り文字は ASCII（0x00–0x7F）の1バイトに限定し、LF、CR、`"` は区切り文字として拒否します。

バックスラッシュは特別扱いしません（FR-05）。Shift_JIS の「表」「能」などの2バイト目 0x5C がエスケープと誤認される問題は、設計上発生しません。
