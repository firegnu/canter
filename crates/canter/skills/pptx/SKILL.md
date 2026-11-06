---
name: pptx
description: Read, create, and edit PowerPoint presentations (.pptx), including slide text, tables, speaker notes, and layout; convert legacy .ppt files first.
---

# PowerPoint presentations

Write a task-specific script to inspect or change the deck. Work on a copy and retain the original unless replacement is requested. Separate content extraction from layout inspection: extracted text cannot reveal overlaps or clipped text.

## Choose available tools

Check `command -v python3 uv node pandoc textutil soffice libreoffice`. On macOS, check `/Applications/LibreOffice.app/Contents/MacOS/soffice` as well. Inspect installed Python support with `python3 -c "import importlib.util; print(importlib.util.find_spec('pptx'))"`, or Node support with `node -e "console.log(require.resolve('pptxgenjs'))"` in the intended project.

- `python-pptx` can read, generate, and edit supported presentation objects. PptxGenJS is useful for generating new decks in Node; do not treat it as an arbitrary deck editor. Pandoc can generate simple decks from Markdown; textutil is not a PowerPoint converter.
- Prefer `uv run --no-project --with python-pptx python edit_pptx.py` for temporary Python dependencies. Without uv, use `python3 -m venv /tmp/pptx-env-UNIQUE` and install inside it. For Node, use a temporary project and local dependencies. Tell the user before installing anything globally.
- If no presentation library is available, Python's `zipfile` and XML facilities can extract parts or make narrow edits. `unzip -l input.pptx` is useful for inventory. If there is no usable tool and temporary installation cannot be done, explain what is missing and ask for a PDF export for reading. Do not invent slide contents.

## Read slides, notes, and comments

Load `Presentation('input.pptx')` and iterate `prs.slides` in presentation order. For each slide, record the title, shape text, tables (`shape.has_table`), chart data when available (`shape.has_chart`), and images. Descend into grouped shapes. Shape order is not necessarily reading order; use positions and grouping for interpretation. For text, retain paragraph levels and run formatting when they carry meaning.

Check `slide.has_notes_slide` before accessing `slide.notes_slide.notes_text_frame`; accessing a missing notes slide creates one. A notes text frame can be absent. Keep speaker notes separate from visible text, slide numbers, and master placeholders. Image-only slides need image inspection or available OCR; text extraction alone is incomplete.

OOXML layout guide:

- `ppt/presentation.xml` lists slides through relationship IDs. Resolve them in `ppt/_rels/presentation.xml.rels`; do not sort slide filenames to determine order.
- `ppt/slides/slide*.xml` contains shapes and DrawingML text (`a:p`, `a:r`, `a:t`). Positions and dimensions use EMUs (914400 per inch). Styles may inherit from layouts, masters, and themes, not the slide itself.
- Slide `.rels` files link to notes, layouts, media, charts, and comments. Read linked `ppt/notesSlides/` and `ppt/comments/` parts and author metadata where present. Newer threaded comments may use different parts/namespaces: inspect relationships and content types instead of assuming one comment API covers them.
- Tables are DrawingML `a:tbl`; chart parts can contain cached values and links to embedded workbooks in `ppt/embeddings/`. Report whether numbers came from a cache or the workbook. Preserve both when editing chart data.

## Create a readable deck

Plan slide titles and the main point of each slide first. Choose a consistent slide size, margins, type scale, theme, and alignment. Use a supplied template when available; its layout indices and placeholders may differ from the library defaults. Avoid shrinking body text to fit an overcrowded slide; split or simplify the content. Include speaker notes when useful.

Minimal python-pptx starting point using its default template:

```python
from pptx import Presentation
from pptx.util import Inches, Pt

prs = Presentation()
prs.slide_width, prs.slide_height = Inches(13.333), Inches(7.5)
slide = prs.slides.add_slide(prs.slide_layouts[6])
box = slide.shapes.add_textbox(Inches(0.7), Inches(0.5), Inches(12), Inches(1))
run = box.text_frame.paragraphs[0].add_run()
run.text = 'Project update'
run.font.size = Pt(32)
run.font.bold = True
slide.notes_slide.notes_text_frame.text = 'Introduce the main result.'
prs.save('output.pptx')
```

For a text-oriented deck, `pandoc slides.md -t pptx -o output.pptx` is another path; add `--reference-doc=template.pptx` when appropriate. Either way, review slide density and image scaling after generation. Use real table cells for tabular data and preserve image aspect ratios.

## Modify existing decks

Identify slides by title/content and relationship, then locate specific shapes. Modify existing runs when retaining text styling matters. Assigning `shape.text` or `text_frame.text` rebuilds paragraphs/runs and can remove styling or links. For multi-run matches, map the text to the affected runs and preserve formatting outside the replacement. Keep slide size, master/layout links, shape positions, crop settings, and notes unless the requested change affects them.

For advanced decks with animations, transitions, embedded objects, or unsupported comments, use a narrow ZIP/XML update instead of recreating the presentation. Copy all entries unchanged except the selected part; parse XML with namespace awareness, preserve compatibility namespace declarations, relationship IDs, content types, and unknown elements. Do not use broad XML string replacement. Added images, slides, or parts need correct relationships and content-type declarations. Library round-tripping does not guarantee full fidelity; compare important unsupported parts before and after.

## Older and Apple formats

Convert `.ppt` using an installed LibreOffice: create a separate directory, then run `soffice --headless --convert-to pptx --outdir converted input.ppt`. Alternatively ask for a PowerPoint export. Confirm a real converted file exists before editing; conversion can change fonts, animations, and layout. Renaming a binary `.ppt` to `.pptx` does not work.

For Pages, Keynote, or Numbers, ask the user to export on their Mac: Pages to DOCX or PDF, Keynote to PPTX or PDF, Numbers to XLSX or PDF. Request PPTX when slide editing is needed; PDF supports content/visual review but not faithful editable reconstruction.

## Verify before delivery

Reopen the output, compare slide count/order, changed text, tables, notes, and important media with the requested result. Run `python3 -m zipfile -t output.pptx` and parse all `.xml`/`.rels` entries. For XML edits, compare untouched package entries and check that internal relationships still resolve.

If possible, render a PDF with `soffice --headless --convert-to pdf --outdir preview output.pptx`, then inspect every slide for overflow, overlaps, contrast, and missing fonts/images. A successful ZIP check does not prove a slide looks right. If no renderer is installed, state that visual layout remains unverified and return the editable file with the content checks actually performed.

API references, when needed: [python-pptx quickstart](https://python-pptx.readthedocs.io/en/latest/user/quickstart.html), [speaker notes](https://python-pptx.readthedocs.io/en/latest/user/notes.html), [uv script environments](https://docs.astral.sh/uv/guides/scripts/).
