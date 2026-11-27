# Lookout

A local-first command bar for Windows. Find files and apps by name, by the text inside them (including text in screenshots) or by when you changed them, and run a few actions, with a preview, confirmation and undo where they apply. Windows 10/11 x64 only. Early version (0.0.1).

## Install

1. Download `Lookout_<version>_x64-setup.exe` from this repository's [Releases](https://github.com/mayan-124/Lookout-v2/releases) page, or [build it yourself](#build).
2. Run it. It installs for your Windows account only, without administrator rights. The installer is not code-signed, so Windows SmartScreen may warn. If the Microsoft Edge WebView2 runtime is missing, the installer downloads it from Microsoft.
3. On first run, choose the folders Lookout may index. Nothing is indexed until you choose.

The optional "By meaning" search also needs the Microsoft Visual C++ 2015-2022 x64 runtime, which the installer does not include. Most PCs already have it.

Uninstall from Windows Settings. Your Lookout data stays unless you tick the "delete the application data" box. Screenshots and recordings you saved are not removed.

## Hotkey

`Alt+Space` opens and hides the bar. Change it in Settings > General (`Ctrl+,` opens Settings). Other launchers often use `Alt+Space` too.

## Privacy

- **Local-first.** Your files, searches and index are not uploaded. There is no account, telemetry or analytics.
- **What is stored**, on your PC: the file-name and text indexes, usage history, undo log, recent searches, notes, and, if you turn those add-ons on, clipboard history and meaning vectors. These are encrypted with a key that only your Windows account can unlock (DPAPI). Settings (including folder names), add-on approvals, error logs, downloaded model files and the screenshots and recordings you save are not encrypted. Text that looks like a secret (keys, card numbers, passwords) is masked before it is indexed.
- **Network use.** Lookout connects to the internet only when you ask it to: **Check for updates** or **Download and install** (github.com), and the optional model download for "By meaning" search (huggingface.co and files.pythonhosted.org). It only receives files. Separately, the installer may download WebView2 (see above), web addresses you open go to your browser, and Windows components such as WebView2 follow Microsoft's own settings.
- Add-ons are separate programs with your Windows permissions; Lookout does not sandbox them.

Details: [PRIVACY.md](PRIVACY.md).

## Known limits

- Text is read from plain-text and code files, `.docx`, `.xlsx`, `.pptx`, PDFs with a text layer, and images (Windows OCR, needs an OCR language installed). Not from `.doc`, `.xls`, `.ppt`, scanned PDFs or e-mail files.
- Screen capture may show some protected or GPU-drawn windows as black.
- Not yet tested on a clean PC.

## Report a bug

1. In Lookout, open **Settings > Advanced > Repair** and click **Copy diagnostics**. It copies versions, display facts, counts, which add-ons are on and recent error lines, with no file names, paths, searches or file text.
2. Open an [issue](https://github.com/mayan-124/Lookout-v2/issues), say what you did, what you expected and what happened, and paste the diagnostics. Issues are public, so read what you paste.

## Build

Needs Rust stable (MSVC toolchain), the Visual Studio C++ build tools and Node.js 22.

```powershell
npm --prefix ui ci
cargo build --release -p lookout-clipboard -p lookout-files-tools -p lookout-semantic -p lookout-office-text -p lookout-screenshot -p lookout-fastscan -p lookout-cli
powershell -File scripts\stage_plugins.ps1 release
npm --prefix ui run tauri build -- --config src-tauri/tauri.installer.conf.json
```

More: [architecture](docs/architecture.md), [add-ons](docs/plugins.md), ["By meaning" search](docs/semantic.md), [measurements](docs/benchmarks.md).

## Licence

Copyright (c) 2026 mayan-124. [PolyForm Noncommercial 1.0.0](LICENSE): free for personal and other noncommercial use. Commercial use needs a separate licence from the copyright holder. Third-party software: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
