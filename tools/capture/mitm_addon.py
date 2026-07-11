"""mitmproxy addon that records each HTTP flow to a JSON Lines file.

Loaded in-process by ``capture.py`` (mitmproxy runs as a library via
``DumpMaster``), so the output path is passed to the constructor rather than
read from the environment. Each completed request/response pair becomes one
``\\n``-terminated JSON line, matching the shape of the Cast log so the two
streams (``cast.jsonl`` + ``http.jsonl``) can be merged and ordered by ``ts``.

This deliberately records sensitive data (auth flows, license/manifest
requests). Its output is git-ignored and never touches any logging framework.
"""

from __future__ import annotations

import json
from datetime import UTC, datetime
from itertools import count
from pathlib import Path
from typing import Any

#: Response/request bodies larger than this are summarized, not stored inline.
_MAX_BODY_SIZE = 256 * 1024

_TEXTUAL_HINTS = ("json", "xml", "javascript", "x-www-form-urlencoded")


def _is_textual(content_type: str) -> bool:
    base = content_type.split(";")[0].strip().lower()
    if base.startswith("text/"):
        return True
    if base in ("application/dash+xml", "application/vnd.apple.mpegurl"):
        return True
    return any(hint in base for hint in _TEXTUAL_HINTS)


def _decode_body(raw: bytes | None, content_type: str) -> Any:
    if not raw:
        return None
    if not _is_textual(content_type):
        return {"_binary": True, "content_type": content_type, "size": len(raw)}
    if len(raw) > _MAX_BODY_SIZE:
        return {
            "_truncated": True,
            "size": len(raw),
            "content_type": content_type,
            "preview": raw[:2048].decode("utf-8", errors="replace"),
        }
    text = raw.decode("utf-8", errors="replace")
    if "json" in content_type.split(";")[0].strip().lower():
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            pass
    return text


class HttpJsonl:
    """Writes each completed HTTP flow to ``http.jsonl``."""

    def __init__(self, path: Path) -> None:
        # Line-buffered append so the file stays readable during a session.
        self._file = Path(path).open("a", buffering=1, encoding="utf-8")  # noqa: SIM115
        self._seq = count(1)

    def done(self) -> None:
        try:
            self._file.flush()
            self._file.close()
        except Exception:  # noqa: BLE001 — best-effort close on shutdown
            pass

    def response(self, flow: Any) -> None:
        response = getattr(flow, "response", None)
        if response is None:
            return

        request = flow.request
        req_ct = request.headers.get("content-type", "")
        resp_ct = response.headers.get("content-type", "")
        now = datetime.now(tz=UTC)

        entry = {
            "seq": next(self._seq),
            "ts": now.isoformat(),
            "ts_unix_ms": int(now.timestamp() * 1000),
            "layer": "http",
            "method": request.method,
            "url": request.pretty_url,
            "host": request.pretty_host,
            "status": response.status_code,
            "request_headers": dict(request.headers),
            "request_body": _decode_body(request.get_content(), req_ct),
            "response_headers": dict(response.headers),
            "response_body": _decode_body(response.get_content(), resp_ct),
        }
        self._file.write(json.dumps(entry, ensure_ascii=False, default=str) + "\n")

        path = request.path.split("?", 1)[0]
        print(
            f"  http  {request.method:<6} [{response.status_code}]  {request.pretty_host}{path}",
            flush=True,
        )
