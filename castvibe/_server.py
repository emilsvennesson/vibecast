"""Asyncio TLS server for the Google Cast protocol.

Listens on a TCP port with TLS (self-signed peer certificate) and spawns
a :class:`~castvibe._connection.Connection` for each accepted sender.
"""

from __future__ import annotations

import asyncio
import ssl
import tempfile
from pathlib import Path
from typing import TYPE_CHECKING

from castvibe._connection import Connection
from castvibe._log import get_logger

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from castvibe._certificate import CertificateBundle
    from castvibe._proto.cast_channel_pb2 import CastMessage

log = get_logger("server")


class CastServer:
    """Asyncio TLS server accepting Cast sender connections.

    Parameters
    ----------
    bundle:
        Certificate material used for TLS and device authentication.
    host:
        Interface to bind to (default ``""`` = all interfaces).
    port:
        TCP port to listen on (default ``8009``, the Cast standard port).
    on_message:
        Optional async callback forwarded to each :class:`Connection`.
        Invoked for every message not handled locally (i.e. not
        deviceauth or heartbeat).
    on_connection:
        Optional async callback ``(connection) -> None`` invoked when a
        new sender connects (after the TLS handshake completes).
    on_disconnect:
        Optional async callback ``(connection) -> None`` invoked when a
        sender disconnects.
    """

    __slots__ = (
        "_bundle",
        "_connections",
        "_crl",
        "_host",
        "_on_connection",
        "_on_disconnect",
        "_on_message",
        "_port",
        "_server",
        "_ssl_ctx",
        "_tasks",
    )

    def __init__(
        self,
        bundle: CertificateBundle,
        *,
        host: str = "",
        port: int = 8009,
        crl: bytes | None = None,
        on_message: Callable[[Connection, CastMessage], Awaitable[None]] | None = None,
        on_connection: Callable[[Connection], Awaitable[None]] | None = None,
        on_disconnect: Callable[[Connection], Awaitable[None]] | None = None,
    ) -> None:
        self._bundle = bundle
        self._host = host
        self._port = port
        self._crl = crl
        self._on_message = on_message
        self._on_connection = on_connection
        self._on_disconnect = on_disconnect

        self._ssl_ctx = _build_ssl_context(bundle)
        self._server: asyncio.Server | None = None
        self._tasks: set[asyncio.Task[None]] = set()
        self._connections: set[Connection] = set()

    # ------------------------------------------------------------------
    # Properties
    # ------------------------------------------------------------------

    @property
    def is_serving(self) -> bool:
        """Whether the server is currently listening for connections."""
        return self._server is not None and self._server.is_serving()

    @property
    def serving_port(self) -> int | None:
        """The TCP port the server is bound to, or *None* if not started.

        Useful when the server was created with ``port=0`` (OS-assigned).
        """
        if self._server is None:
            return None
        sockets = self._server.sockets
        return int(sockets[0].getsockname()[1]) if sockets else None

    @property
    def active_connections(self) -> int:
        """Number of currently active client connections."""
        return len(self._connections)

    @property
    def crl(self) -> bytes | None:
        """CRL bytes included in device auth responses."""
        return self._crl

    @crl.setter
    def crl(self, value: bytes | None) -> None:
        self._crl = value

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def start(self) -> None:
        """Start listening for TLS connections.

        Raises :class:`OSError` if the port is already in use.
        """
        self._server = await asyncio.start_server(
            self._accept,
            host=self._host,
            port=self._port,
            ssl=self._ssl_ctx,
        )
        addrs = [sock.getsockname() for sock in self._server.sockets]
        log.info("cast server listening on %s", addrs)

    async def stop(self) -> None:
        """Stop the server and close all active connections."""
        if self._server is None:
            return

        self._server.close()
        log.info("cast server stopped accepting connections")

        # Cancel connection tasks *before* wait_closed().  In Python 3.12+
        # wait_closed() waits for all active connections to finish, so we
        # must tear them down first to avoid a deadlock.
        for task in self._tasks:
            _ = task.cancel()
        if self._tasks:
            _ = await asyncio.gather(*self._tasks, return_exceptions=True)
        self._tasks.clear()
        self._connections.clear()

        await self._server.wait_closed()
        self._server = None
        log.info("cast server shut down")

    # ------------------------------------------------------------------
    # Internal
    # ------------------------------------------------------------------

    async def _accept(
        self,
        reader: asyncio.StreamReader,
        writer: asyncio.StreamWriter,
    ) -> None:
        """Handle a newly accepted TLS connection."""
        conn = Connection(
            reader,
            writer,
            self._bundle,
            crl=self._crl,
            on_message=self._on_message,
            on_disconnect=self._connection_closed,
        )
        self._connections.add(conn)

        if self._on_connection is not None:
            await self._on_connection(conn)

        task = asyncio.current_task()
        if task is not None:
            self._tasks.add(task)
            task.add_done_callback(self._tasks.discard)

        await conn.handle()

    async def _connection_closed(self, conn: Connection) -> None:
        """Clean up when a connection drops."""
        self._connections.discard(conn)
        if self._on_disconnect is not None:
            await self._on_disconnect(conn)


# ------------------------------------------------------------------
# SSL helpers
# ------------------------------------------------------------------


def _build_ssl_context(bundle: CertificateBundle) -> ssl.SSLContext:
    """Create a server-side TLS context from *bundle*.

    The context uses the peer certificate and private key from the bundle.
    Client certificates are not requested (Cast senders do not present them).
    Minimum TLS version is 1.2, matching real Chromecast behaviour.
    """
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2

    # ssl.SSLContext.load_cert_chain() requires file paths — there is no
    # in-memory API in the stdlib.  Write PEM bytes to temporary files.
    cert_path: Path | None = None
    key_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(suffix=".pem", delete=False) as cert_f:
            _ = cert_f.write(bundle.peer_cert_pem)
            cert_path = Path(cert_f.name)

        with tempfile.NamedTemporaryFile(suffix=".pem", delete=False) as key_f:
            _ = key_f.write(bundle.peer_key_pem)
            key_path = Path(key_f.name)

        ctx.load_cert_chain(certfile=str(cert_path), keyfile=str(key_path))
    finally:
        if cert_path is not None:
            cert_path.unlink(missing_ok=True)
        if key_path is not None:
            key_path.unlink(missing_ok=True)

    return ctx
