---
name: web-testing
description: Verify and debug websites and frontends, including elements, interaction logic, visual appearance, console errors, and network activity; choose headless Playwright scripts or interactive Playwright MCP as appropriate.
---

# Web testing

Choose the approach from the task rather than asking the user whether to use a visible or headless browser. Verify the actual page behavior and report the evidence and any limitations.

## Check the sandbox first

If the shell tool has a `sandboxRequest` parameter, the sandbox is enabled. Browser scripts cannot launch Chrome in this sandbox, even after requesting network access or writable paths. Do not retry browser launch variations; use Playwright MCP instead.

If no browser MCP tools are configured, explain the two options: configure Playwright MCP as described in the MCP section of `docs/usage.md`, or start a session with `canter --yolo` to disable the sandbox for headless scripts. Do not silently change the sandbox or MCP configuration.

## Choose an approach

- For page verification, screenshots, console or network inspection, or a known repeatable flow, write and run a headless Playwright script. Inspect its output and screenshots afterward.
- For exploratory testing, step-by-step interaction, repeatedly trying different actions, or when the user wants to watch the browser, use Playwright MCP.

Use MCP when the sandbox is enabled regardless of whether the requested flow is fixed. Otherwise, choose between these approaches yourself.

## Use Playwright MCP

Find browser tools with `mcp_search`, for example by searching for `browser_navigate` or `browser_snapshot`. Do not assume the MCP server is named `playwright`; tool names commonly include `browser_navigate`, `browser_snapshot`, `browser_click`, `browser_type`, and `browser_take_screenshot`.

Prefer `browser_snapshot` to understand the accessibility structure and act on elements by their roles, labels, or text. Use screenshots when appearance matters; only a model with image input can see them. Check `browser_console_messages` and `browser_network_requests` when debugging errors or requests. For a fixed sequence, `browser_run_code_unsafe` can run the Playwright steps in one call, but still inspect and assert the result.

## Write a headless script

Check the available runtimes and browser first:

```sh
command -v uv python3 node
```

On macOS, also check for `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`. Put scripts, screenshots, console logs, and other output in the session temporary directory. Use an isolated environment such as `uv run --no-project --with playwright python <script>`, or a Node project inside that temporary directory; do not install packages globally.

Prefer the installed Chrome with Playwright's `channel="chrome"` and `headless=True`. Only if Chrome is unavailable, tell the user before downloading Chromium because it is hundreds of megabytes. Do not use the user's normal Chrome profile.

A script should collect console errors and uncaught page errors, navigate and act using discovered selectors, save a screenshot, and print concrete results. The browser setup can be as small as:

```python
browser = playwright.chromium.launch(channel="chrome", headless=True)
page = browser.new_page()
page.on("pageerror", lambda error: errors.append(str(error)))
```

Read saved screenshots with the image-reading tool rather than treating a successful screenshot call as visual verification.

## Test local pages

Check whether the development server is already running and reachable. If not, start it with a background shell command, retain its process or command ID, and wait until the port accepts connections before testing. Stop only the server you started, using that recorded process or command ID; never kill processes by project or browser name.

For a static page, read its HTML first to identify stable selectors and expected content. For a dynamic application, inspect the loaded page before choosing selectors.

## Verify the result

Wait for page loading to finish: wait for network idle when appropriate, or for the specific target element when the page has continuing network activity. Inspect the DOM, accessibility snapshot, or HTML before interacting. Prefer stable roles, labels, and text over brittle coordinates or generated CSS classes.

Assert specific expected text, element counts, states, and URLs rather than only checking that no exception occurred. Capture relevant console errors and failed network requests. When reporting, distinguish assertions and DOM observations from judgments based only on a screenshot, and include console errors even when the main assertions pass.
