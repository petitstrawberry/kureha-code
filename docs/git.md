# Git 対応

## 今できること(macOS・Linux・Scarlet)

- ステータスバーに、開いているフォルダのリポジトリのブランチ名と変更数(`main · 3 changes`)。
  HEAD を直接チェックアウトしているときは `detached at 1a2b3c4`、コミットのないブランチは `main (no commits)`。
- エクスプローラーの変更マーク。ファイルは右端に `M`(変更)・`A`(追加)・`R`(名前の変更)・
  `D`(削除)・`U`(未追跡)・`!`(競合)を出し、名前も同じ色にする。フォルダは中に変更があれば点を出し、
  中で一番重い変更の色にする(競合 > 変更・削除 > 追加・名前の変更 > 未追跡)。
- 対象はエクスプローラーで開いているフォルダ、なければ編集中のファイルのフォルダ。
  フォルダがリポジトリの一部なら、マークはそのフォルダの中だけ、変更数はリポジトリ全体。
- 読み直すのは、フォルダが変わったとき、ファイルを保存したとき、それ以外は3秒ごと
  (エディタの外での変更やコミットに追従するため)。

`git status` と同じ分類になることをテストで確かめている(ステージ済みと作業ツリーの両方で変わったファイルは
1件として数える)。

## 作り

