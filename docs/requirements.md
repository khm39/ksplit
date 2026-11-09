# ksplit 要件定義書

作成日: 2026-11-09 · 作成者: Koki

状態: Draft（レビュー指摘反映済み、実装着手前）

## 概要と目的

ksplit は、メモリに乗らない大量の CSV を、指定した1列（キー）の値の範囲に基づいて複数ファイルに分割するライブラリ兼 CLI ツールです。10GB 規模の入力を単一パスのストリーミングで処理し、途中で中断しても続きから再開できます。

解決する課題は次の3点です。

- 既存の `split` コマンドは行数やバイト数でしか分けられず、「2024年1月分だけ」「ID が 10000 未満」のような意味のある単位で分けられない
- 汎用ツール（pandas、DuckDB など）は10GB の CSV をそのまま扱うにはメモリや前処理の手間がかかる
- 長時間処理が途中で失敗した場合、最初からやり直すしかない

名前の `k` は key を表し、CSV 以外のフォーマット（JSONL など）への拡張を妨げません。実装言語は Rust、配布形態はライブラリクレート `ksplit` と同名の CLI バイナリです。

## スコープ

初期スコープは「CSV の範囲分割を、再開付きで正しく行う」ことに絞ります。後から足せる機能は外し、後から直すと全面書き直しになるもの（引用符内改行、エンコーディング、再開）は最初から含めます。

| 項目 | 初期スコープ | 備考 |
| --- | --- | --- |
| 入力フォーマット | CSV のみ | JSONL/JSON は将来課題。拡張点だけ用意する |
| 入力元 | ローカルファイル、標準入力 | 標準入力では再開不可 |
| 入力エンコーディング | UTF-8（BOM 有無）、Shift_JIS（CP932） | 明示指定。自動判定はしない |
| 分割方式 | 単一キーの値の範囲（半開区間） | キー値ごとの分割、ハッシュ分割は含まない |
| キーの型 | 整数、固定小数、日付、日時、文字列 | 型は明示指定 |
| モード | 分割（split）、抽出（extract） |  |
| 解析・補助機能 | inspect（下見）、profile（キー列の分布）、plan（区間の試算）、status（中断状態の確認） | ライブラリでは別関数、CLI では inspect と status の2サブコマンドに集約。将来の GUI の基盤 |
| キャンセル | 全ての長時間処理で対応 | CLI では SIGINT、ライブラリでは CancelToken |
| 出力 | 区間ごとに1ファイル、入力と同じエンコーディング | 出力エンコーディング変換は将来課題 |
| 再開 | 非圧縮のローカルファイル入力に対応 | 圧縮入力の再開は将来課題 |
| 圧縮 | 非対応 | gzip/zstd は将来課題 |
| 複合キー | 非対応 | 辞書式順序の区間定義が必要になり複雑化するため外す |
| 出力ファイル数 | FD 上限を超えない範囲 | LRU によるファイル管理は将来課題 |
| GUI | 非対応 | ライブラリ API は GUI からの利用を前提に設計するが、GUI 自体は別プロジェクト |

## 用語定義

| 用語 | 定義 |
| --- | --- |
| キー | 分割の基準にする CSV の1列。列名または 0 始まりの列番号で指定する |
| キー型 | キー値を比較するための型。int / decimal / date / datetime / string のいずれか |
| 範囲（区間） | 半開区間 `[lo, hi)`。lo を含み hi を含まない。lo または hi は無限大でもよい |
| 出力名 | 区間に対応する出力ファイルのベース名 |
| split モード | 全行をいずれかの出力に振り分ける。どの区間にも入らない行は unmatched に書く |
| extract モード | 区間に入る行だけを出力する。どの区間にも入らない行は捨てる |
| unmatched | キー値は正常に解釈できたが、どの区間にも入らない行 |
| invalid | キー列が存在しない、空、またはキー型に変換できない行 |
| レコード | CSV の1行。引用符内の改行を含む場合があり、物理行とは一致しない |
| チェックポイント | 再開のために定期的に保存する進捗情報（入力オフセットと各出力のバイト長） |
| バイト透過 | 入力レコードのバイト列を再シリアライズせずそのまま出力に書くこと |

## 機能要件

機能要件は FR-番号で識別します。「必須」は初期リリースに含める要件です。

### 入力

| ID | 要件 | 優先度 |
| --- | --- | --- |
| FR-01 | ローカルファイルパスまたは標準入力から CSV を読み込む | 必須 |
| FR-02 | RFC 4180 準拠の CSV を解析する。引用符内の改行、引用符の二重化によるエスケープを正しく扱う | 必須 |
| FR-03 | 区切り文字を指定できる（既定はカンマ、タブ・セミコロンなど ASCII 1文字） | 必須 |
| FR-04 | ヘッダ行の有無を指定できる。既定はヘッダあり | 必須 |
| FR-05 | バックスラッシュによるエスケープはサポートしない（Shift_JIS の2バイト目 0x5C と衝突するため） | 必須 |
| FR-06 | 1 レコードの長さに上限を設ける（既定 16 MiB、オプションで変更可）。上限を超えた時点で、レコード番号、開始バイトオフセット、「引用符が閉じられていない可能性」を示して停止する（終了コード 2）。閉じ忘れた引用符がファイル末尾までを1レコードにしてメモリを使い切るのを防ぐ | 必須 |

### キー指定と型

