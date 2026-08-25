# Yabumi 言語仕様

Agent Skills(SKILL.md 同梱スクリプト)向けの単一ファイル完結スクリプト言語。LLM が bash/python 代替として記述することを想定する。

設計優先度: **依存ゼロ配布 > 機械可読エラー > 一発で正しく書ける > 権限監査**。
全体思想: **「見た目 = 挙動」** — 暗黙動作を排除し、並行・可変・エラー伝播をすべて構文上で可視化する。

---

## 1. CLI

サブコマンドは3つのみ。

| コマンド | 動作 |
|---|---|
| `ybm <file>` | 型チェック実行後、成功時のみ実行 |
| `ybm check <file>` | 型チェック + fmt差分確認(書き換えなし)+ lint。docテストブロックの型チェックも行う(実行はしない) |
| `ybm test <file>` | docテストを実行 |

- `ybm check --apply <file>`: fmtを書き換える。`--apply` なしではfmtを書き換えず、差分を表示する
- exit code: 成功 = 0。型エラー・lint 警告・fmt 差分(デフォルトcheck時)・テスト失敗・実行時 Err 終了 = 1
- 診断形式: `file:line:col [E0000] message`。エラーコードは安定(機械可読)

## 2. 字句

- 拡張子: `.ybm`
- shebang: 1行目の `#!/usr/bin/env ybm` を許容(無視して実行)
- コメント: `#` 行コメントのみ。ブロックコメントなし
- doc コメント: 宣言直前の `##`
- **ブロックはインデントのみ**。行末コロンは全廃。インデントはスペース4固定、タブは構文エラー
- エンコーディング: UTF-8

## 3. 型システム

静的型付け。名前的型付け(構造的部分型なし)。継承・trait なし。

### 3.1 プリミティブ(小文字)

| 型 | 実体 |
|---|---|
| `int` | i64 |
| `float` | f64 |
| `bool` | true / false |
| `str` | UTF-8、不変 |

任意精度整数・decimal はなし。

### 3.2 コレクション

`list[T]` / `dict[K, V]` / `set[T]` / `tuple[A, B, ...]`。リテラルは Python 風:

```
xs = [1, 2, 3]
m = {"a": 1}
s = {1, 2}
t = (1, "a")
```

### 3.3 stdlib 型(大文字)

`Result[T, E]` / `Option[T]` / `Error` / `Value`。

### 3.4 型注釈の必須範囲

- 関数シグネチャ(引数・戻り値)は**必須**
- ローカル変数は推論。推論不能箇所(空コレクション等)は注釈要求: `xs: list[int] = []`

### 3.5 struct / enum

struct 内にメソッド同居。フィールド単位の可変性指定はなし(束縛の `var` に従属)。

```
struct User
    name: str
    age: int

    def greet(self): str
        return f"hello {self.name}"

enum Shape
    Circle(radius: float)
    Rect(w: float, h: float)
```

コンストラクタは**名前付き引数必須**(位置引数不可): `User(name: "a", age: 3)`

### 3.6 ジェネリクス

ユーザー定義可。`[T]` 記法。

```
def first[T](xs: list[T]): Option[T]
    return xs.get(0)
```

## 4. 変数と可変性

- **不変デフォルト**。再代入・フィールド変更には `var` 宣言が必要
- 型は初回束縛で固定

```
x = 5          # 不変。x = 6 は型エラー
var y = 5      # 可変。y = 6 OK
var u = User(name: "a", age: 3)
u.age = 4      # var 束縛なので OK
```

## 5. 関数

main 関数なし。トップレベルから上から実行される。

```
def fetch(url: str): Result[str, Error] uses {net}
    return http.get(url)?.body
```

- 戻り値注釈は `:`(`->` ではない)
- effect は `uses {..}` で宣言(§8)
- 関数型の表記: `(int) -> str uses {net}`(型文脈では `->`)

### 5.1 ラムダ

JS 風無名関数。**括弧必須・単一式のみ**(複数文は `def` に切り出す)。引数型は文脈推論、注釈も可。

```
xs.map((x) => x * 2)
xs.filter((x: int) => x > 3)
```

## 6. 式

### 6.1 式指向

