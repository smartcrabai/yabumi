# Yabumi Language Server

`ybm lsp` starts the Yabumi Language Server Protocol server on stdin/stdout. It uses standard
JSON-RPC 2.0 messages framed with `Content-Length` headers. The server uses no additional
command-line arguments and does not execute the open source file.

## Features

- Diagnostics after `textDocument/didOpen`, `textDocument/didChange`, and `textDocument/didSave`
- Whole-document formatting with `textDocument/formatting`
- Type hover with `textDocument/hover`
- Definition locations with `textDocument/definition`
- Full document synchronization (`textDocumentSync: 1`)
- UTF-16 positions by default; UTF-32 when the client advertises `general.positionEncodings` with
  `"utf-32"`

The server re-analyzes unsaved contents from the open-document overlay. An open `.ybm` document is
an analysis root, so same-directory `.ybm` files with a `module` directive are included; open sibling
overlays take precedence over files on disk. Close a document to remove that overlay. Formatting uses
Yabumi's canonical formatter and ignores client formatting options.
Malformed JSON, invalid requests, and requests for unknown methods receive JSON-RPC errors when
possible and do not by themselves terminate the server. A shutdown followed by `exit`, or stdin EOF, exits 0;
transport failures and `exit` before shutdown exit 1.

## Neovim

Neovim 0.11 or later:

```lua
vim.filetype.add({
  extension = {
    ybm = "yabumi",
  },
})

vim.lsp.config("yabumi", {
  cmd = { "ybm", "lsp" },
  filetypes = { "yabumi" },
})

vim.lsp.enable("yabumi")
```

## Helix

Add this to `languages.toml`:

```toml
[language-server.yabumi]
command = "ybm"
args = ["lsp"]

[[language]]
name = "yabumi"
scope = "source.yabumi"
file-types = ["ybm"]
language-servers = ["yabumi"]
```

## Visual Studio Code

Visual Studio Code does not provide a generic LSP client configuration in `settings.json`. Install
a generic LSP client extension, then configure that extension with the equivalent command and file
mapping. The values are:

```json
{
  "server": {
    "command": "ybm",
    "args": ["lsp"]
  },
  "filetypes": ["yabumi"],
  "extensions": [".ybm"]
}
```

Use the extension's own setting names for this equivalent configuration. No Yabumi-specific VS Code
extension is included.
