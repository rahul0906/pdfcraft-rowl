# Developing PdfCraft

Working instructions for agents and contributors are in `AGENTS.md` and `CLAUDE.md`. This page
collects what the desktop app reads from its environment and where it writes its diagnostics.

## Low-disk Cargo profile

When local build storage is constrained, select the opt-in `low-disk` profile explicitly:

```sh
cargo build --profile low-disk -p pdfcraft-cli
cargo test --profile low-disk -p pdfcraft-cos --lib
```

The profile inherits the normal development settings, including debug assertions, overflow
checks and line-table-only debug information, and disables incremental compilation for this
profile only. Ordinary `cargo build` and `cargo test` keep incremental compilation.

The development profile itself keeps only line tables for workspace code and no debug information
for dependencies. Backtraces show file and line numbers but no local variables or parameters; for
full debug information in workspace code (for a debugger), build with
`CARGO_PROFILE_DEV_DEBUG=true`.

Cargo stores custom-profile artifacts separately under `target/low-disk`. Switching between the
normal and low-disk profiles can therefore leave both sets of artifacts on disk. To remove only
the low-disk artifacts, run `cargo clean --profile low-disk`.

Building PdfCraft requires Rust 1.95 or later: the UI crate and its `egui` and `craft-ui`
dependencies need it, and the workspace `rust-version` states the same minimum.

## AI Assistant on macOS

The [AI reading roadmap](ai-reading-roadmap.md) records the planned features, their order, and the tests required for each phase.

Open **Preferences → AI Providers**, add a profile with a base URL and model ID, then select
**Use provider**. The OpenAI preset uses `https://api.openai.com/v1`. Choose **Chat Completions**
or **Responses** according to the provider's documentation. For Meta's `muse-ember-1.0`, use
`https://api.meta.ai/v1` and **Responses**. For a server running on your Mac, you can use a loopback URL
such as `http://127.0.0.1:1234/v1` and leave the API key empty if the server supports that.
When supplied, the key is saved in macOS Keychain. **Test connection** uses sample text.

To use llama.cpp, start `llama-server` with a chat-capable GGUF model. For example, the
following uses the loopback address and port prefilled by PdfCraft. [llama.cpp server documentation](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md)

```sh
llama-server --model /path/to/model.gguf --host 127.0.0.1 --port 8080 --alias local-model --ctx-size 8192
```

If the server is already running, use its port. In **Preferences → AI Providers**, choose
**Add llama.cpp**, enter the server's API key if required, and click **Find models**. Choose
a model or enter its ID directly, then click **Save provider** and **Use provider**.
Start and stop the server separately.
For long requests, choose a page range that fits the model's configured context.

Open a PDF and choose **AI Assistant → Ask about this document**, **Summarize**, or
**Translate**. All three open the same sidebar. Review the provider and page range in the
composer at the bottom. Results appear above it, with a **Copy** button for each answer.

- **Ask** searches the selected pages locally. Open the scope menu to set a page range for
  **This PDF** or choose **Multiple PDFs…** to include up to six open PDFs. Hover over
  the compact source list to see the full filenames and page ranges. Requests send your
  question, matching excerpts, and up to four
  recent Ask turns from the same scope. A citation opens its PDF and highlights the passage.
  Retrieval covers up to 1,000 pages and 4 MiB of text in total.
- Highlight text and click **Ask AI** to ask about just the selection. The selection can span up
  to six pages and contain up to 7,200 bytes of extracted text. Press **Escape** to clear a text
  or snapshot selection and return to the current PDF. The question draft stays in the composer.
- **Summarize** sends text from the selected pages, up to 50 pages and 64 KiB per request.
- **Translate** starts with the current page when opening the conversation. Choose a
  target language from the dropdown, then select up to 10 pages or 12 KiB of text.
  The result is copyable text in the conversation.
- For scans, open **OCR settings** in **Ask** or **Translate**, enable **Recognize scanned text
  locally**, and choose the source language. PdfCraft uses the existing ocrs models for English. For the other source
  languages in the menu, choose **auto** or **tesseract** and install the matching Tesseract
  language pack. Ask recognizes up to 20 scanned pages per request. OCR leaves the PDF
  unchanged. Expand **Source text** after translating to review what OCR read, and click a
  citation to see its location on the page.