if / match は値を返す式。三項演算子なし。

```
label = if score > 80
    "high"
else
    "low"

area = match shape
    Circle(r) => 3.14 * r * r
    Rect(w, h) => w * h
```

- match アームは `=>`。複数文アームは `=>` 後改行 + インデント
- enum の match は**網羅性チェック**あり

### 6.2 イテレータ(eager)

メソッドチェーン方式。内包表記はなし。`xs.map(f)` は即 `list[U]` を返す(`Iterator[T]` 型は存在しない)。

メソッド語彙は Rust 風: `map / filter / fold / find / any / all / count / sum / enumerate / zip / rev / take / skip / flat_map / sort_by / chain` 等。

### 6.3 パイプ

`|>`。優先順位最低・左結合。

- 単項関数は裸名 OK: `x |> json.encode`
- 引数ありは `_` プレースホルダ**必須**: `x |> fs.write("out.json", _)`
- 暗黙の第1引数挿入は**なし**
- ステージ後置 `?` 可: `x |> parse? |> validate?`。パイプ自体は Result を自動短絡しない
- fmt は長いパイプを1ステージ1行に整形

### 6.4 文字列

- f-string: `f"count: {n}"`
- 連結: `+`
- 比較は構造等価 `==` のみ。参照等価なし

## 7. エラー処理

### 7.1 Error 型

組み込み単一 `Error` 型を stdlib 全体で統一使用:

```
struct Error
    kind: str            # "net" | "fs" | "decode" | ... ユーザー定義も自由
    message: str
    cause: Option[Error]
```

`?` は型変換なしで貫通する(Rust の From 変換地獄を排除)。

### 7.2 `?` 演算子

- `Result` / `Option` 両対応(Rust 同様、戻り型が一致する関数内で使用可)
- トップレベルでの `?`: Err 時に stderr へエラー表示 + exit 1

### 7.3 Result 無視の禁止

Result 戻り値を使わない = **型エラー**。明示破棄は `_ = f()`。

### 7.4 panic 排除

catch 可能な panic は存在しない。範囲外アクセス・ゼロ除算・整数オーバーフローは**即プロセス異常終了**(exit 1 + トレース表示、捕捉不可)。安全版 API を併設:

- `xs[i]` → 範囲外で即終了 / `xs.get(i): Option[T]`
- `a / b` → ゼロ除算で即終了 / `math.checked_div(a, b): Option[int]`

### 7.5 assert

組み込み `assert(cond, msg)`。失敗で exit 1。docテストの主要検証手段。

## 8. Effect システム

**静的検査のみ**(実行時強制なし)。粒度は6種固定:

`fs, net, env, proc, time, rand`

- stdin 読み取りは `env` に含む
- `print` / `eprint` は effect 不要(純粋関数内でもデバッグ出力可)
- effect 宣言なし関数 = 純粋関数。純粋関数から effect 関数を呼ぶと**型エラー**
- トップレベルは全 effect 暗黙許可
- 高階関数: 関数型引数の effect は**暗黙伝播**(チェッカ内部で effect row を推論。ユーザーが effect 変数を書くことはない)

```
def map[T, U](xs: list[T], f: (T) -> U): list[U]
    # f が {net} を持てば map(xs, f) の呼び出し元にも {net} が要求される
```

## 9. 並行実行

- IO は構文上の async/await なし。内部非同期・呼び出し点で暗黙待機(CPU をブロックしない)
- 並行は**明示コンストラクトのみ**。暗黙並行なし

| 構文 | 型 | 用途 |
|---|---|---|
| `par [f(), g()]` | `list[T]`(全要素同型) | 固定個数・同種 |
| `par (f(), g())` | `tuple[A, B]` | 固定個数・異種 |
| `xs.par_map((x) => ...)` | `list[U]` | 動的コレクション |
| `xs.par_each((x) => ...)` | なし | 副作用のみ |

- fail-fast なし。**全完走**を待つ。要素が `Result` ならそのまま `list[Result[T, E]]` で受ける
- クロージャキャプチャは値コピー。par 枝間の共有可変状態なし
- ランタイム: マルチスレッド(CPU 並列あり)