| ID | 要件 | 優先度 |
| --- | --- | --- |
| FR-10 | キーを列名（ヘッダありのとき）または 0 始まりの列番号で指定できる | 必須 |
| FR-11 | キー型を int / decimal / date / datetime / string から明示指定する。型の自動推論は行わない | 必須 |
| FR-12 | int は `-?[0-9]+` に全体一致する ASCII 文字列を 64 ビット符号付き整数として解釈する。桁区切り、全角数字、指数表記、`+` 符号、範囲外の値は受理せず invalid とする。空白の扱いは FR-17 に従う | 必須 |
| FR-13 | decimal はスケール（小数部桁数）を指定し、`-?[0-9]+(\.[0-9]+)?` に全体一致する値を固定小数として比較する。浮動小数点数には変換しない。小数部がスケールより長い値は丸めず invalid とする。短い値は 0 で埋める。桁区切り、全角数字、指数表記は受理しない | 必須 |
| FR-14 | date / datetime は strftime 形式の書式指定を必須とする。書式は値の全体に一致しなければならず、書式より後ろに文字が残る値は invalid とする | 必須 |
| FR-15 | datetime は書式にオフセット指定子（`%z` 、`%:z` など）が含まれれば値のオフセットを使い、含まれなければ指定された既定タイムゾーンで解釈し、UTC の瞬間として比較する。既定タイムゾーンが未指定でオフセット無しの値が来た場合は invalid とする | 必須 |
| FR-16 | string はデコード後の Unicode コードポイント順で比較する。照合順序（ロケール依存の並び順）は使わない | 必須 |
| FR-17 | キー値の前後の空白（ASCII 空白、タブ）を trim してから型変換する。既定で有効、オプションで無効化できる。大文字小文字の統一は行わない。trim は比較のためだけに行い、出力行には影響しない（FR-33 のバイト透過を維持する） | 必須 |

### 範囲指定

| ID | 要件 | 優先度 |
| --- | --- | --- |
| FR-20 | 区間はすべて半開区間 `[lo, hi)` とする。閉区間は提供しない | 必須 |
| FR-21 | 明示的な区間の列挙（名前、下限、上限）で指定できる。下限・上限は省略でき、省略時は無限大とする | 必須 |
| FR-22 | 境界値の列挙で指定できる。N 個の境界値から N+1 個の区間（両端は無限大）を生成し、出力名は境界値から自動生成する | 必須 |
| FR-23 | 次の場合は処理開始前にエラーで停止する: 区間が重複する、境界値がキー型に変換できない、サニタイズ後の出力名が衝突する、出力名が予約名（`_unmatched`、`_invalid`）と一致する、キー列が見つからない（列名がヘッダにない、列番号がヘッダ行の列数以上） | 必須 |
| FR-24 | 区間の間に隙間があってもよい。隙間に落ちた行は unmatched として扱う | 必須 |
| FR-25 | 開始点と幅からの等幅区間生成は初期スコープに含めない。--range と --bounds で代替する | 対象外 |

### 出力

| ID | 要件 | 優先度 |
| --- | --- | --- |
| FR-30 | 区間ごとに1ファイルを出力ディレクトリ直下に生成する。出力ディレクトリが存在しなければ作成する。ファイル名は `<出力名>.csv` | 必須 |
| FR-31 | 出力名はファイルシステムで安全な文字にサニタイズする（`/`、``、`:`、制御文字、Windows 予約語） | 必須 |
| FR-32 | ヘッダありの場合、各出力ファイルの先頭に入力のヘッダ行をバイト列のまま複製する。入力に BOM があれば BOM も複製する | 必須 |
| FR-33 | データ行は入力のバイト列をそのまま書く（バイト透過）。引用符、空白、改行コードを変更しない | 必須 |
| FR-34 | 入力行の順序を各出力ファイル内で保持する | 必須 |
| FR-35 | 1行も該当しなかった区間のファイルも（ヘッダのみで）生成する。後続処理がファイルの存在を前提にできるようにする | 必須 |
| FR-36 | 出力ディレクトリに既存の出力ファイルがある場合、再開指定がなければエラーで停止する。上書きは明示フラグでのみ許可する | 必須 |

### モードと不正行の扱い

| ID | 要件 | 優先度 |
| --- | --- | --- |
| FR-40 | split モード（既定）では、どの区間にも入らない行を `_unmatched.csv` に書く | 必須 |
| FR-41 | extract モードでは、どの区間にも入らない行を捨て、`_unmatched.csv` を生成しない | 必須 |
| FR-42 | キー列が存在しない、空、型変換に失敗した行は両モードとも `_invalid.csv` に書く | 必須 |
| FR-43 | strict オプション指定時は、invalid 行を検出した時点で行番号とキー値を示して停止する | 必須 |
| FR-44 | CSV として解析できない行（引用符の不整合など）は常にエラーで停止する。列数の不一致は許容し、キー列が欠けていれば invalid とする | 必須 |
| FR-45 | invalid ガード: profile / plan / split は、先頭の N レコード（既定 10,000、ヘッダを除く）がすべて invalid だった時点で、キー指定または型指定の誤りの可能性を示して停止する（終了コード 2）。N は変更でき、オプションで無効化できる。入力が N レコード未満の場合は判定しない。strict とは独立（strict は 1 行目で止まる） | 必須 |

### レポート

