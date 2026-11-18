# AI reading roadmap

This plan is for reading research papers and magazines in PdfCraft. Implement the phases below in order, starting only when the user explicitly asks to proceed. Use the existing AI Assistant and provider settings as the starting point. [Current AI workflow](development.md#ai-assistant-on-macos)

## 1. Ask across selected PDFs

Available on macOS. [Usage and regression commands](development.md#ai-assistant-on-macos)

Let the reader choose which open PDFs to include in a conversation. Search those documents for relevant passages and show which files are in scope before sending the question. Each citation should identify the file and page and open the document at the highlighted passage. When two sources disagree, show both claims with their evidence. If the selected sources give insufficient evidence, say so.

Add **Ask AI** to the text selection menu as a shortcut into the same assistant. The selected text is the starting scope. The reader can explicitly broaden it to the current document or other selected PDFs. For example, a reader can highlight a methods paragraph in one paper and ask how a second paper measured the same outcome.

Extend the existing local passage retrieval to carry document, revision, page, and text-position information. Start with the current keyword retrieval. Test questions about paraphrases and terminology before deciding whether to add local embeddings.

**Required tests**

- Use three synthetic PDFs with overlapping terminology. Select two, verify that both can support a cited answer, and assert that the third PDF's text never enters the model request.
- Cover conflicting passages, unsupported questions, and citations that open and highlight the right passage even when two files have the same name.
- Cover **Ask AI** from a text selection and verify that a selection-only request sends only that selection.

## 2. Ask and translate scanned PDFs with local OCR

Available on macOS. [Usage and regression commands](development.md#ai-assistant-on-macos)

Offer to recognize text when a reader asks about or translates a scanned page. Use PdfCraft's existing local OCR engines for the requested page range or region. Reuse native text where available and keep recognized text and word boxes in a temporary cache tied to the document revision. Show translations beside the source and link answers back to the recognized words. Prioritize multi-column papers, captions, and magazine layouts.

Add region selection for photos, figures, and graphs. For example, the reader could drag around a chart and ask which series rises fastest or request a translation of the labels. Use OCR for readable labels and captions. When a compatible local vision model is configured, pass it the selected crop for questions about lines, bars, and spatial relationships. Report which details the available evidence could support, especially when labels are blurry or absent.

Reuse the read-only OCR word and region paths and the existing page renderer. Keep text-only providers usable for questions grounded in OCR text, and show when a question needs an image-capable model.

**Required tests**

- Cover image-only pages, mixed text and scan pages, multi-column reading order, and a cropped chart. Verify language choice, citation positions, and the scope sent to the provider.
- Cover OCR cancellation, unavailable OCR engines, low-confidence text, and the distinction between translation of labels and a question that requires interpreting the image.
- Run a smoke test with an installed local OCR model on a synthetic scan. For the figure workflow, also smoke-test a compatible local vision model on a synthetic graph.

## 3. Explain what changed between two PDFs

Let the reader select an older and a newer version from the existing comparison view and ask for an explanation. Use PdfCraft's text diff as the evidence. Prioritize changes such as sample sizes, definitions, methods, and conclusions. Link each explained change to the relevant passages in both versions, with a single source for insertions and deletions. For example, a reader could compare a preprint with the published paper and ask what changed in the reported sample size.

**Required tests**

- Use synthetic before-and-after PDFs with a changed numeric result, an insertion, a deletion, and an unchanged passage. Verify the direction and source links of each reported change.
- Cover identical inputs, moved text, and a model reply that claims a change outside the diff.

## Implementation and testing instructions

- Add regression tests as part of each phase. Use synthetic PDFs and deterministic mock provider replies for automated runs. Keep real-model smoke tests separate and record the model and scenario that ran.
- Expose each new reading workflow through the engine and the shared CLI/MCP tool table. Add an end-to-end tool test and GUI regression coverage for new selection, review, or citation interactions. Record passing tests in the parity ledger as required by the repository rules.
- Rerun the existing macOS AI regression suite alongside the new tests after each phase and after upstream syncs. [Regression commands](development.md#check-the-fork-after-an-upstream-sync)
