"""Device authentication for the Google Cast protocol.

Provides helpers to build the binary ``DeviceAuthMessage`` response sent on
the ``urn:x-cast:com.google.cast.tp.deviceauth`` namespace, and to fetch the
Cast CRL (Certificate Revocation List) from Google's servers.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import aiohttp

from castvibe._proto.cast_channel_pb2 import (
    AuthResponse,
    DeviceAuthMessage,
    HashAlgorithm,
    SignatureAlgorithm,
)

if TYPE_CHECKING:
    from castvibe._certificate import CertificateBundle

#: Google's public endpoint serving the Cast device CRL.
CRL_URL = "https://clients3.google.com/cast/chromecast/device/crl"


def build_auth_response(
    bundle: CertificateBundle,
    *,
    crl: bytes | None = None,
) -> bytes:
    """Build a serialized ``DeviceAuthMessage`` containing an ``AuthResponse``.

    The response uses the static pre-computed SHA-1 signature from *bundle*.
    No sender nonce is incorporated (static signature mode).

    Returns raw protobuf bytes ready to be sent as a ``BINARY`` payload on the
    ``urn:x-cast:com.google.cast.tp.deviceauth`` namespace.
    """
    auth_response = AuthResponse(
        signature=bundle.signature_sha1,
        client_auth_certificate=bundle.device_cert_der,
        intermediate_certificate=bundle.intermediate_certs_der,
        hash_algorithm=HashAlgorithm.SHA1,
        signature_algorithm=SignatureAlgorithm.RSASSA_PKCS1v15,
    )

    if crl is not None:
        auth_response.crl = crl

    message = DeviceAuthMessage(response=auth_response)
    return message.SerializeToString()


async def fetch_crl(url: str = CRL_URL) -> bytes:
    """Download the Cast device CRL from Google.

    The CRL is an opaque protobuf-encoded binary blob included in the
    ``AuthResponse.crl`` field.  It is typically fetched once at startup
    and reused for all subsequent auth challenges.

    Raises:
        aiohttp.ClientError: On network or HTTP errors.
    """
    async with aiohttp.ClientSession() as session, session.get(url) as resp:
        resp.raise_for_status()
        return await resp.read()