| ID | 要件 | 優先度 |
| --- | --- | --- |
| FR-50 | 処理終了時に、区間ごとの行数、unmatched 行数、invalid 行数、読み込みバイト数、処理時間を標準エラー出力に表示する | 必須 |
| FR-51 | 同じ内容を JSON ファイルに保存できる。区間ごとに出力パスを含める。JSON には `schema_version` を含め、構造を変えるときは値を上げる（`--json` の出力も同様） | 必須 |
| FR-52 | 処理中に進捗（読み込みバイト数 / 全体バイト数、行数）を標準エラー出力に表示する。標準入力の場合は全体バイト数を省略する | 必須 |

### 解析・補助機能

分割を実行する前にデータを調べ、区間案を試算するための機能です。ライブラリでは inspect / profile / plan / read_checkpoint の4関数、CLI では inspect（引数に応じて profile / plan を兼ねる）と status の2サブコマンドで提供し、CLI は `--json` で機械可読な出力を返します。

| ID | 要件 | 優先度 |
| --- | --- | --- |
| FR-60 | inspect: 入力の先頭を読み、BOM の有無、検出した改行コード、列数、ヘッダの列名一覧、各列の先頭 N 件のサンプル値を返す。読み込み量はサンプル件数で決まり、ファイル全体は読まない | 必須 |
| FR-61 | inspect: 各列のサンプル値が int / decimal / date（代表的な書式）に全件変換できるかを判定し、型の候補として返す。候補は提案であり、split の型指定は引き続き明示とする（FR-11） | 任意 |
| FR-62 | profile: キー列と型を指定して全件を1パス読み、総行数、invalid 行数、最小値、最大値を正確に返す | 必須 |
| FR-63 | profile: キー値の分布をヒストグラム（ビン数指定、既定 20）で返す。ヒストグラムはリザーバサンプリング（既定 100,000 件）に基づく近似であることを結果に明記する。string 型ではヒストグラムを生成しない | 必須 |
| FR-64 | profile: 指定した区間数 N に対して、件数がおおよそ均等になる境界値をサンプルから提案する。近似であることを明記する | 任意 |
| FR-65 | plan: 区間を指定して全件を1パス読み、区間ごとの行数、unmatched 行数、invalid 行数を返す。ファイルは書かない。split と同じ引数を受け付け、同じ判定ロジックを使う | 必須 |
| FR-66 | plan: 結果は split のレポート（FR-50）と同じ構造で返す。同じ入力と設定で plan と split の件数が一致することをテストで保証する | 必須 |
| FR-67 | status: 出力ディレクトリのチェックポイントを読み、中断した処理の設定、進捗（バイト数・行数）、区間ごとの行数、入力ファイルが変更されていないかを返す。チェックポイントがなければその旨を返す | 必須 |

### 実行制御

| ID | 要件 | 優先度 |
| --- | --- | --- |
| FR-70 | profile / plan / split はキャンセルできる。ライブラリでは CancelToken、CLI では SIGINT（Ctrl+C）で要求する | 必須 |
| FR-71 | split のキャンセル時は、その時点でチェックポイントを書いてから終了する。その後 `--resume` で再開できる | 必須 |
| FR-72 | SIGINT を2回受けた場合はチェックポイントを書かず即時終了する。直前のチェックポイントからの再開は可能 | 必須 |
| FR-73 | profile / plan / split は進捗（読み込みバイト数、全体バイト数、行数）をコールバックで通知する。通知間隔はバイト数で指定でき、既定は 16 MiB | 必須 |

## エンコーディング要件

エンコーディングは明示指定とし、行はバイト列のまま出力し、デコードはキー列に限定します。これにより変換によるデータ化けと性能劣化を同時に避けます。

| ID | 要件 | 優先度 |
| --- | --- | --- |
| EN-01 | 入力エンコーディングは `utf-8` または `shift_jis` を明示指定する。既定は `utf-8`。自動判定は行わない | 必須 |
| EN-02 | `shift_jis` は CP932（Windows-31J）として扱う。機種依存文字（①、㈱、～ など）をデコードできること | 必須 |
| EN-03 | UTF-8 の BOM を先頭で検出し、ヘッダ列名の解釈から除外する。出力には入力と同じく BOM を付与する | 必須 |
| EN-04 | CSV の構造解析（区切り、引用符、改行）はバイト単位で行う。Shift_JIS の2バイト目（0x40–0x7E、0x80–0xFC）は区切り文字・引用符・改行と衝突しないため安全 | 必須 |
| EN-05 | 区切り文字に 0x40 以上のバイト（例: 縦棒 0x7C）が指定され、かつ入力が Shift_JIS の場合はエラーで停止する（2バイト目と衝突するため） | 必須 |
| EN-06 | split / plan / profile でのデコードはキー列の値とヘッダ行（列名の解決用）に限定する。他の列はデコードしない。inspect のサンプル値は全列をデコードする | 必須 |
| EN-07 | キー値のデコードに失敗した行（不正なバイト列）は invalid として扱う | 必須 |
| EN-08 | string 型の境界値はコマンドラインから UTF-8 で受け取り、入力のエンコーディングに関わらず Unicode で比較する | 必須 |
| EN-09 | 出力エンコーディングの変換（Shift_JIS → UTF-8 など）は行わない。出力は常に入力と同じエンコーディング | 必須（制約） |

## 再開（チェックポイント）要件

再開は「チェックポイント時点の状態に出力を巻き戻してから続行する」方式で実現します。チェックポイント以降に書かれた中途半端なデータは truncate で消えるため、重複も欠落も生じません。

### チェックポイントの記録

