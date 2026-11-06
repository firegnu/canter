---
name: xlsx
description: Read, analyze, create, and edit Excel workbooks (.xlsx), including formulas, tables, comments, and formatting; convert legacy .xls files before editing.
---

# Excel workbooks

Write a small script suited to the workbook and requested analysis. Preserve the original and write a new output unless replacement was requested. Inspect sheet names, dimensions, formulas, and special features before choosing how to edit.

## Choose available tools

Check `command -v python3 uv node pandoc textutil soffice libreoffice`; on macOS also check `/Applications/LibreOffice.app/Contents/MacOS/soffice`. Check existing libraries with `python3 -c "import importlib.util; print(importlib.util.find_spec('openpyxl'))"`, or `node -e "console.log(require.resolve('exceljs'))"` in the intended Node project.

- Use openpyxl for structured `.xlsx` reading, generation, and edits. ExcelJS is a Node alternative. XlsxWriter is useful for generation, but does not edit existing workbooks. Pandoc and textutil are not Excel workbook converters. CSV loses sheets, formulas, types, and formatting.
- Prefer `uv run --no-project --with openpyxl python edit_xlsx.py`. Without uv, use `python3 -m venv /tmp/xlsx-env-UNIQUE` and its Python/pip. A temporary Node project can hold local dependencies. Tell the user before any global installation; do not modify the global environment silently.
- Without these libraries, Python's ZIP/XML facilities can read `.xlsx` internals or make narrow edits. `unzip -l input.xlsx` can inventory parts. If no usable tools exist and temporary installation is unavailable, explain the limitation and ask for an export (CSV per sheet for values, PDF for layout, or an accessible XLSX). Never pretend a CSV preserves a workbook.

## Read and analyze

Load twice when both formulas and saved results matter: `load_workbook('input.xlsx', data_only=False)` exposes formula expressions, while `data_only=True` exposes the last cached results. Those results may be missing or stale; neither openpyxl nor a ZIP parser calculates formulas. Do not present cached numbers as newly verified calculations, and never save the `data_only=True` copy over a formula workbook.

Inspect sheet names/order, hidden sheets/rows/columns, used ranges, merged cells, tables, filters, named ranges, hyperlinks, cell comments, and number formats as relevant. Iterate cells to retain coordinates, `cell.value`, `data_type`, `number_format`, and `cell.comment.text`/`author`. Use normal mode when inspecting comments or editing; `read_only=True` is useful for large value scans but does not expose all features. Bound the rows/columns actually needed, since inflated worksheet dimensions can otherwise cause enormous scans.

Keep numeric values separate from displayed formats: dates may be serials with an epoch, percentages store fractions, and identifiers with leading zeros are text. Distinguish blank cells, empty strings, zero, and formula errors. Preserve units and sheet/cell references in analysis. Check totals independently with explicit arithmetic where feasible, without claiming to evaluate arbitrary Excel formulas.

OOXML layout guide:

- `xl/workbook.xml` names and orders sheets. Resolve its relationship IDs through `xl/_rels/workbook.xml.rels` to worksheet parts; filenames alone do not identify sheets.
- Worksheet cells (`c`, coordinate `r`) hold a type (`t`), style index (`s`), value (`v`), and optional formula (`f`). A shared-string value indexes `xl/sharedStrings.xml`; inline strings use `is`. Shared strings may contain rich-text runs. Do not treat all `<v>` values as literal text or numbers.
- `xl/styles.xml` defines number formats and styles; workbook settings select the 1900/1904 date system. Shared/array/dynamic formulas need special handling rather than rewriting each cell independently.
- Follow sheet relationships to tables, drawings, and comment parts. Legacy comments and modern threaded comments differ; inspect threaded-comment/person parts if a library omits them. Chart caches, external links, pivot caches, and embedded objects may contain data beyond visible cells.

## Create

For a small styled workbook:

