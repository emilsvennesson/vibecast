"""Shared TLS context helpers for receiver services."""

from __future__ import annotations

import ssl
import tempfile
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from vibecast._security.certificate import CertificateBundle


def build_server_ssl_context(bundle: CertificateBundle) -> ssl.SSLContext:
    """Create a server-side TLS context for one certificate bundle."""
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    load_cert_chain(context, bundle)
    return context


def load_cert_chain(context: ssl.SSLContext, bundle: CertificateBundle) -> None:
    """Load certificate chain from *bundle* into *context*."""
    cert_path: Path | None = None
    key_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(suffix=".pem", delete=False) as cert_file:
            _ = cert_file.write(bundle.peer_cert_pem)
            cert_path = Path(cert_file.name)

        with tempfile.NamedTemporaryFile(suffix=".pem", delete=False) as key_file:
            _ = key_file.write(bundle.peer_key_pem)
            key_path = Path(key_file.name)

        context.load_cert_chain(certfile=str(cert_path), keyfile=str(key_path))
    finally:
        if cert_path is not None:
            cert_path.unlink(missing_ok=True)
        if key_path is not None:
            key_path.unlink(missing_ok=True)


__all__ = ["build_server_ssl_context", "load_cert_chain"]