| ID | 要件 | 優先度 |
| --- | --- | --- |
| RS-01 | チェックポイントファイル `.ksplit-checkpoint.json` を出力ディレクトリ直下に置く。出力ファイルを作成した直後、最初のレコードを書く前に、入力オフセット 0 のチェックポイントを書く（「出力はあるがチェックポイントがない」状態を作らないため） | 必須 |
| RS-02 | 記録項目: ツールのバージョン、設定ハッシュ、入力ファイルの絶対パス・サイズ・mtime、入力のバイトオフセット、処理済みレコード数、各出力ファイル（`_unmatched.csv`、`_invalid.csv` を含む）のバイト長、区間ごとの行数、unmatched 行数、invalid 行数。設定ハッシュの対象は出力内容を左右する項目のみ（キー指定と型、trim、区間、モード、invalid の扱い、区切り文字、ヘッダ有無、エンコーディング、レコード長上限）とし、バッファサイズ・チェックポイント間隔・進捗間隔・レポート出力先は含めない。入力の同一性はサイズと mtime だけで判定する。同サイズ・同 mtime の別内容に差し替わるケースは検知できないことを承知の上で受容する（想定する利用場面では起きないと判断） | 必須 |
| RS-03 | 入力オフセットは必ずレコード境界とする。CSV パーサが報告する位置を使い、物理行のカウントは使わない | 必須 |
| RS-04 | チェックポイントの保存前に、全出力ファイルを flush し fsync する。チェックポイントファイル自体は一時ファイルに書いて fsync し、rename で置き換え、その後出力ディレクトリを fsync する（rename の永続化に必要）。rename の原子性は Linux / macOS で保証し、Windows ではベストエフォートとする（NF-06） | 必須 |
| RS-05 | 保存間隔は読み込みバイト数で指定する。既定は 256 MiB | 必須 |
| RS-06 | 正常終了時にチェックポイントファイルを削除する | 必須 |

### 再開手順

再開フラグが指定され、チェックポイントファイルが存在する場合、次の順で処理します。

1. チェックポイントのツールバージョンと設定ハッシュを現在の設定と比較し、不一致ならエラーで停止する
2. 入力ファイルのサイズと mtime を比較し、不一致ならエラーで停止する
3. 各出力ファイル（`_unmatched.csv`、`_invalid.csv` を含む）が記録されたバイト長以上であることを確認し、記録長で truncate する。記録長未満ならエラーで停止する
4. 入力を記録オフセットまで seek し、区間ごとの行数・unmatched 行数・invalid 行数・処理済みレコード数を復元して処理を続行する

### 制約と拒否条件

| ID | 要件 | 優先度 |
| --- | --- | --- |
| RS-10 | 再開は入力がローカルファイルパスの場合のみ有効。標準入力と再開フラグの組み合わせはエラー | 必須 |
| RS-11 | 設定不一致や入力変更を無視して再開する抑えフラグは提供しない。やり直す場合は上書きフラグで最初から実行する | 必須 |
| RS-12 | チェックポイントファイルが存在するのに再開フラグも上書きフラグもない場合は、前回が中断している旨を示してエラーで停止する | 必須 |
| RS-13 | チェックポイントファイルがなく、出力ディレクトリに出力ファイルもない状態で再開フラグが指定された場合は、警告を出して最初から実行する。チェックポイントファイルがなく出力ファイルがある場合は、前回が最初のチェックポイント前に中断したか別の実行の出力である可能性を示し、上書きフラグを促してエラーで停止する（終了コード 4）。再開フラグが既存出力を上書きすることはない（FR-36） | 必須 |

## 非機能要件

| ID | 要件 | 備考 |
| --- | --- | --- |
| NF-01 | メモリ使用量は入力サイズに依存しない。上限の目安は「読み込みバッファ + 区間数 × 書き込みバッファ + レコード長上限（FR-06） + profile のリザーバサンプル」 | 10GB 入力を数十 MB で処理できること |
| NF-02 | 入力を1回だけ読む（単一パス）。事前のスキャンやソートを行わない | 等幅区間の開始点自動探索を外した理由 |
| NF-03 | スループットの目標はローカル SSD で 200 MB/s 以上（10GB を約 1 分） | ディスク I/O が律速になる水準。それを下回るならパーサやバッファを見直す |
| NF-04 | 出力は決定的。同じ入力と設定なら、再開の有無に関わらずバイト単位で同一の出力を生成する | 再開の正しさをテストで検証できる根拠 |
| NF-05 | エラー停止時、最後のチェックポイントまでの出力は必ずディスクに残っている | RS-04 の fsync が保証する |
| NF-06 | 動作保証の対象は Linux と macOS。Windows はビルドでき動作を目指すがベストエフォートとし、CI での検証と rename の原子性は保証しない。ファイル名のサニタイズは3環境で有効な名前を生成する | Windows の予約語と `:` が主な制約 |
| NF-07 | ライブラリは標準出力・標準エラー出力に直接書かない。進捗とログはコールバック経由で CLI 側が扱う | ライブラリとして組み込める条件 |
| NF-08 | エラーは発生箇所（レコード番号、バイトオフセット、キー値）を含む | 10GB の中から問題行を探せるように |
| NF-09 | 出力ファイルごとの書き込みバッファは既定 64 KiB。オプションで変更できる。区間数 × バッファが 1 GiB を超える場合は処理開始前に警告を出す | 1,000 区間で 64 MiB。警告はエラーにしない |
| NF-10 | テストは手書きの小規模フィクスチャと生成スクリプトの2層で構成する。フィクスチャは境界値ちょうどの行、引用符内改行、BOM 付き、Shift_JIS の 0x5C を含む文字、空白付きキー値、欠損キーを網羅する。生成スクリプトは GB 級の性能テストと、強制中断→再開の出力が一気通し実行とバイト一致することの検証に使う | NF-04 の決定性が再開テストの判定基準 |

