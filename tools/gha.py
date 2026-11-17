"""GitHub Actions output of the image tools (image-smoke.py, release-promote.py).

Failures are printed as workflow commands (`::error title=…::…`), which
GitHub turns into annotations that the public API shows without a login, and
results are appended to the job summary (`$GITHUB_STEP_SUMMARY`) when it is
set. Outside GitHub Actions the same lines are plain, harmless output.
Standard library only.
"""

from __future__ import annotations

import os
import secrets

# Longest annotation message (GitHub truncates long ones anyway).
MAX_MESSAGE = 4000


def escape_data(text: str) -> str:
    """The message part of a workflow command: `%`, CR and LF escaped."""
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def escape_property(text: str) -> str:
    """A property value (e.g. `title=`): like data, plus `:` and `,`."""
    return escape_data(text).replace(":", "%3A").replace(",", "%2C")


def error_line(title: str, message: str) -> str:
    """`::error title=<title>::<message>`, both escaped."""
    if len(message) > MAX_MESSAGE:
        message = "…" + message[-MAX_MESSAGE:]
    return f"::error title={escape_property(title)}::{escape_data(message)}"


def error(title: str, message: str) -> None:
    print(error_line(title, message), flush=True)


def summary_table(heading: str, rows: list[tuple[str, bool, str]]) -> str:
    """A markdown table of (check, passed, detail) rows."""
    def cell(text: str) -> str:
        text = text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
        return text.replace("|", "\\|").replace("\r", " ").replace("\n", " ")

    lines = [f"### {heading}", "", "| | check | detail |", "|---|---|---|"]
    lines += [f"| {'✅' if ok else '❌'} | {cell(name)} | {cell(detail)} |" for name, ok, detail in rows]
    return "\n".join(lines) + "\n"


def untrusted_block(heading: str, text: str, token: str | None = None) -> str:
    """`text` (e.g. a container log) framed by `::stop-commands::`, so no
    line of it can be read as a workflow command; returns the lines to print."""
    token = token or secrets.token_hex(16)
    return f"{heading}\n::stop-commands::{token}\n{text}\n::{token}::"


def append_summary(markdown: str) -> None:
    """Append to the job summary when running in GitHub Actions."""
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if not path:
        return
    try:
        with open(path, "a", encoding="utf-8") as summary:
            summary.write(markdown + "\n")
    except OSError as err:
        print(f"cannot write the job summary: {err}")