```python
from openpyxl import Workbook
from openpyxl.styles import Font, PatternFill

wb = Workbook()
ws = wb.active
ws.title = 'Budget'
ws.append(['Item', 'Amount'])
ws.append(['Supplies', 25])
ws.append(['Total', '=SUM(B2:B2)'])
for cell in ws[1]:
    cell.font = Font(bold=True, color='FFFFFF')
    cell.fill = PatternFill('solid', fgColor='24476B')
ws.column_dimensions['A'].width = 24
ws.column_dimensions['B'].width = 16
ws['B2'].number_format = ws['B3'].number_format = '#,##0.00'
ws.freeze_panes = 'A2'
wb.save('output.xlsx')
```

Use descriptive sheet names, a clear header row, appropriate number/date formats, readable widths, and filters or tables where useful. Keep formulas for derived values instead of unexplained hardcoded totals. Use English formula function names and comma argument separators in the file. Store literal user text as text when it starts with `=` and is not intended as a formula.

## Modify while preserving structure

Open the original with `data_only=False`, change only target cells, and save a copy. Existing cell styles normally survive value assignments; to copy styles between cells, use `copy.copy` on style objects rather than mutating shared styles. Preserve merges, row heights, column widths, validations, conditional formatting, print settings, and named ranges. Only the top-left cell of a merged range holds its value.

Inserting, deleting, or moving ranges does not automatically repair every formula, chart, table, or external reference. Inspect dependencies and update them deliberately. For macro-enabled workbooks, keep the correct extension and use appropriate macro-preserving options only when supported; never silently strip macros. Do not run macros or refresh external connections just to read a workbook. Openpyxl does not preserve every object, and legacy comment formatting can be lost. If fidelity of unsupported objects matters, patch only the intended ZIP/XML part and copy all other entries unchanged, preserving namespaces, compatibility declarations, relationships, and content types. Shared-string changes can affect many cells, so add a distinct string or use a correctly encoded inline string when changing just one cell.

After changing formulas or their inputs, cached values may be invalid or absent. An installed spreadsheet application can recalculate a copy, for example `soffice --headless --convert-to xlsx --outdir recalculated output.xlsx` in a separate directory. Confirm an output was produced and check results; unsupported functions can still differ from Excel. Without a calculation engine, deliver formulas with an explicit note that recalculation is required in Excel/LibreOffice. Do not manufacture cached results.

## Older and Apple formats

For `.xls`, an installed LibreOffice can convert with `soffice --headless --convert-to xlsx --outdir converted input.xls` after creating a separate output directory. `xlrd` can read old `.xls` values when conversion is unavailable, but is not an `.xlsx` editor. Confirm formulas, dates, and formatting after conversion; do not just rename the extension.

For Pages, Keynote, or Numbers, ask the user to export on their Mac: Pages to DOCX or PDF, Keynote to PPTX or PDF, Numbers to XLSX or PDF. XLSX is appropriate for spreadsheet editing and analysis; PDF is a display snapshot and may omit formula information.

## Verify before delivery

Reopen with formulas enabled and inspect changed cells plus important untouched sheets, formulas, comments, merges, and formats. Compare expected totals where they can be computed independently. Check formula errors and cached results after recalculation, if performed. Run `python3 -m zipfile -t output.xlsx` and parse every `.xml`/`.rels` entry; these checks detect package damage, not incorrect formulas or poor appearance.

If available, open or render a copy using a spreadsheet application and inspect widths, clipped values, number formats, and print areas. PDF export can help check print layout but may span many pages. State when rendering or recalculation was unavailable. Return the output path and any remaining formula/fidelity limitations.

API references, when needed: [openpyxl tutorial](https://openpyxl.readthedocs.io/en/stable/tutorial.html), [creation and styling](https://openpyxl.readthedocs.io/en/stable/usage.html), [comment limitations](https://openpyxl.readthedocs.io/en/stable/comments.html), [uv script environments](https://docs.astral.sh/uv/guides/scripts/).
