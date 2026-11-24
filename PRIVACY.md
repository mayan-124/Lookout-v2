# Privacy

Lookout runs on your PC. Your files, the search index, clipboard history, screenshots and settings stay on your PC and are never uploaded. There is no account, no telemetry and no analytics.

Two things use the network, and only when you ask:

**Semantic search model download (optional).** If you turn on "By meaning" search, Lookout downloads the model, its tokenizer and `onnxruntime.dll` from huggingface.co and files.pythonhosted.org. Your files and queries are not sent. Those hosts see the usual connection data (your IP address).

**Updates (optional, manual).** Lookout never checks for updates by itself. It connects to github.com only when you click "Check for updates" or "Download and install" in Settings, and it only receives files: a small `latest.json` describing the newest version, and the installer if you choose to install it. Nothing about you, your PC or your files is sent (no telemetry, no machine ID). The update's signature is checked before it is installed; if the check or the download fails, nothing changes. GitHub sees the usual connection data (your IP address).

Nothing is indexed until you pick folders. "Delete my index" and "Delete my meaning index" in Settings remove what Lookout built, and the uninstaller can remove all application data.