```
results = par [fetch(url1), fetch(url2)]
pages = urls.par_map((u) => http.get(u))
```

## 10. モジュール

- import 構文なし。パッケージ概念なし
- モジュールファイルは**1行目に `module`** directive を書く
- エントリファイル(`ybm` に渡すファイル)と**同階層**の directive 付き `.ybm` が**自動で全部**取り込まれる
- 名前空間分離なし(単一フラット名前空間)。名前衝突 = 型エラー
- モジュールは**宣言のみ**(def / struct / enum / 定数)。トップレベル実行文は禁止(取り込み順で挙動が変わる問題を根絶)
- `ybm check` は同階層モジュールをまとめて検査・fmt・lint

## 11. 標準ライブラリ

import 不要の組み込み名前空間。

### 11.1 codec(4形式)

`json` / `csv` / `yaml` / `toml`(XML・JSON5 は対象外)。

- decode は**代入先注釈駆動**: `data: User = json.decode(s)?`
- encode: `json.encode(value): str`
- CSV: `csv.decode[T](s): Result[list[T], Error]`。T はフラット struct、1行目ヘッダと名前マッチ
- YAML: 安全サブセット(アンカー / エイリアス / マルチドキュメント非対応)
- スキーマ未知データ用に動的 `Value` 型を併設(全 codec 共通)

### 11.2 モジュール一覧

| 名前空間 | 内容 | effect |
|---|---|---|
| `fs` | read / write / append / list / exists / remove | `fs` |
| `http` | client のみ: get / post / put / delete、headers、timeout、body | `net` |
| `env` | get / set / args / stdin | `env` |
| `proc` | コマンド実行、stdout / stderr / exit code 取得 | `proc` |
| `time` | now / sleep / format / parse | `time` |
| `rand` | 乱数 | `rand` |
| `regex` | 正規表現 | なし(純粋) |
| `math` | 数学関数、checked 系 | なし(純粋) |

http server / sqlite / crypto は対象外。

### 11.3 組み込み関数

`print` / `eprint`(stderr)/ `assert(cond, msg)`。log レベル機構なし。

## 12. fmt / lint

- fmt: `ybm check --apply` で in-place 書き換え(gofmt 流)。`ybm check` は差分確認のみ。冪等(fmt∘fmt = fmt)
- lint ルール: 未使用変数 / 未使用関数 / シャドーイング / 到達不能コード / 命名規約(snake_case 変数・関数、PascalCase 型)
- lint 警告ありも exit 1(「check が通る = 綺麗」の単純規範)

## 13. doc テスト

`##` doc コメント内の ``` フェンスブロックがテスト。

```
## 2つの int を加算する。
##
## ```
## assert(add(1, 2) == 3)
## ```
def add(a: int, b: int): int
    return a + b
```

- 実行は `ybm test <file>`。`ybm check` は型チェックのみ行う(実行しない)
- 各ブロックは独立プログラムとして実行。スコープはファイル全体(エントリ + 同階層モジュールの全宣言)
- effect 制限なし(トップレベル同等)
- `assert` 失敗 or Err での異常終了 = fail。ブロック単位で pass/fail 集計、1つでも fail なら exit 1

## 14. メモリモデル

- GC なし。**値セマンティクス + スコープ RAII**
- 実装は参照カウント(Arc + copy-on-write)。所有権・借用の概念はユーザーに露出しない
- 「GC なし」は実装特性であり、ユーザーが所有権エラーに遭遇することはない

## 15. サンプル

```
#!/usr/bin/env ybm

struct Repo
    name: str
    stars: int

def fetch_repos(user: str): Result[list[Repo], Error] uses {net}
    body = http.get(f"https://api.example.com/users/{user}/repos")?.body
    repos: list[Repo] = json.decode(body)?
    return Ok(repos)

users = ["alice", "bob", "carol"]
results = users.par_map((u) => fetch_repos(u))

top = results
    .filter((r) => r.is_ok())
    .flat_map((r) => r.unwrap_or([]))
    .filter((repo) => repo.stars > 100)
    .sort_by((repo) => repo.stars)
    .rev()
    .take(10)

top |> toml.encode |> fs.write("top.toml", _)
print(f"done: {top.count()} repos")
```
