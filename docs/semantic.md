# Semantic search

Finds files by what they mean ("tooth hole repair visit" finds a note about a dentist's cavity filling). It is a plugin (`plugins/semantic`, id `com.lookout.semantic`) behind the same host, gate and Job Object as the other plugins, **off until you accept its permissions** in the Plugins panel (Ctrl+, then Plugins). The model files can be downloaded with one button (see below), or placed by hand.

## One-click setup

Plugins panel, Semantic search: **Set up semantic search** shows the total size (51.4 MB: model 34.0, tokenizer 0.7, ONNX Runtime package 16.7) and asks first. Only after you confirm, Lookout downloads the three sources listed below, over HTTPS, from `huggingface.co` (and the `*.hf.co` CDN names it redirects to) and `files.pythonhosted.org`. It sends a plain GET: no cookies, no credentials, no headers of its own, nothing about you. Files go to a temp folder inside the data folder (`semantic-download`), their SHA-256 is checked before use (the runtime package, then the one dll taken from it), free disk space is checked first, and only when all three pass are they moved into the folder below. A cancelled or failed download leaves nothing behind. Nothing runs at startup or in the background. **Remove semantic files** deletes only those three files. After setup, turn the plugin on; no restart is needed.

## What you must place by hand, and where

Folder: `%APPDATA%\Lookout\plugins\com.lookout.semantic\assets\` (the data folder Lookout uses, plus `plugins\<plugin id>\assets`; the Plugins panel shows the exact path). Three files, with exactly these names:

| file | what | SHA-256 |
|---|---|---|
| `model.onnx` | bge-small-en-v1.5, int8 ONNX (34,014,426 bytes) | `6c9c6101a956d62dfb5e7190c538226c0c5bb9cb27b651234b6df063ee7dbfe4` |
| `tokenizer.json` | that model's tokenizer (711,396 bytes) | `d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66` |
| `onnxruntime.dll` | ONNX Runtime 1.32.0, Windows x64 (from the official `onnxruntime` wheel) | `443c0065fcdba17618ddef05050308f4b471893c5c7da0337d6957c9073ad8a7` |

The helper hashes all three at start and **refuses to load on any mismatch** (the Plugins panel shows which file). The model and tokenizer are used from the very bytes that were hashed; the DLL is loaded by its absolute path in this folder only, never by name or `PATH` (a different `onnxruntime.dll` in `System32` is never used). The check protects against a wrong or damaged file, not against someone who can already write to your data folder: they could swap the DLL between the check and the load.

### Where the files come from (checked against their published hashes)

* `model.onnx`: `https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/ea104dacec62c0de699686887e3f920caeb4f3e3/onnx/model_quantized.onnx`, saved under the name `model.onnx`. The repository lists this file as 34,014,426 bytes with the LFS SHA-256 above.
* `tokenizer.json`: `https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/ea104dacec62c0de699686887e3f920caeb4f3e3/tokenizer.json` (711,396 bytes, git blob `688882a79f44442ddc1f60d70334a7ff5df0fb47`, which is what the local file hashes to).
* `onnxruntime.dll`: inside the wheel `https://files.pythonhosted.org/packages/a9/3e/27039badc39a1b01628b6b40b1ff1bf4a886e6dfb2f311c483606b7c86f7/onnxruntime-1.32.0-cp312-cp312-win_amd64.whl` (PyPI, `onnxruntime` 1.32.0, wheel SHA-256 `efe7456952c243a05a53d5b8732953625144642c2f3989094277f6217f0c0c40`). A wheel is a zip file: open it and take `onnxruntime/capi/onnxruntime.dll`. Leave the other files of the wheel out.

### Visual C++ runtime

`onnxruntime.dll` is built with Visual C++ and needs the **Microsoft Visual C++ 2015-2022 Redistributable (x64)** (`vcruntime140.dll`, `vcruntime140_1.dll`, `msvcp140.dll`). Most Windows 10 and 11 machines have it; Lookout does not install it. Without it the runtime cannot be loaded and the Plugins panel shows the loader's error instead of results. Not tested on a machine without it.

## How it works

* The core reads text with the existing extractor and OCR, **masks secrets**, skips excluded files (a file must be in the content index and pass the exclusion rules again) and sends the masked text, never a file or path to open, to the helper with `index_batch`. The helper has no file access of its own.
* One vector per file: the file name's words plus the first 512 tokens, embedded with bge-small-en-v1.5 (CLS pooling, L2-normalised, as in the model card; queries get "Represent this sentence for searching relevant passages: "). Vectors are quantised to int8 and kept in `vectors.log`, an append-only log of frames each sealed with the plugin's own key. They live in the helper only: the daemon holds no vectors.
* Indexing runs only when the user is idle, indexing is not paused, and the content worker is not busy, at the same 10% duty cycle; files you open often or changed recently go first. The footer shows `meaning index done/total`.
* Query: for two or more words that do not start with a plugin trigger, after a 150 ms typing pause, the helper is started if needed, scans all vectors (brute force) and returns files whose similarity is at least `min_similarity` (default 0.60, editable in the panel). Results come after the instant ones, score below exact name matches and full-text matches, and are dropped when the next key is typed. If the helper, model or runtime is missing or fails, nothing is shown and the Plugins panel gives the reason.
* The helper is started on demand and stopped after 60 s without a call. At most 500,000 files; the job memory cap is 1 GiB.
* **Delete my meaning index** (Plugins panel) removes every vector and `vectors.log` and turns the plugin off, so nothing is rebuilt until you turn it on again.

## Tokenizer

A small WordPiece implementation (`plugins/semantic/src/tokenizer.rs`, about 150 lines, plus `unicode-normalization` for accent stripping) instead of the `tokenizers` crate, which brings a large dependency tree and a C regex library. It follows the model's BertNormalizer / BertPreTokenizer / WordPiece / `[CLS] … [SEP]` template. Verified against `tokenizers` 0.21.4: ids for 34 mixed sentences (accents, CJK, emoji, URLs, code, control characters, long words) are identical (`tests/model.rs`, fixture `tests/fixtures/tokens.json`). Known gap: punctuation outside ASCII, Latin-1 and the General Punctuation and CJK blocks is treated as a letter, not punctuation.