- For an individual figure, choose **Edit → Take a snapshot**, drag over the figure, and use
  **Ask about area…** or **Translate area…**. To ask about bars, lines or spatial relationships,
  enable **Include image for local vision** in Ask. This requires a loopback llama.cpp profile
  running a vision-capable model with its projector loaded. Text-only requests send recognized
  labels and captions to the selected provider. [llama.cpp multimodal setup](https://github.com/ggml-org/llama.cpp/blob/master/docs/multimodal.md)

In **Chat options**, enable **Save chats on this Mac** for a named document to save up to
24 turns per revision under the private `AI Chats` folder in PdfCraft's application support
directory. **New chat** clears the current conversation after confirmation. You can view
and delete older saved revisions in **Chat options**. **Stop** cancels local work before
sending where possible and discards the result of a request already sent.

CLI and MCP expose `ai_ask`, `ai_summarize`, and `ai_translate`. The first call previews the
endpoint and model. After the user confirms, repeat with `user_confirmed: true`,
`confirmed_endpoint`, and `confirmed_model`. Use `first_page` and `last_page` for
summaries and page-range translations. Translation requires `target_language`.
For multiple PDFs, pass `documents`, an array of open `doc` IDs in objects with optional
`first_page` and `last_page`. For selected text, pass `doc` and a `selection` with a 1-based
`page` and `rect` in displayed page points. For both forms, echo the preview's `scope_token`
as `confirmed_scope` along with the confirmed endpoint and model. The answer's citations
include a document ID, revision, and passage rectangles in displayed page points.
Set `ocr_language` (for example `"en"`) to enable local OCR in Ask or Translate.
Set `ocr_engine` to `"auto"`, `"ocrs"`, or `"tesseract"`. OCR requests also require
`confirmed_scope` from the preview. For area translation, pass `selection` in place
of the page range. OCR translation returns `source_passages` and an `ocr_report`.
For Ask, `vision: true` with `selection` and `ocr_language` sends the cropped image
and any labels to the selected loopback llama.cpp model.

Use `ai_llama_models` to list available models without sending a prompt or any PDF text.
It accepts a loopback `base_url` for a keyless server or the name of a saved llama.cpp
`provider` for a server with a Keychain credential. With neither argument it uses
`http://127.0.0.1:8080/v1`.

### Explain what changed between two PDFs

On macOS, open both versions and select the newer one. Choose **Compare Files**, pick the older
version, and set the optional page range in the newer PDF. In the Compare sidebar, click
**Explain what changed**, review the provider, and click **Send and explain**. Click an older or
newer page link to highlight the matching text in that version. The links for additions and
deletions appear on the version that contains the passage.

PdfCraft sends the changed words and short excerpts of their surrounding text so the provider can
prioritize them. PdfCraft builds the explanations and citations directly from the comparison.
Matching passages that appear as an exact deletion and insertion can be shown as moves. A
comparison with identical extracted text skips the provider request. Run **Scan & OCR** on
image-only PDFs before comparing their text. Each request covers up to 48 underlying changes and
16 KiB of evidence. Each PDF can have up to 250 pages.

In CLI and MCP, use `ai_compare` with `other` for the older open document ID and `doc` for the
newer ID. Optional `pages` is a list of 1-based pages in the newer PDF. Preview the scope and
provider, then repeat with `user_confirmed: true`, `confirmed_endpoint`, `confirmed_model`, and the
previewed `scope_token` as `confirmed_scope`. Results include the older and newer text with page
citations, and `used_ai` reports whether the provider supplied a usable ranking.

The macOS smoke test used LFM2.5 2.6B Q8_0 through a loopback llama.cpp server with
`--reasoning-budget 0`. It ranked a sample-size change from 100 to 120, an addition,
and a deletion in two synthetic PDFs. Both files stayed byte-for-byte unchanged.

### Check the fork after an upstream sync

From the synced checkout on macOS, run the focused AI regression tests. They use synthetic PDFs,
temporary preference files, and mock servers on loopback.

```sh
cargo test --locked -p pdfcraft-cli --test it ai_
cargo test --locked -p pdfcraft-engine --lib ai_
cargo test --locked -p pdfcraft-platform --lib ai::tests
cargo test --locked -p pdfcraft-platform --lib ai_chat::tests
cargo test --locked -p pdfcraft-ui-egui --lib ai_
cargo test --locked -p pdfcraft-ui-egui --test it ai_
```

The tests cover provider activation and reload, Chat Completions and Responses, model discovery,
the three document actions, request confirmation, citations, and chat history. The existing
macOS workspace run also includes them. [Nightly macOS workflow](../.github/workflows/nightly-macos.yml)

To exercise the CLI regression on a real image-only scan, install the pinned English
OCR models and point the test at them. The PDF is generated by the test. The provider
reply is deterministic.

```sh
models_dir="$(mktemp -d)"
cargo xtask models "$models_dir"
PDFCRAFT_MODELS="$models_dir" cargo test --locked -p pdfcraft-cli --test it ai_scanned_pdf
```

The macOS smoke test used the pinned ocrs models and llama.cpp with LFM2.5 2.6B Q8_0.
On a synthetic English scan it recovered a numeric answer with page rectangles and translated
the page into Spanish. A separate run sent a synthetic two-bar chart crop to SmolVLM 500M
Instruct Q8_0 with its projector and identified the taller bar. The scan's bytes matched before
and after the runs. The native app also highlighted the cited passage.

**AI Providers → Test connection** checks a live provider with its saved credentials.

## Annotation tool pin

The annotation quick bar's **Keep tool selected** pin is a shared preference, stored as the
boolean `comment_tool_pinned`. It survives application restarts, including when switched off.
Startup uses Select; the remembered pin applies when an annotation tool is picked. Older settings
without the key keep the default one-shot behavior. Non-boolean values are ignored, preserving
the current choice and the other valid settings.

The existing opt-in UI control channel uses the same preference:

```sh
pdfcraft-cli ui --control /path/to/control.json set key=comment-tool-pinned value=true
pdfcraft-cli ui --control /path/to/control.json state
```

`ui.state.comment_tool_pinned` reports the boolean. `ui.set` accepts `true` or `false` and refuses
other values before changing the preference. This changes interface settings only.

## Interface appearance

Display theme saves System, Light or Dark separately from the palette currently drawn. New
installations still default to Light. System follows native appearance events on macOS and
Windows. The native window keeps its inherited appearance in every mode; both egui style branches
use the selected application palette, so desktop changes cannot override a manual choice.

On Linux the native app starts an XDG Settings portal subscription when System is first selected.
It reads `org.freedesktop.appearance` / `color-scheme`, where numeric `1` requests Dark, `2` requests
Light, and `0` means no preference. When that states no preference or is missing, it falls back to
`org.gnome.desktop.interface` / `color-scheme` (`prefer-dark`, `prefer-light`, `default`), which
XFCE's portal answers instead of the freedesktop namespace; KDE answers only the freedesktop one. The subscription is installed before the initial read. Initial
results and later `SettingChanged` signals wake an idle window. There is no periodic desktop
polling, and signals received in a manual mode update the cache without requesting frames.

System falls back to Light if no appearance is available, including an absent portal, an
unsupported appearance backend, no preference, an unknown value or a failed connection. This
does not rewrite the saved System choice. The watcher cancels when its window closes; it does not
reconnect after a session-bus disconnection. Headless UI contexts use their supplied egui input and
do not contact a desktop session bus. Page and document colours retain their existing behavior.


## Logs

The desktop app writes its `log` records to standard error and to `logs/pdfcraft.log` in its
data folder: Linux and FreeBSD `~/.local/share/pdfcraft/logs/` (or
`$XDG_DATA_HOME/pdfcraft/logs/`), macOS `~/Library/Application Support/PdfCraft/logs/`, Windows
`%APPDATA%\PdfCraft\data\logs\`. In portable mode (a `portable.txt` file next to the executable, as
the Windows portable zip ships) the settings folder is `PdfCraftData\` beside the executable, and
crash recovery and new digital IDs go there too. A start from a desktop menu, Finder or the Start
menu has no terminal, so this file is what to attach to a bug report: a failed autosave, a page that
would not render, a render worker that could not start and the report of an internal error all land
there.
Each launch moves the previous log to `pdfcraft.1.log` (and that one to `pdfcraft.2.log`), so the
log of a run that crashed survives the next start. The file stops growing at 16 MiB. `--version`
writes no file.

By default PdfCraft's own crates (`pdfcraft*`) log at `info` and everything else at `warn`.
`RUST_LOG` replaces that with env_logger-style directives, for example `RUST_LOG=debug`,
`RUST_LOG=warn,pdfcraft_render=trace` or `RUST_LOG=info,wgpu_core=warn`; a directive ending in `*`
covers every target starting with it (`pdfcraft*=debug`). The logger is
`apps/pdfcraft/src/logging.rs`. It never records the control-channel token or document passwords.

## GPU surface limits

The desktop renderer enables the selected adapter's supported 2D texture extent, while keeping
egui-wgpu's other device requirements. Restored windows on mixed-DPI monitors can exceed its
default 8192-pixel extent even when the GPU supports a larger surface (#577). The enabled extent
never exceeds the adapter's capability. A window larger than even that (restored at another
monitor's scale, or stretched across monitors) gets a surface fitted within the limit and is drawn
at a correspondingly lower scale instead of panicking in `Surface::configure` (vendored egui-wgpu,
`surface_fit`; see `vendor/README.md`). On DX12, PdfCraft's Windows default, the surface is
stretched over the window: softer, but complete and lined up with the pointer. wgpu's GL backend
copies it unscaled into a corner of the window instead.

For blurry or incorrectly scaled UI reports, attach `pdfcraft.log`. Look for the successful
startup `renderer:` line, which records the wgpu adapter, device type, backend and enabled texture
limit, or the OpenGL renderer. Also look for the `drawing it at … and scaling it up` warning: it
means PdfCraft hit the surface-size safety fallback and rendered below the physical window size, so
softness is expected.

## Renderer fallback

PdfCraft draws with wgpu, and starts again with eframe's own OpenGL renderer (glow) when wgpu fails
before the app is created: no adapter, no device, or a window surface the device can't configure.
The last of these was a panic, not an error, because wgpu's default error handler panics and
eframe configures the surface before any app code runs; a 2015 Intel GPU whose driver timed out in
`Surface::configure` on wgpu's GL backend closed PdfCraft at launch (#519). Vendored egui-wgpu
configures a new window's surface inside `winit::catch_errors` and returns a validation,
out-of-memory or internal error as `WgpuError::ConfigureSurface` (see `vendor/README.md`). Still a
panic, as upstream: an error in a later reconfigure (resizing, a lost surface); a device lost
during that first configure (wgpu reports it only to a callback, so the first frame panics, after
the app has started); and a window shown at zero size, which is first configured on resize. If
OpenGL can't start either, PdfCraft exits with both errors in the log. The fallback is logged at
`error`, with the wgpu error. Renderer resource creation now uses the same scopes and returns
`WgpuError::CreateRenderer` before the app is created, so a rejected `egui_pipeline` can also
reach this retry instead of the uncaptured-error panic handler. This does not fix the failing
driver's shader compilation, and explicit `PDFCRAFT_RENDERER=wgpu` still disables the retry.
`PDFCRAFT_RENDERER` skips it or chooses OpenGL from the start.

## Preferences

Linux and FreeBSD store `app.ron` in `$XDG_CONFIG_HOME/pdfcraft/`, or
`~/.config/pdfcraft/` when that variable is unset, empty or relative. This follows the
[XDG Base Directory Specification](https://specifications.freedesktop.org/basedir/latest/).
On first launch after upgrading, existing `app.ron` preferences are copied from the old
data folder without replacing an existing configuration file. The old copy is retained;
logs, crash recovery and digital IDs stay in their data locations. If migration fails,
the app logs the failure and continues using the original preferences folder.
The existing PrintCraft-to-PdfCraft migration runs first. macOS, Windows and portable
copies retain their current preferences locations.

## Environment variables

| Variable | Effect |
|---|---|
| `RUST_LOG` | Log levels for standard error and the log file (see [Logs](#logs)) |
| `RUST_BACKTRACE` | `1` adds a backtrace to the report of an internal error |
| `XDG_CONFIG_HOME` | Linux/FreeBSD: absolute configuration base for `pdfcraft/app.ron`; defaults to `~/.config` |
| `XDG_DATA_HOME` | Linux/FreeBSD: base of the log folder and crash recovery; previous preferences are migrated from here |
| `WGPU_POWER_PREF` | GPU choice; by default PdfCraft draws on the GPU the Wayland compositor draws with (it says so through linux-dmabuf feedback), else the GPU that drives the primary display (Windows) or the built-in panel (Linux), else the low-power (integrated) GPU. The log says which one was chosen |
| `WGPU_BACKEND` | Graphics backend; by default Windows uses Direct3D 12, falling back to OpenGL |
| `WGPU_DX12_PRESENTATION_SYSTEM` | Windows, Direct3D 12: `hwnd` presents through a swap chain on the window itself, which Windows stretches while the window is resized (#2738); unset or `visual`, PdfCraft presents through a DirectComposition visual |
| `WGPU_VALIDATION_INDIRECT_CALL` | `1` turns wgpu's check of indirect draw arguments back on. PdfCraft draws nothing indirectly and turns it off, since the compute shader the check needs fails to compile on some drivers (Metal on an Intel Iris Pro), which lost the device at start-up |
| `PDFCRAFT_RENDERER` | `gl` starts with OpenGL (glow) and never loads wgpu; `wgpu` reports a wgpu failure instead of retrying with OpenGL; unset, wgpu is retried with OpenGL when it can't start (see [Renderer fallback](#renderer-fallback)) |
| `PDFCRAFT_TITLEBAR` | Linux: `app` draws PdfCraft's own title bar (window buttons in the tab strip, in the order of GNOME's `button-layout`, and resize edges); `system` keeps the frame winit or the desktop draws; unset, PdfCraft draws its own when the Wayland compositor draws no frames (GNOME), and leaves it to the desktop otherwise (KDE, Sway, Hyprland, X11). Where PdfCraft draws it, Preferences ▸ Use system title bar switches back to winit's frame without a restart; `system` also hides that option |
| `PDFCRAFT_BUILD_LABEL` | Build time: a label for a local or patched build (letters, digits, inner dots), shown after the version in `--version`, About and the log (`0.7.1 (titlebar)`). `packaging/linux/package.sh` also puts it in the file names and the .rpm/.deb release (`1.titlebar`), which upgrades over the release of the same version |
| `CRAFT_FONTS_DIR` | Build time: a [craft-fonts](https://github.com/storytold/craft-fonts) checkout to embed, including Noto Sans Devanagari for Hindi UI and translation results. Local Cargo builds default to the repository's gitignored `craft-fonts/` directory; an explicit environment value overrides it. |
| `PDFCRAFT_SYSTEM_FONTS` | `0` disables installed-font fallback for both interface text and non-embedded CJK page text in the desktop viewer, Combine thumbnails and accessibility figure previews (`cargo xtask screenshots` sets it). Document fallback is display-only: exports, printing, OCR and automation retain the bundled-font path. |
| `FONTCONFIG_FILE` | On Linux/Unix, the Fontconfig configuration file used to discover installed UI fallback fonts (default: `/etc/fonts/fonts.conf`); relative filenames are resolved under `FONTCONFIG_PATH` |
| `FONTCONFIG_PATH` | Directory for a relative `FONTCONFIG_FILE` (default: `/etc/fonts`; the first entry of a search path is used) |

On Linux/Unix, the UI's existing fallback filenames are also searched in Fontconfig's configured
directories and their subdirectories. This finds fonts in Nix store paths and user font directories
without invoking `fc-match` or linking libfontconfig. The scan is cached and bounded; missing or
malformed configuration leaves the ordinary `/usr/share/fonts` and `/usr/local/share/fonts`
candidates available. Font size and glyph-coverage checks still apply. This only reads installed
fonts for the interface; it does not embed them into PDFs or packages.

Relative includes resolve beside the config that contains them. Included directories load only
digit-prefixed `*.conf` files, in lexical order; backups and disabled files are ignored. The scan
limits each config to 1 MiB, visits at most 256 config files/directories, and bounds include depth
to 16 and directory entries to 32,768. Cyclic includes are skipped.
