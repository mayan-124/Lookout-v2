# Privacy

Lookout runs on your PC. Your files, the search index, clipboard history, screenshots and settings stay on your PC and are never uploaded. There is no account, no telemetry and no analytics.

Two things use the network, and only when you ask:

**Semantic search model download (optional).** If you turn on "By meaning" search, Lookout downloads the model, its tokenizer and `onnxruntime.dll` from huggingface.co and files.pythonhosted.org. Your files and queries are not sent. Those hosts see the usual connection data (your IP address).

**Updates (optional, manual).** Lookout never checks for updates by itself. It connects to github.com only when you click "Check for updates" or "Download and install" in Settings, and it only receives files: a small `latest.json` describing the newest version, and the installer if you choose to install it. Nothing about you, your PC or your files is sent (no telemetry, no machine ID). The update's signature is checked before it is installed; if the check or the download fails, nothing changes. GitHub sees the usual connection data (your IP address).

The text of recent searches (at most 50, never text that looks like a secret) is kept encrypted on your PC. "Remember my searches" and "Delete search history" are in Settings > Privacy.

Notes you save with `note:` (at most 500 of 2,000 characters each) are kept in the same encrypted database on your PC, with keys and card numbers masked. They are not put in the file or content index, the search history, add-ons or the command line. The 20 most recently deleted notes stay there so a delete can be undone; "Delete all notes" in Settings > Privacy deletes every note, and "Delete my data" deletes them too. A note you copy goes to the Windows clipboard, so clipboard history can keep it.

**Copied images (off unless you turn it on).** "Keep copied images" in Settings > Privacy makes the Clipboard add-on keep the last 20 images you copy, each at most 5 MB (larger ones are not kept), for 24 hours or less. Each is stored encrypted next to your text clips. Images are **not** checked for secrets, so a screenshot of a password is kept as it is. The same pause, "Ignore next copy", ignored programs and password-manager exclusions apply as for text. An image is not recorded when the copy also holds text. Expired images are removed while the add-on runs, or the next time it starts. Deleting an image cannot be undone. "Delete clipboard history" (type `clip clear`) and "Delete my data" delete them, and so does turning the setting off.

Dragging an image out writes an **unencrypted** copy to `Lookout-drag` in your Windows temp folder, so the other program can read it. It is removed after an hour, when the image or the history is deleted, and when Lookout closes (after a crash, at the next start). Copies other programs made are theirs.

**Web search (off until you choose an engine).** When you press Enter on a web search result, the words you typed go to that engine (DuckDuckGo, Google or Bing) through your default browser; Lookout sends nothing while you type, and text that looks like a secret is never offered for search.

Nothing is indexed until you pick folders. "Delete my data" and "Delete meaning data" in Settings delete what Lookout built, and the uninstaller can remove all application data.
