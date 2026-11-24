# Privacy

Lookout runs on your PC. Your files, the search index, clipboard history, screenshots and settings stay on your PC and are never uploaded. There is no account, no telemetry and no analytics.

One thing uses the network:

**Semantic search model download (optional).** If you turn on "By meaning" search, Lookout downloads the model, its tokenizer and `onnxruntime.dll` from huggingface.co and files.pythonhosted.org. Your files and queries are not sent. Those hosts see the usual connection data (your IP address).

Nothing is indexed until you pick folders. "Delete my index" and "Delete my meaning index" in Settings remove what Lookout built, and the uninstaller can remove all application data.