## CLI 仕様

CLI はライブラリの薄いラッパーで、引数の解釈、SIGINT の扱い、進捗表示、結果の整形だけを担います。サブコマンドは inspect / split / status の3つで、inspect は引数に応じてライブラリの inspect / profile / plan を呼び分けます。

```
# 共通: CSV 読み込みオプション（全サブコマンド）
  [--encoding utf-8|shift_jis]         # 既定 utf-8
  [--delimiter <char>]                 # 既定 ,
  [--no-header]
  [--max-record-size <SIZE>]           # 既定 16MiB。超えたら終了コード 2 で停止
  [--json]                             # 結果を JSON で標準出力に書く
  [--quiet]                            # 進捗表示を抑制

# 共通: キー指定（inspect / split）
  --key <列名 | #列番号>
  --type int|decimal|date|datetime|string
  [--scale <N>]                        # decimal で必須
  [--format <strftime書式>]            # date / datetime で必須
  [--tz <IANA名 | +09:00>]             # datetime でオフセット無しの値を解釈するとき必須
  [--no-trim]
  [--invalid-guard <N> | --no-invalid-guard]   # 先頭 N 件が全て invalid なら停止。既定 10000

# 共通: 区間指定（inspect / split）
  (--bounds <v1,v2,...> | --range <NAME>=<LO>..<HI> ...)
  [--mode split|extract]               # 既定 split

ksplit inspect <INPUT | ->
  [--sample <N>]                       # 各列のサンプル件数、既定 20
  [<キー指定>]                         # あると全件を読み、キー列の分布を出す
  [--bins <N>]                         # ヒストグラムのビン数、既定 20
  [--sample-size <N>]                  # リザーバサンプル件数、既定 100000
  [--suggest <N>]                      # 等件数 N 区間の境界値を提案
  [<区間指定>]                         # キー指定と併せてあると区間ごとの件数も出す
  [--strict]

ksplit split <INPUT | -> -o <OUT_DIR> <キー指定> <区間指定>
  [--strict]
  [--resume | --overwrite]
  [--checkpoint-interval <SIZE>]       # 既定 256MiB
  [--write-buffer <SIZE>]              # 既定 64KiB、出力ファイルごと
  [--report <path.json>]

ksplit status <OUT_DIR>
```

### 引数の規則

- `inspect` は引数に応じて3段階で動く。キー指定なしなら先頭だけ読んで列一覧とサンプルを出す（ライブラリの inspect）。キー指定があれば全件を読んで分布を加える（profile）。区間指定もあれば区間ごとの件数を加える（plan）。このとき分布と区間件数は同じパスで集計する
- `inspect` が全件を読む場合は、処理開始前に「キー指定があるため全件を読みます」と標準エラー出力に表示する（`--quiet` で抑制）
- 区間指定はキー指定とセットでのみ有効。区間指定だけを渡した場合はエラー
- `--range` は複数回指定できる。`..100`（下限無し）、`100..`（上限無し）を許す。`--bounds` とは排他
- `--bounds 0,1000,10000` は `lt_0`、`0_1000`、`1000_10000`、`ge_10000` の4出力を生成する。日時値はファイル名用に `:` を `-` に置換する
- `--key` の値が `#` で始まる場合は列番号、それ以外は列名。`--no-header` 時は列番号のみ許す
- `INPUT` が `-` のときは標準入力。`split --resume` との組み合わせはエラー
- `--resume` と `--overwrite` は排他。どちらもなく出力が既にあればエラー
- `inspect` と `split` は同じキー・区間指定を受け付ける。`inspect` で確認したコマンドラインの `inspect` を `split` に変えて `-o` を足せばそのまま実行できる
- `--json` 指定時は結果のみを標準出力に書き、進捗と警告は標準エラー出力に書く。パイプで後続処理に渡せる
- SIGINT を1回受けたらチェックポイントを書いて終了コード 5 で終わる（inspect はチェックポイントなしで終了）。2回目は即時終了
- 引数の検証エラー（キー列が見つからない、区間の重複、予約名との衝突を含む）はすべて処理開始前に報告する。区間名に `_unmatched`、`_invalid` は使えない

### 終了コード

| コード | 意味 |
| --- | --- |
| 0 | 正常終了 |
| 1 | 引数・設定のエラー（処理開始前） |
| 2 | 入力データのエラー（CSV 解析失敗、レコード長上限超過、strict モードでの invalid 検出、invalid ガードによる停止） |
| 3 | I/O エラー（読み込み・書き込み失敗、ディスクフル） |
| 4 | 再開の拒否（設定不一致、入力変更、チェックポイント破損） |
| 5 | キャンセルによる中断（split ではチェックポイント書き込み済み） |

### 使用例

