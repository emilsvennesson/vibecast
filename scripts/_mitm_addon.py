"""mitmproxy addon that logs HTTP flows as JSON Lines.

Loaded by mitmdump via:
    mitmdump --mode wireguard -s scripts/_mitm_addon.py

Reads the output log path from the ``CAPTURE_LOG_PATH`` environment variable.
Each completed HTTP request/response pair is written as a single JSON line to
the shared capture log file (the same file the Cast proxy writes to).
"""

from __future__ import annotations

import contextlib
import json
import os
import sys
from datetime import UTC, datetime
from itertools import count
from pathlib import Path
from typing import Any

#: Maximum response body size to include inline (64 KiB).
_MAX_BODY_SIZE = 64 * 1024

#: Content types treated as "text" (bodies included inline).
_TEXT_CONTENT_TYPES = frozenset(
    {
        "application/json",
        "application/xml",
        "application/x-www-form-urlencoded",
        "text/html",
        "text/plain",
        "text/xml",
    }
)


def _is_text_content(content_type: str) -> bool:
    """Return True if *content_type* looks like a textual format."""
    base = content_type.split(";")[0].strip().lower()
    if base in _TEXT_CONTENT_TYPES:
        return True
    return base.startswith("text/") or "+json" in base or "+xml" in base


def _decode_body(raw: bytes | None, content_type: str) -> Any:
    """Decode a response body for inclusion in the log.

    JSON bodies are parsed so they appear as nested objects (not escaped
    strings) in the log.  Large or binary bodies are replaced with a
    size/type summary.
    """
    if raw is None or len(raw) == 0:
        return None

    if not _is_text_content(content_type):
        return {"_binary": True, "content_type": content_type, "size": len(raw)}

    if len(raw) > _MAX_BODY_SIZE:
        return {
            "_truncated": True,
            "size": len(raw),
            "content_type": content_type,
            "preview": raw[:2048].decode("utf-8", errors="replace"),
        }

    text = raw.decode("utf-8", errors="replace")

    # Try parsing as JSON so the LLM gets structured data.
    base = content_type.split(";")[0].strip().lower()
    if "json" in base:
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            pass

    return text


def _capture_url(flow: Any) -> str:
    """Return a capture URL that prefers the logical request hostname."""
    request = flow.request
    pretty_url = getattr(request, "pretty_url", "")
    if isinstance(pretty_url, str) and pretty_url:
        return pretty_url
    return request.url


class CaptureAddon:
    """Writes each HTTP flow to the shared JSON Lines capture log."""

    def __init__(self) -> None:
        log_path = os.environ.get("CAPTURE_LOG_PATH")
        if not log_path:
            print(
                "[_mitm_addon] CAPTURE_LOG_PATH not set; "
                "HTTP flows will NOT be captured.",
                file=sys.stderr,
            )
            self._file = None
        else:
            # Open in append + line-buffered mode so writes interleave safely
            # with the Cast proxy process writing to the same file.
            self._file = Path(log_path).open("a", buffering=1)  # noqa: SIM115
            print(
                f"[_mitm_addon] logging HTTP flows to {log_path}",
                file=sys.stderr,
            )
        self._seq = count(1)

    def done(self) -> None:
        """Called by mitmproxy when the addon is shutting down."""
        f = self._file
        if f is None:
            return
        with contextlib.suppress(Exception):
            f.flush()
        with contextlib.suppress(Exception):
            f.close()
        self._file = None

    def response(self, flow: Any) -> None:
        """Called when a complete request/response pair is available."""
        if self._file is None:
            return

        response = flow.response
        if response is None:
            return

        req_ct = flow.request.headers.get("content-type", "")
        resp_ct = response.headers.get("content-type", "")

        entry: dict[str, Any] = {
            "ts": datetime.now(tz=UTC).isoformat(),
            "seq": next(self._seq),
            "layer": "http",
            "method": flow.request.method,
            "url": _capture_url(flow),
            "request_headers": dict(flow.request.headers),
            "request_body": _decode_body(flow.request.get_content(), req_ct),
            "status": response.status_code,
            "response_headers": dict(response.headers),
            "response_body": _decode_body(response.get_content(), resp_ct),
        }

        line = json.dumps(entry, ensure_ascii=False, default=str)
        self._file.write(line + "\n")
        self._file.flush()


addons = [CaptureAddon()]
