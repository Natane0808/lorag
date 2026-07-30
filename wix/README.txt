lorag — fully local RAG desktop GUI
====================================

lorag lets you ingest multi-format documents (PDF / DOCX / PPTX / XLSX / MD / TXT)
into a fully local vector database (LanceDB + SQLite) and ask questions using a
local LLM (aha / Qwen3). No cloud, no telemetry, no network calls.

Launch:
  - Double-click the "lorag" shortcut on your desktop or in the Start Menu.
  - The GPUI desktop launcher opens — start the local service from the Service tab,
    then click "Open Chat" to open the browser chat UI.

System requirements:
  - Windows 10 / 11 (64-bit)
  - GPU with DirectX 11/12 support recommended (integrated graphics work; CPU-only
    is slower but functional)
  - ~2 GB free disk space for the default 4B LLM + 0.6B embedding models

Data is stored under %LOCALAPPDATA%\lorag\ (this directory) and %APPDATA%\lorag\.
Uninstall from Settings -> Apps -> lorag to remove everything except your
downloaded models.

Project: https://codeberg.org/natane/lorag
License: MIT