```
# 1. まず下見。先頭だけ読んで列名とサンプル値を確認する
ksplit inspect orders.csv

# 2. キーを指定して全件を読み、分布を見てどこで区切るか決める
ksplit inspect orders.csv --key order_date --type date --format %Y-%m-%d --bins 12

# 3. 区間案を加えて試算する。ファイルは書かない
ksplit inspect orders.csv --key order_date --type date --format %Y-%m-%d \
  --bounds 2024-01-01,2024-04-01,2024-07-01,2024-10-01,2025-01-01

# 4. inspect を split に変えて -o を足し、実行する
ksplit split orders.csv -o out/ --key order_date --type date --format %Y-%m-%d \
  --bounds 2024-01-01,2024-04-01,2024-07-01,2024-10-01,2025-01-01

# Shift_JIS の売上データから金額 1万円以上だけ抽出
ksplit split sales.csv -o out/ --encoding shift_jis --key 金額 --type decimal --scale 0 \
  --range high=10000.. --mode extract

# 中断した処理の状態確認と再開（引数は前回と同一であること）
ksplit status out/
ksplit split orders.csv -o out/ --key order_date --type date --format %Y-%m-%d \
  --bounds 2024-01-01,2024-04-01,2024-07-01,2024-10-01,2025-01-01 --resume
```

## ライブラリ公開 API 案

公開 API は2層で構成します。高レベル層は5関数（inspect / profile / plan / split / read_checkpoint）、低レベル層は I/O を持たない区間判定器 `Router` です。すべて同期 API で、全ての型は `Send` とし、GUI はバックグラウンドスレッドから呼ぶことを想定します。CSV パーサやエンコーディング変換の実装詳細は公開しません。

```rust
// ---- 共通の入力・キー定義 ----

pub enum Input {
    Path(PathBuf),
    Reader(Box<dyn Read + Send>),
}

pub struct CsvDialect {
    pub delimiter: u8,
    pub has_header: bool,
    pub max_record_bytes: usize,
}

pub enum Encoding { Utf8, ShiftJis }

pub enum KeySelector {
    Name(String),
    Index(usize),
}

pub enum KeyType {
    Int,
    Decimal { scale: u32 },
    Date { format: String },
    DateTime { format: String, default_tz: Option<Tz> },
    String,
}

impl KeyType {
    pub fn parse(&self, text: &str) -> Result<KeyValue, ParseError>;
}

pub enum KeyValue {
    Int(i64),
    Decimal(i128),
    Date(NaiveDate),
    DateTime(DateTime<Utc>),
    String(String),
}

pub struct KeySpec {
    pub selector: KeySelector,
    pub key_type: KeyType,
    pub trim: bool,
    pub invalid_guard: Option<InvalidGuard>,   // None で無効
}

pub struct InvalidGuard {
    pub window_records: u64,   // 先頭この件数が全て invalid なら停止
}

// ---- 区間と判定器（低レベル層） ----

pub struct Range {
    pub name: String,
    pub lo: Bound,
    pub hi: Bound,
}

pub enum Bound {
    Unbounded,
    Value(KeyValue),
}

pub struct Router { /* ソート済み境界値 */ }

impl Router {
    pub fn new(key_type: &KeyType, ranges: Vec<Range>) -> Result<Self, ConfigError>;
    pub fn route(&self, key: &KeyValue) -> Routed;
    pub fn ranges(&self) -> &[Range];
}

pub enum Routed {
    Range(usize),
    Unmatched,
}

// ---- 実行制御 ----

#[derive(Clone)]
pub struct CancelToken { /* Arc<AtomicBool> */ }

impl CancelToken {
    pub fn new() -> Self;
    pub fn cancel(&self);
    pub fn is_cancelled(&self) -> bool;
}

pub struct Progress {
    pub bytes_read: u64,
    pub total_bytes: Option<u64>,
    pub records: u64,
}

// ---- inspect ----

pub struct InspectConfig {
    pub input: Input,
    pub dialect: CsvDialect,
    pub encoding: Encoding,
    pub sample_rows: usize,
}

pub struct Inspection {
    pub has_bom: bool,
    pub line_ending: LineEnding,
    pub columns: Vec<ColumnInfo>,
}

pub struct ColumnInfo {
    pub index: usize,
    pub name: Option<String>,
    pub samples: Vec<String>,
    pub type_candidates: Vec<KeyType>,
}

pub fn inspect(config: InspectConfig) -> Result<Inspection, Error>;

// ---- profile ----

pub struct ProfileConfig {
    pub input: Input,
    pub dialect: CsvDialect,
    pub encoding: Encoding,
    pub key: KeySpec,
    pub bins: usize,
    pub sample_size: usize,
    pub suggest: Option<usize>,
}

pub struct Profile {
    pub records: u64,
    pub invalid: u64,
    pub min: Option<KeyValue>,
    pub max: Option<KeyValue>,
    pub histogram: Option<Histogram>,
    pub suggested_bounds: Option<Vec<KeyValue>>,
    pub bytes_read: u64,
}

pub struct Histogram {
    pub approximate: bool,
    pub sampled: u64,
    pub bins: Vec<HistogramBin>,
}

pub struct HistogramBin {
    pub lo: KeyValue,
    pub hi: KeyValue,
    pub estimated_count: u64,
}

pub fn profile(
    config: ProfileConfig,
    cancel: &CancelToken,
    progress: impl FnMut(&Progress),
) -> Result<Profile, Error>;

// ---- plan / split ----

pub struct SplitConfig {
    pub input: Input,
    pub output_dir: PathBuf,
    pub dialect: CsvDialect,
    pub encoding: Encoding,
    pub key: KeySpec,
    pub ranges: Vec<Range>,
    pub mode: Mode,
    pub on_invalid: OnInvalid,
    pub existing: ExistingOutput,
    pub checkpoint: Option<CheckpointPolicy>,
    pub write_buffer_bytes: usize,
    pub progress_interval_bytes: u64,
}

impl SplitConfig {
    pub fn validate(&self) -> Result<(), ConfigError>;
}

pub enum Mode { Split, Extract }
pub enum OnInvalid { WriteToFile, Fail }
pub enum ExistingOutput { Fail, Resume, Overwrite }

pub struct CheckpointPolicy {
    pub interval_bytes: u64,
}

pub struct Report {
    pub per_range: Vec<RangeResult>,
    pub unmatched: u64,
    pub invalid: u64,
    pub records: u64,
    pub bytes_read: u64,
    pub elapsed: Duration,
    pub resumed_from: Option<u64>,
}

pub struct RangeResult {
    pub name: String,
    pub path: Option<PathBuf>,   // plan では None
    pub records: u64,
}

pub fn plan(
    config: &mut SplitConfig,   // Input::Reader を読むため &mut
    cancel: &CancelToken,
    progress: impl FnMut(&Progress),
) -> Result<Report, Error>;

pub fn split(
    config: SplitConfig,
    cancel: &CancelToken,
    progress: impl FnMut(&Progress),
) -> Result<Report, Error>;

// ---- status ----

pub struct CheckpointStatus {
    pub tool_version: String,
    pub input_path: PathBuf,
    pub input_unchanged: bool,
    pub bytes_done: u64,
    pub total_bytes: u64,
    pub records_done: u64,
    pub per_range: Vec<RangeResult>,
    pub saved_at: SystemTime,
}

pub fn read_checkpoint(output_dir: &Path) -> Result<Option<CheckpointStatus>, Error>;

// ---- エラー ----

pub enum Error {
    Config(ConfigError),
    Data { record: u64, byte_offset: u64, column: Option<usize>, kind: DataErrorKind },
    Io { path: Option<PathBuf>, source: std::io::Error },
    ResumeRejected(ResumeRejection),
    Cancelled { checkpoint_written: bool },
}

pub enum DataErrorKind {
    Malformed,            // 引用符の不整合など
    RecordTooLarge,       // FR-06
    InvalidKey,           // strict モードでの invalid
    InvalidGuardTripped,  // FR-45
}
```