- [gitoxide](https://github.com/GitoxideLabs/gitoxide)(`gix`、純Rust)を使い、`git` コマンドは呼ばない。
  使う機能は `status` と `sha1` だけ(ネットワーク、認証、シグナル処理、並列化は入れない)。
- `src/git.rs`(ライブラリ): `read` がリポジトリを読んで `Snapshot`(HEAD、変更の表、未追跡フォルダ、総数)を
  返す。`Watcher` は専用スレッド `kureha-git` で読み、依頼が重なったら最新の1件だけに答える。
  読んでいる最中に新しい依頼が来たら gix の中断フラグで止める。
- `src/git_ui.rs`(アプリ): 依頼の発行、古い答えの破棄(フォルダの世代で判定)、ステータスバーとエクスプローラーへの
  反映。答えが前回と同じなら描き直さない。
- 未追跡のファイルは、丸ごと未追跡のフォルダを1件にまとめる(`node_modules` のような大きなフォルダで
  何万件にならないように)。保持する変更は2万件まで。
- サブモジュールの中は見ない。作業ツリー内の名前の変更は検出しない(ステージ済みの名前の変更は出す)。

## 安全

**フォルダを開いただけで、そのフォルダのコードが動いてはいけない。**

- Git の設定には、コマンドを指定するもの(clean/smudge フィルタなど)と秘密(認証情報、署名鍵)がある。
  gix の既定では、持ち主が自分のリポジトリの `.git/config` も信用する。実際に、`.git/config` に clean フィルタを
  書いたリポジトリで変更を調べると、そのコマンドが実行された(`git status` も同じ)。
  clone や展開したリポジトリは自分の持ち物になるので、持ち主での判定は役に立たない。
- そこで、この種の設定(gix の `filter_config_section` が使われる読み込み: フィルタ、サブモジュール、リモート、
  認証情報、署名鍵など)は、リポジトリの設定からは一切読まず、ユーザーとシステムの設定からだけ読む。
  ユーザーが自分で入れた Git LFS のようなフィルタは今までどおり効く。
- その結果、リポジトリが自分で定義したフィルタのファイルは、日時だけ変わった場合に「変更あり」と出ることがある
  (中身をフィルタなしで比べるため)。表示だけの機能なので、コマンドを動かすよりこちらを選ぶ。
- 書き込まない。index は読むだけで、更新(`git status` が行う stat 情報の書き戻し)もロックもしない。
  エディタとユーザーの `git` が index を取り合わないようにするため。

テスト: `.git/config` の clean フィルタが目印のファイルを作るリポジトリで、変更は正しく出るが目印は作られないこと。
index のバイト列と更新時刻が変わらないこと、`index.lock` が残らないこと。

## Scarlet

gix の依存のうち4つが Scarlet を想定していないので、Scarlet 用の fork を `[patch.crates-io]` で差し替えている
(コミットで固定)。どの fork も、Scarlet 以外では元の版と同じコードになる。gix の版は、fork を作って確かめた
`=0.90.0` に固定している。差し替えは `Cargo.lock` で保たれるが、差し替えたクレートの新しい版が crates.io に出たあとで
`cargo update` すると、その版が選ばれて差し替えが外れる(cargo が patch を使っていないと警告する)。そのときは
fork をその版の上に作り直す。

| fork | ブランチ | 中身 |
| --- | --- | --- |
| [memmap2-rs](https://github.com/petitstrawberry/memmap2-rs) | `scarlet-0.9.12` | ネイティブの MemoryMap / MemoryUnmap / MemoryProtect(700〜702)。ハンドルは std の `std::os::fd::AsRawFd` で取る(std の変更は不要) |
| [getrandom](https://github.com/petitstrawberry/getrandom) | `scarlet-0.4.3` | ネイティブの GetRandom(0.3.3 用の Scarlet ブランチの移植) |
| [filetime](https://github.com/petitstrawberry/filetime) | `scarlet-0.2.29` | std の `Metadata` と `File::set_times` だけで書いた実装(Unix 用のものはビルドできず、wasm 用は読むと panic する) |
| [gitoxide](https://github.com/petitstrawberry/gitoxide) | `scarlet-gix-0.90.0` | `gix-sec` は所有者の確認を WASI と同じく「常に本人」、`gix-config-value` はユーザー名からのホーム取得を Windows と同じく「なし」にする。ほかの gitoxide クレートは crates.io の版を使う(ワークスペースのパスのままだと `gix-error` などが2重になる) |

Scarlet のファイルシステムとメモリマップ:

- tmpfs・ext2・fat32 の通常ファイルはマップできる。initramfs(cpiofs)はできないので、ディスクなしで起動したときの
  ルート(initramfs の上に tmpfs を重ねた overlay)では、下の層のファイルがマップできない。
- そこで memmap2 の fork は、private(読み取り専用・コピーオンライト)のマップに失敗したら、匿名メモリを確保して
  ファイルの中身を読み込む。後からのファイルの変更が見えない以外は private のマップと同じで、ファイルのカーソルは
  元に戻す。書き込みを反映する共有マップには代わりがないのでエラーのまま。
- 読み取り専用のマップは private にする。ext2 の共有マップは、ファイル全体をカーネルのメモリに複製し、
  解除すると書き込みがなくても丸ごと書き戻すため(大きな pack ファイルで重い)。
- Scarlet のスレッドの既定のスタックは 64 KiB で gix には足りない。Git のワーカーは、ファイルの読み書きのワーカーと
  同じく 2 MiB を指定する。

確認(AArch64、QEMU TCG): Scarlet の `tests/loader-smoke/build-kernel.sh` のカーネルとブートストラップ init
(`tests/native-desktop-smoke/run.py --program ... --expect-marker ...`)で、Kureha の `git::read` を呼ぶ小さな
プログラムを動かした。pack ファイルと packed-refs を持ち、変更・削除・ステージ済みの追加・未追跡のファイルと
フォルダがあるリポジトリを、initramfs の層(読み込みでの代替)と tmpfs へのコピー(本物のマップ)の両方で読み、
どちらも `git status` と同じ結果になった(約 100 ms)。ワーカースレッド経由でも同じ(2 MiB のスタックを指定する前は
スタックが溢れて答えが返らなかった)。RISC-V64 は、ビルドは通るが実行はまだ確かめていない。

## 次にやること

- エディタの行の左に変更のしるし(追加・変更・削除)。scarlet-ui の TextView に行ごとの印を描く API が要る
  (今は `diagnostic_spans` と行番号しかない)。
- 変更の差分表示、ステージ・取り消し・コミット。
- RISC-V64 の Scarlet での実行確認。
