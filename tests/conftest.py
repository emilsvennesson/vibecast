"""Shared test fixtures for vibecast tests."""

from __future__ import annotations

import datetime
import ssl
import struct
import tempfile
from pathlib import Path

import pytest
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa
from cryptography.x509 import (
    CertificateBuilder,
    Name,
    NameAttribute,
    random_serial_number,
)
from cryptography.x509.oid import NameOID

from vibecast._certificate import CertificateBundle
from vibecast._proto.cast_channel_pb2 import CastMessage

# ---------------------------------------------------------------------------
# Crypto helpers
# ---------------------------------------------------------------------------


def generate_test_key() -> rsa.RSAPrivateKey:
    """Generate a 2048-bit RSA key for testing."""
    return rsa.generate_private_key(public_exponent=65537, key_size=2048)


def make_test_cert(
    key: rsa.RSAPrivateKey,
    cn: str,
) -> tuple[bytes, bytes]:
    """Return (PEM, DER) for a self-signed certificate."""
    subject = issuer = Name([NameAttribute(NameOID.COMMON_NAME, cn)])
    cert = (
        CertificateBuilder()
        .subject_name(subject)
        .issuer_name(issuer)
        .public_key(key.public_key())
        .serial_number(random_serial_number())
        .not_valid_before(datetime.datetime.now(datetime.UTC))
        .not_valid_after(
            datetime.datetime.now(datetime.UTC) + datetime.timedelta(days=1)
        )
        .sign(key, hashes.SHA256())
    )
    pem = cert.public_bytes(serialization.Encoding.PEM)
    der = cert.public_bytes(serialization.Encoding.DER)
    return pem, der


# ---------------------------------------------------------------------------
# CertificateBundle fixture
# ---------------------------------------------------------------------------


@pytest.fixture
def bundle() -> CertificateBundle:
    """Create a :class:`CertificateBundle` with real test crypto material."""
    peer_key = generate_test_key()
    peer_pem, peer_der = make_test_cert(peer_key, "TestPeer")
    peer_key_pem = peer_key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.TraditionalOpenSSL,
        serialization.NoEncryption(),
    )

    device_key = generate_test_key()
    _device_pem, device_der = make_test_cert(device_key, "TestDevice")

    ica_key = generate_test_key()
    _ica_pem, ica_der = make_test_cert(ica_key, "TestICA")

    sig_sha1 = device_key.sign(
        peer_der,
        padding.PKCS1v15(),
        hashes.SHA1(),  # noqa: S303
    )

    return CertificateBundle(
        peer_cert_pem=peer_pem,
        peer_key_pem=peer_key_pem,
        device_cert_der=device_der,
        intermediate_certs_der=[ica_der],
        signature_sha1=sig_sha1,
        peer_cert_der=peer_der,
    )


# ---------------------------------------------------------------------------
# SSL context fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def ssl_server_context(bundle: CertificateBundle) -> ssl.SSLContext:
    """Build a TLS server context from the test *bundle*."""
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2

    cert_path: Path | None = None
    key_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(suffix=".pem", delete=False) as f:
            _ = f.write(bundle.peer_cert_pem)
            cert_path = Path(f.name)
        with tempfile.NamedTemporaryFile(suffix=".pem", delete=False) as f:
            _ = f.write(bundle.peer_key_pem)
            key_path = Path(f.name)
        ctx.load_cert_chain(certfile=str(cert_path), keyfile=str(key_path))
    finally:
        if cert_path is not None:
            cert_path.unlink(missing_ok=True)
        if key_path is not None:
            key_path.unlink(missing_ok=True)
    return ctx


@pytest.fixture
def ssl_client_context() -> ssl.SSLContext:
    """Build a TLS client context that accepts self-signed certs."""
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


# ---------------------------------------------------------------------------
# CastMessage helpers
# ---------------------------------------------------------------------------


def make_cast_message(
    *,
    source: str = "sender-0",
    destination: str = "receiver-0",
    namespace: str = "urn:x-cast:com.google.cast.tp.heartbeat",
    payload_utf8: str | None = None,
    payload_binary: bytes | None = None,
) -> CastMessage:
    """Build a CastMessage for testing."""
    msg = CastMessage()
    msg.protocol_version = CastMessage.CASTV2_1_0
    msg.source_id = source
    msg.destination_id = destination
    msg.namespace = namespace
    if payload_binary is not None:
        msg.payload_type = CastMessage.BINARY
        msg.payload_binary = payload_binary
    else:
        msg.payload_type = CastMessage.STRING
        msg.payload_utf8 = payload_utf8 or ""
    return msg


def frame_message(msg: CastMessage) -> bytes:
    """Serialize a CastMessage into its wire format (length prefix + protobuf)."""
    payload = msg.SerializeToString()
    return struct.pack(">I", len(payload)) + payload