### 設計上の注意点

- `plan` と `split` は同じ `SplitConfig` を受け取り、内部では同じパイプライン（読み込み → キー抽出 → `Router` → Sink）に異なる Sink（カウンタ / ファイル書き込み）を差すだけの違いとする。`profile` も同じパイプラインで `Router` の代わりに統計コレクタを使う
- `Router` は I/O を持たず、`new` で区間の重複・型不一致を検出する。GUI が自前のデータ源に対して区間判定だけを使うこともできる
- `SplitConfig::validate()` は `Router::new` の検証に加え、出力名の衝突、`Input::Reader` と `ExistingOutput::Resume` の組み合わせ、区間数 × `write_buffer_bytes` の警告閾値を検査する
- 全ての長時間関数は `CancelToken` を受け取り、レコード境界でキャンセルを確認する。`split` はキャンセル時にチェックポイントを書いてから `Error::Cancelled` を返す
- 非同期ランタイムには依存しない。GUI（Tauri、egui など）はバックグラウンドスレッドで呼び、`progress` コールバックからチャネル経由で UI に流す
- `Error` は構造化し、CLI の終了コード（1〜5）と 1 対 1 で対応させる。`Data` エラーはレコード番号とバイトオフセットを必ず持ち、GUI が該当行を表示できるようにする
- `Profile.histogram` はリザーバサンプルから構築する近似値で、`approximate: true` を常に持つ。`min` / `max` / `records` / `invalid` は正確値。string 型では `histogram` と `suggested_bounds` は `None`

### 将来の拡張点

JSONL 対応に備えて、内部のレコード読み込みだけをトレイトで分離します。これ以上の抽象化は初期スコープでは行いません。

```rust
pub(crate) trait RecordSource {
    fn next_record(&mut self) -> Result<Option<RawRecord<'_>>, SourceError>;
    fn position(&self) -> u64;
    fn seek(&mut self, offset: u64) -> Result<(), SourceError>;
    fn header(&self) -> Option<&[u8]>;
}

pub(crate) struct RawRecord<'a> {
    pub bytes: &'a [u8],
    pub key: Option<&'a [u8]>,
}
```

トレイトは当面 `pub(crate)` に留め、JSONL 実装を追加する時点で公開するかを判断します。

## 設計上の決定事項と理由

