# ksplit

メモリに乗らない大量の CSV を、指定したキー列の値の範囲で複数ファイルに分割するライブラリ兼 CLI です。10GB 規模の入力を単一パスで処理し、中断しても `--resume` で続きから再開できます。

- 入力は UTF-8（BOM 可）または Shift_JIS（CP932）。行はバイト列のまま出力するので、引用符・改行コード・空白は変わりません
- キーの型は int / decimal / date / datetime / string を明示指定。区間は半開 `[lo, hi)`
- どの区間にも入らない行は `_unmatched.csv`、キーが解釈できない行は `_invalid.csv` に書き、黙って捨てません
- 要件は [docs/requirements.md](docs/requirements.md)、設計は [docs/design/](docs/design/README.md) にあります

## インストール

```bash
cargo install --path .
```

## 使い方

```bash
# 1. 下見: 先頭だけ読んで列名・サンプル値・型の候補を見る
ksplit inspect orders.csv

# 2. キーを指定して全件を読み、分布を見る
ksplit inspect orders.csv --key order_date --type date --format %Y-%m-%d --bins 12

# 3. 区間案を足して試算する（ファイルは書かない）
ksplit inspect orders.csv --key order_date --type date --format %Y-%m-%d \
  --bounds 2024-01-01,2024-04-01,2024-07-01,2024-10-01

# 4. inspect を split に変えて -o を足せばそのまま実行できる
ksplit split orders.csv -o out/ --key order_date --type date --format %Y-%m-%d \
  --bounds 2024-01-01,2024-04-01,2024-07-01,2024-10-01

# Shift_JIS の売上データから金額 1 万円以上だけ抽出する
ksplit split sales.csv -o out/ --encoding shift_jis --key 金額 --type decimal --scale 0 \
  --range high=10000.. --mode extract

# 中断した処理の状態確認と再開（引数は前回と同じにする）
ksplit status out/
ksplit split orders.csv -o out/ --key order_date --type date --format %Y-%m-%d \
  --bounds 2024-01-01,2024-04-01,2024-07-01,2024-10-01 --resume
```

`--json` を付けると結果だけを標準出力に JSON で書き、進捗や警告は標準エラー出力に出ます。

### 終了コード

| コード | 意味 |
| --- | --- |
| 0 | 正常終了 |
| 1 | 引数・設定のエラー |
| 2 | 入力データのエラー（CSV 解析失敗、レコード長上限超過、`--strict` での invalid 検出、invalid ガード） |
| 3 | I/O エラー |
| 4 | 再開の拒否（設定不一致、入力の変更、チェックポイント破損） |
| 5 | Ctrl+C による中断（split ではチェックポイント書き込み済み） |

## ライブラリとして使う

```toml
[dependencies]
ksplit = { version = "0.1", default-features = false }
```

```rust
use ksplit::{CancelToken, CsvDialect, Encoding, ExistingOutput, Input, KeySelector, KeySpec, KeyType, KeyValue, Mode, OnInvalid, SplitConfig};

let config = SplitConfig {
    input: Input::Path("orders.csv".into()),
    output_dir: "out".into(),
    dialect: CsvDialect::default(),
    encoding: Encoding::Utf8,
    key: KeySpec { selector: KeySelector::Name("id".into()), key_type: KeyType::Int, trim: true, invalid_guard: None },
    ranges: ksplit::ranges_from_bounds(vec![KeyValue::Int(1000), KeyValue::Int(10000)])?,
    mode: Mode::Split,
    on_invalid: OnInvalid::WriteToFile,
    existing: ExistingOutput::Fail,
    checkpoint: Some(Default::default()),
    write_buffer_bytes: 64 * 1024,
    progress_interval_bytes: 16 << 20,
};
let report = ksplit::split(config, &CancelToken::new(), |p| eprintln!("{} bytes", p.bytes_read))?;
# Ok::<(), ksplit::Error>(())
```

`inspect` / `profile` / `plan` / `plan_with_profile` / `read_checkpoint` も同じ形で呼べます。すべて同期 API で、GUI からはバックグラウンドスレッドで呼び、`CancelToken` で止めます。

## 性能（参考値）

`scripts/gen_csv.py` で生成した 10GB（2 億 3,270 万レコード、5% が引用符内改行を含む）を、4 コア・15GB メモリの Linux 仮想マシンで処理した結果です。

| 処理 | 所要時間 | スループット | 備考 |
| --- | --- | --- | --- |
| `split`（date キー、6 区間） | 44 秒 | 243 MB/s | 出力 10GB を含む。ピーク常駐メモリ約 2.3MB |
| `inspect --bounds`（date キー、書き込みなし） | 28 秒 | 379 MB/s | |
| `inspect --bounds`（int キー、書き込みなし） | 28 秒 | 386 MB/s | |
| 35% 地点で Ctrl+C → `--resume` | 37 秒（再開分） | | 出力は一気通しの実行とバイト単位で一致 |

## 開発

```bash
cargo test
cargo clippy --all-targets
scripts/gen_csv.py big.csv 10737418240   # 性能・再開テスト用の 10GB CSV を生成
```
