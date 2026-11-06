---
name: docx
description: Read, create, and edit Word documents (.docx), including tables, comments, and formatting; convert legacy .doc files before editing.
---

# Word documents

Write a small task-specific script when needed. Keep the original and save edits to a new file unless the user requested replacement. Decide whether the task needs plain content, review markup, or faithful layout before selecting a tool.

## Choose available tools

Check `command -v pandoc textutil python3 uv node soffice libreoffice`. On macOS, also check `/Applications/LibreOffice.app/Contents/MacOS/soffice` if no CLI is on PATH. Check installed Python libraries with `python3 -c "import importlib.util; print(importlib.util.find_spec('docx'))"`; for Node, check the chosen package with `node -e "console.log(require.resolve('docx'))"` from its project directory.

- Use pandoc for content extraction or generating ordinary documents from Markdown. Use `python-docx` for structured paragraphs, tables, styles, and targeted edits. Node's `docx` package is another generation option; do not assume it round-trips arbitrary existing documents.
- With uv, run a script in a temporary dependency environment: `uv run --no-project --with python-docx python edit_docx.py`. Without uv, use `python3 -m venv /tmp/docx-env-UNIQUE`, then that environment's Python and pip. Do not install into global Python or Node; tell the user before any global installation. A temporary Node project with local dependencies is also suitable.
- If no library is available, Python's `zipfile` and XML parser can read OOXML and make narrow edits. If only `unzip` is available, inspect package parts with `unzip -l input.docx` and `unzip -p input.docx word/document.xml`; XML is evidence, not a finished text extraction. If no usable tools exist and installation is unavailable, explain the missing tool and request an exported PDF or plain text. Do not claim to have read binary bytes.

## Read

For an initial text view:

```sh
pandoc input.docx -t gfm -o extracted.md
# macOS alternative for text only:
textutil -convert txt -stdout input.docx
```

These views can lose review metadata, floating objects, and layout. For structured reading, load `Document('input.docx')`, iterate `iter_inner_content()` to retain paragraph/table order, and descend into table cells (including nested tables). Inspect each section's headers and footers separately; include image descriptions or image inspection when relevant. Do not summarize a table as an unordered bag of text.

A `.docx` is a ZIP package. `word/document.xml` holds the main body: `w:p` paragraphs, `w:r` runs, `w:t` text, and `w:tbl` tables. Text may be split across runs even inside one word. Preserve tabs (`w:tab`), line breaks (`w:br`), cell boundaries, and hyperlinks when extracting. `word/styles.xml`, `numbering.xml`, and section properties govern appearance. Follow relationship files such as `word/_rels/document.xml.rels` for linked parts instead of assuming filenames.

Read comments via `Document.comments` when the installed python-docx supports it, or inspect `word/comments.xml`: match comment IDs to `w:commentRangeStart`, `w:commentRangeEnd`, and `w:commentReference` in the body. Check `word/footnotes.xml`, `endnotes.xml`, and related header/footer parts if present. Review tracked insertions (`w:ins`) and deletions (`w:del`, `w:delText`) explicitly; distinguish original, accepted, and marked-up text instead of silently mixing them. Plain paragraph APIs may omit text inside revision markup or text boxes. Embedded equations use Office Math (`m:oMath`); a plain-text converter may not preserve their meaning.

## Create

For a prose draft, use `pandoc draft.md -o output.docx`; if a reference document is provided, add `--reference-doc=reference.docx`. For finer control, a small python-docx script can start with:

```python
from docx import Document
from docx.shared import Inches, Pt

doc = Document()
doc.styles['Normal'].font.size = Pt(11)
doc.sections[0].left_margin = Inches(1)
doc.sections[0].right_margin = Inches(1)
doc.add_heading('Project report', 0)
doc.add_heading('Results', 1)
doc.add_paragraph('A concise explanation of the results.')
table = doc.add_table(rows=1, cols=2)
table.style = 'Table Grid'
table.rows[0].cells[0].text = 'Measure'
table.rows[0].cells[1].text = 'Value'
doc.save('output.docx')
```

Use actual heading and list styles, deliberate page size/margins, consistent spacing, and sensible table widths. Keep headings with following paragraphs, avoid manual spaces for alignment, and use page breaks only when needed. Check fonts available on the recipient's system. Add captions, headers, footers, or page numbers only when the document calls for them.

## Modify without flattening formatting

Load the existing file and change only the requested elements. Editing a specific `run.text` preserves that run's formatting; assigning `paragraph.text` or `cell.text` replaces their runs and can lose emphasis, links, and review anchors. If a match spans runs, map character offsets to runs, update the affected text nodes, and retain surrounding formatting and markers. Do not globally replace substrings in raw XML.

For documents with unsupported features, prefer a narrow ZIP/XML edit: read package entries, parse only the target part with a namespace-aware parser, change the intended nodes, and write a new ZIP with all other entries unchanged. Keep namespaces and compatibility prefixes, relationships, content types, IDs, and `xml:space="preserve"` for significant surrounding spaces. Adding parts requires updating `[Content_Types].xml` and relationships; a text-only edit normally does not. Preserve original non-text children in runs. A library save is not proof that drawings, revision history, or embedded objects survived.

## Older and Apple formats

For `.doc`, use macOS `textutil -convert docx -output converted.docx input.doc`, or an installed LibreOffice with `soffice --headless --convert-to docx --outdir converted input.doc`. Create a separate output directory first, confirm conversion produced a new file, then inspect it before editing. Renaming the extension is not conversion; converters may change layout or review data.

For Pages, Keynote, or Numbers, ask the user to export on their Mac: Pages to DOCX or PDF, Keynote to PPTX or PDF, Numbers to XLSX or PDF. Use an editable Office export for modifications; PDF is suitable for reading or checking appearance. Do not parse the native iWork package as OOXML.

## Verify before delivery

Reopen the saved document and read the changed paragraphs, tables, comments, and headers/footers as applicable. Confirm intended edits and retention of important untouched content. Run `python3 -m zipfile -t output.docx`; also parse every `.xml` and `.rels` entry with an XML parser. ZIP integrity and well-formed XML do not establish layout correctness.

If a renderer is installed, export a separate PDF with `soffice --headless --convert-to pdf --outdir preview output.docx` and inspect pages for clipping, broken tables, page breaks, and font substitution. Otherwise report that content/package checks passed but visual fidelity was not checked. Return the output path and disclose any conversion losses or unsupported review features.

API references, when needed: [python-docx quickstart](https://python-docx.readthedocs.io/en/latest/user/quickstart.html), [comments](https://python-docx.readthedocs.io/en/latest/user/comments.html), [uv script environments](https://docs.astral.sh/uv/guides/scripts/).