| 決定 | 理由 | 捨てた選択肢 |
| --- | --- | --- |
| 区間は半開 `[lo, hi)` に固定 | 閉区間と混在させると境界値がどちらに入るかで必ず混乱する。境界値の列挙から隙間なく区間を生成できる | 閉区間、開閉の選択オプション |
| キー型は明示指定 | 推論は10GB のうち1行だけ形式が違うと結果が変わり、原因特定が困難になる | 先頭 N 行からの型推論 |
| decimal は固定小数（スケール付き i128） | f64 では境界値 0.3 と入力の 0.3 が表現誤差で異なる値になりうる | f64 比較 |
| datetime は UTC の瞬間で比較、既定タイムゾーンは明示指定 | ローカルタイムゾーンを暗黙に使うと実行環境で分割結果が変わる | システムのタイムゾーンを既定にする |
| string は Unicode コードポイント順 | Shift_JIS のバイト順と UTF-8 のバイト順は一致しない。エンコーディングに依らず同じ区間に入ることを保証する | バイト列比較、ロケール依存の照合 |
| 行はバイト透過で出力 | 再シリアライズすると引用符の有無や改行コードが変わる。デコード不要で速く、再開時のバイト長検証も単純になる | パース後の再シリアライズ |
| 区間判定はソート済み境界値への二分探索 | 区間数が数千でも 1 行あたりの判定コストがほぼ一定 | 線形探索 |
| 範囲外・不正行は専用ファイルに書くが既定 | 10GB 処理後に「数行捨てられていた」と気づくのが最悪。黙って捨てるのは extract モードでの範囲外行だけ | 黙ってスキップ |
| 再開は truncate + seek 方式 | 追記方式だとチェックポイント以降の半端な行が残り重複が生じる。出力のバイト長を記録すれば巻き戻しは正確にできる | 一時ディレクトリへの書き出しと最後の rename（再開不可） |
| 設定不一致での再開は拒否し、抑えフラグを作らない | 異なる区間設定で続行すると前半と後半で分割基準が違う出力になり、検知できない | `--force` による強制再開 |
| エンコーディングは明示指定 | Shift_JIS の自動判定は先頭数 KB では確実でなく、誤判定すると全出力が壊れる | BOM とバイトパターンによる自動判定 |
| バックスラッシュエスケープ非対応 | Shift_JIS の2バイト目に 0x5C が現れる文字（表、能など）があり、バイト単位の解析と両立しない | オプションでの有効化 |

GUI 対応を見越して追加した決定です。

| 決定 | 理由 | 捨てた選択肢 |
| --- | --- | --- |
| ライブラリは同期 API、全ての型を Send にする | GUI はバックグラウンドスレッドから呼べば十分で、tokio などへの依存を課さない。Tauri でも egui でも同じ形で使える | async API |
| キャンセルは CancelToken（AtomicBool）を引数で渡す | 最も単純で、ランタイム非依存。レコード境界で確認するので応答は最大で、1レコード分遅れるだけ | スレッドの強制停止、チャネル経由の停止要求 |
| キャンセル時にチェックポイントを書く | 中止した処理をその場から再開できる。直前のチェックポイントまで最大 256 MiB 分を捨てる必要がなくなる | チェックポイントを書かずに終了 |
| profile / plan / split は同じパイプラインに異なる Sink を差す | 判定ロジックが単一になり、plan と split の件数一致（FR-66）が構造的に保証される | 関数ごとの個別実装 |
| CLI では profile と plan を inspect に統合 | サブコマンドを増やすより「引数を足すほど詳しく調べる」のが観えやすい。全件読み込みに切り替わることは開始時に明示する | ライブラリ関数と 1 対 1 のサブコマンド |
| ヒストグラムはリザーバサンプリングによる近似 | 単一パスでは min/max が確定する前にビン幅を決められない。サンプルを持っておき最後にビンに落とすのが単純で、用途（区切り位置の当たりを付ける）には十分 | 2パスの正確なヒストグラム、t-digest |

## 未決事項・将来課題

初期スコープから外した項目と、実装時に決める項目です。

| 項目 | 状態 | メモ |
| --- | --- | --- |
| JSONL / JSON 対応 | 将来課題 | `RecordSource` の実装追加。キーはドット記法でネスト指定。値がオブジェクト・配列の場合は invalid。JSON（配列全体）はストリーミングパースが必要で JSONL より難度が高い |
| GUI | 将来課題（別プロジェクト） | ライブラリの inspect / profile / plan / split / read_checkpoint と CancelToken を使う。フレームワークは未決 |
| 分位点の正確な算出 | 将来課題 | 等件数区間の境界を近似でなく出すには t-digest などが必要。初期はリザーバサンプルからの提案（FR-64）に留める |
| 圧縮入力（gzip / zstd） | 将来課題 | 読み込み自体は容易。再開は seek 不可のため「先頭から読み捨て」か「圧縮入力では再開無効」のどちらか |
| 圧縮出力 | 将来課題 | 再開時の truncate が圧縮ストリームと両立しない。チェックポイントごとに圧縮フレームを区切る設計が必要 |
| 出力エンコーディング変換 | 将来課題 | バイト透過と両立しないため、有効時は全列をデコード・再エンコードする別経路になる |
| FD 上限を超える区間数 | 将来課題 | 開いたファイルの LRU 管理。初期は区間数がプロセスの FD 上限に近い場合に警告を出すだけ |
| 複合キー | 将来課題 | 辞書式順序での区間定義が必要。需要が明確になるまで着手しない |
| キー値ごとの分割・ハッシュ分割 | 将来課題 | 範囲分割とは区間判定部だけが異なる。対応する場合はサブコマンドで分ける |
| 等幅区間の生成 | 将来課題 | 初期スコープから外した（FR-25）。月単位の幅は日数が一定でないため、対応時は境界値列挙への展開規則を定義する |
| 大文字小文字の統一 | 将来課題 | trim のみ初期スコープに含めた（FR-17）。string 型の範囲指定の需要が出てから、Unicode の折り畳み範囲を決めて追加する |
| --bounds と --range の実装順 | 決定 | --range を先に実装し、--bounds はその上の変換として後から追加する。どちらも初期リリースに含める |
