<div align="center">
  <img src="assets/icon.svg" alt="paper" width="128">
</div>

# paper

`paper` is a small **native** tool to help retouch PDFs: modify and add text and images. It is a single Rust binary built on [`egui`](https://github.com/emilk/egui) for the UI and [PDFium](https://pdfium.googlesource.com/pdfium/) (via [`pdfium-render`](https://crates.io/crates/pdfium-render)) for rendering. Pages are rasterized lazily on a background thread as you scroll, so even a large scan opens quickly.

## Quick guide

**Open a PDF.** Drag a file onto the window, open one from inside the app, or pass it on the command line: `paper /path/to/file.pdf`. Linux, macOS and Windows are all supported.

**Move around.** Zoom with the toolbar buttons, with `Ctrl/Cmd` + scroll (which zooms toward the cursor), or with `Ctrl/Cmd` `+` and `-`; there is also fit-to-width. Drag to pan. Move between pages with prev/next or jump-to-page, or click a page in the **thumbnail sidebar**, which tracks wherever you are.

**Add text and images.** Drop in **text** and **image** overlays, then select and drag to move them (or nudge with the arrow keys), resize from the corner handles, rotate with the rotation handle, or remove with `Delete`. The properties panel sets position, size and rotation, plus the font, size and colour of text.

**Adding non-Latin text.** Latin text uses the built-in fonts or any of the document's own fonts. For other scripts (Cyrillic, Greek and similar) the text is rendered automatically with a bundled Unicode font (DejaVu Sans), so it shows up correctly in both the preview and the export, whichever font is selected. Two current limits: characters that font does not cover, such as Chinese, Japanese and Korean, are not supported yet and will not appear (the editor shows a note under the font selector when this happens); and non-Latin text always uses the bundled font, so it may not match the look of the document's own fonts.

**Edit what is already in the PDF.** Switch to the **page-object editor**. It outlines the document's own objects (text, paths, images and more); click one to select it, and click again to cycle through anything stacked underneath. Edit it directly on the page (drag, corner handles, rotation circle) or from the panel: move, scale, rotate, flip, fill and stroke colour and opacity, stroke width, the **text content**, font size, z-order (front, forward, backward, back), **swap an image**, or mark it for deletion. You can also **duplicate** an object by right-clicking the selected one; each copy is independent, so you can move and restyle it on its own. The page updates live, and **Reset** reverts your changes.

**If edited text looks wrong.** A PDF usually carries only the slice of each font that the document already uses, so editing text that is already on the page is not always perfect: a character you type might be missing from that font (it shows up as a box or the wrong shape), or editing one word can nudge other letters in the same block, such as flipping their upper and lower case. When that happens, the easiest fix is to leave the original text alone and instead **add a new text overlay** and retype it, then delete or cover the old text underneath. The page preview shows exactly how the file will export, so you can check the result before saving.

**Undo and redo.** `Ctrl+Z`, `Ctrl+Shift+Z` or `Ctrl+Y`, covering both overlays and page-object edits one step at a time.

**Export.** Save a flattened copy with every edit baked in.

## Licensing

All first-party code is **MIT**. Key dependencies and their licenses, all MIT-compatible:

- `eframe` / `egui` / `egui-phosphor`: MIT / Apache-2.0 (Phosphor Icons: MIT)
- `pdfium-render`, `lopdf`, `image`, `rfd`: MIT (or MIT / Apache-2.0)
- PDFium runtime binaries (from [`bblanchon/pdfium-binaries`](https://github.com/bblanchon/pdfium-binaries)): BSD-3-Clause, plus bundled third-party notices

## Build & run

Requires Rust 1.78+ (developed with 1.88) and the usual native GUI deps.

```bash
cargo run --release            # or: cargo run --release -- /path/to/file.pdf
```

### PDFium runtime

A Linux x64 build of `libpdfium` is vendored under `pdfium/linux-x64/`. For other
platforms, drop the matching prebuilt library where the loader looks for it:

```
pdfium/linux-x64/lib/libpdfium.so
pdfium/linux-arm64/lib/libpdfium.so
pdfium/macos-x64/lib/libpdfium.dylib
pdfium/macos-arm64/lib/libpdfium.dylib
pdfium/win-x64/lib/pdfium.dll
pdfium/win-arm64/lib/pdfium.dll
```

`scripts/fetch-pdfium.sh <platform>` downloads the right archive from
`bblanchon/pdfium-binaries` into the correct path. If no bundled library is
found, `paper` falls back to a system-installed PDFium.

### Platform packages

- **Linux (Debian/Ubuntu):** `sudo apt install build-essential libxkbcommon-dev libwayland-dev`. The file dialog uses the XDG Desktop Portal, so a running `xdg-desktop-portal` (with a backend such as `xdg-desktop-portal-gtk`/`-gnome`/`-kde`) is needed at runtime for Open/Save (already present on most desktops).
- **macOS:** `xcode-select --install`
- **Windows:** Visual Studio Build Tools with the C++ workload, and the Rust MSVC toolchain.

### Running a downloaded macOS build

Release binaries are not yet code-signed or notarized, so macOS quarantines a downloaded build and Gatekeeper reports that it "cannot check it for malicious software" (this blocks the bundled `libpdfium.dylib`, so PDFium fails to load). Clear the quarantine flag once on the extracted folder, then open it normally:

```bash
xattr -dr com.apple.quarantine /path/to/paper-macos-arm64
```
