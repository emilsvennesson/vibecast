"""Device authentication for the Google Cast protocol.

Provides helpers to build the binary ``DeviceAuthMessage`` response sent on
the ``urn:x-cast:com.google.cast.tp.deviceauth`` namespace, and to fetch the
Cast CRL (Certificate Revocation List) from Google's servers.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import httpx

from vibecast._proto.cast_channel_pb2 import (
    AuthResponse,
    DeviceAuthMessage,
    HashAlgorithm,
    SignatureAlgorithm,
)

if TYPE_CHECKING:
    from httpx import AsyncClient

    from vibecast._certificate import CertificateBundle

#: Google's public endpoint serving the Cast device CRL.
CRL_URL = "https://clients3.google.com/cast/chromecast/device/crl"


def build_auth_response(
    bundle: CertificateBundle,
    *,
    crl: bytes | None = None,
) -> bytes:
    """Build a serialized ``DeviceAuthMessage`` containing an ``AuthResponse``.

    The hash algorithm is selected automatically: SHA-1 when the bundle was
    loaded from a ``sig_sha1`` manifest field, SHA-256 when loaded from the
    legacy ``sig`` field.  No sender nonce is incorporated (static signature
    mode).

    If *crl* is provided it takes precedence; otherwise the CRL embedded in
    *bundle* (if any) is used.

    Returns raw protobuf bytes ready to be sent as a ``BINARY`` payload on the
    ``urn:x-cast:com.google.cast.tp.deviceauth`` namespace.
    """
    hash_algorithm = HashAlgorithm.SHA1
    if not bundle.signature_is_sha1:
        hash_algorithm = HashAlgorithm.SHA256

    auth_response = AuthResponse(
        signature=bundle.signature_sha1,
        client_auth_certificate=bundle.device_cert_der,
        intermediate_certificate=bundle.intermediate_certs_der,
        hash_algorithm=hash_algorithm,
        signature_algorithm=SignatureAlgorithm.RSASSA_PKCS1v15,
    )

    effective_crl = bundle.crl if crl is None else crl
    if effective_crl is not None:
        auth_response.crl = effective_crl

    message = DeviceAuthMessage(response=auth_response)
    return message.SerializeToString()


async def fetch_crl(
    url: str = CRL_URL,
    *,
    client: AsyncClient | None = None,
) -> bytes:
    """Download the Cast device CRL from Google.

    The CRL is an opaque protobuf-encoded binary blob included in the
    ``AuthResponse.crl`` field.  It is typically fetched once at startup
    and reused for all subsequent auth challenges.

    Raises:
        httpx.HTTPError: On network or HTTP errors.
    """
    if client is not None:
        return await _fetch_crl_with_client(client, url)

    async with httpx.AsyncClient(timeout=15.0, follow_redirects=True) as session:
        return await _fetch_crl_with_client(session, url)


async def _fetch_crl_with_client(client: AsyncClient, url: str) -> bytes:
    response = await client.get(url)
    _ = response.raise_for_status()
    return response.content
