"""Tests for device authentication response building."""

from __future__ import annotations

import httpx
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

from vibecast._auth import CRL_URL, build_auth_response, fetch_crl
from vibecast._certificate import CertificateBundle
from vibecast._proto.cast_channel_pb2 import (
    DeviceAuthMessage,
    HashAlgorithm,
    SignatureAlgorithm,
)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _generate_key() -> rsa.RSAPrivateKey:
    return rsa.generate_private_key(public_exponent=65537, key_size=2048)


def _self_signed_cert_bytes(
    key: rsa.RSAPrivateKey,
    cn: str,
) -> tuple[bytes, bytes]:
    """Return (pem, der) for a self-signed certificate."""
    import datetime

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


def _parse_auth_response(raw: bytes) -> DeviceAuthMessage:
    """Deserialize raw bytes into a DeviceAuthMessage."""
    msg = DeviceAuthMessage()
    _ = msg.ParseFromString(raw)
    return msg


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def bundle() -> CertificateBundle:
    """Create a CertificateBundle with real dummy certificates."""
    peer_key = _generate_key()
    peer_pem, peer_der = _self_signed_cert_bytes(peer_key, "Peer")
    peer_key_pem = peer_key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.TraditionalOpenSSL,
        serialization.NoEncryption(),
    )

    device_key = _generate_key()
    device_pem, device_der = _self_signed_cert_bytes(device_key, "Device")
    _ = device_pem  # only DER is stored on the bundle

    ica_key = _generate_key()
    _ica_pem, ica_der = _self_signed_cert_bytes(ica_key, "ICA")

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
# build_auth_response tests
# ---------------------------------------------------------------------------


class TestBuildAuthResponse:
    """Tests for build_auth_response()."""

    def test_returns_valid_protobuf(self, bundle: CertificateBundle) -> None:
        """The returned bytes can be deserialized as a DeviceAuthMessage."""
        msg = _parse_auth_response(build_auth_response(bundle))

        assert msg.HasField("response")
        assert not msg.HasField("challenge")
        assert not msg.HasField("error")

    def test_signature_matches_bundle(self, bundle: CertificateBundle) -> None:
        """AuthResponse.signature matches the bundle's pre-computed signature."""
        msg = _parse_auth_response(build_auth_response(bundle))

        assert msg.response.signature == bundle.signature_sha1

    def test_client_auth_certificate(self, bundle: CertificateBundle) -> None:
        """AuthResponse.client_auth_certificate is the device cert DER."""
        msg = _parse_auth_response(build_auth_response(bundle))

        assert msg.response.client_auth_certificate == bundle.device_cert_der

    def test_intermediate_certificates(self, bundle: CertificateBundle) -> None:
        """AuthResponse.intermediate_certificate contains the ICA chain."""
        msg = _parse_auth_response(build_auth_response(bundle))

        assert (
            list(msg.response.intermediate_certificate) == bundle.intermediate_certs_der
        )

    def test_hash_algorithm_sha1(self, bundle: CertificateBundle) -> None:
        """hash_algorithm is explicitly set to SHA1."""
        msg = _parse_auth_response(build_auth_response(bundle))

        assert msg.response.hash_algorithm == HashAlgorithm.SHA1

    def test_hash_algorithm_sha256_for_legacy_signature(
        self, bundle: CertificateBundle
    ) -> None:
        """hash_algorithm is SHA256 when the bundle marks a legacy signature."""
        bundle.signature_is_sha1 = False
        msg = _parse_auth_response(build_auth_response(bundle))

        assert msg.response.hash_algorithm == HashAlgorithm.SHA256

    def test_signature_algorithm_pkcs1v15(self, bundle: CertificateBundle) -> None:
        """signature_algorithm is explicitly set to RSASSA_PKCS1v15."""
        msg = _parse_auth_response(build_auth_response(bundle))

        assert msg.response.signature_algorithm == SignatureAlgorithm.RSASSA_PKCS1v15

    def test_no_sender_nonce(self, bundle: CertificateBundle) -> None:
        """sender_nonce is not set (static signature, no nonce support)."""
        msg = _parse_auth_response(build_auth_response(bundle))

        assert not msg.response.HasField("sender_nonce")

    def test_crl_included_when_provided(self, bundle: CertificateBundle) -> None:
        """CRL bytes are included in the response when passed."""
        crl_data = b"\x0a\x0b\x0c\x0d"
        msg = _parse_auth_response(build_auth_response(bundle, crl=crl_data))

        assert msg.response.crl == crl_data

    def test_crl_omitted_when_none(self, bundle: CertificateBundle) -> None:
        """CRL field is absent when crl=None."""
        msg = _parse_auth_response(build_auth_response(bundle, crl=None))

        assert not msg.response.HasField("crl")

    def test_bundle_crl_used_when_override_missing(
        self, bundle: CertificateBundle
    ) -> None:
        """Bundle-level CRL is used when no explicit ``crl=`` override is given."""
        bundle.crl = b"\x9a\x9b"
        msg = _parse_auth_response(build_auth_response(bundle))

        assert msg.response.crl == b"\x9a\x9b"

    def test_multiple_intermediate_certs(self) -> None:
        """Multiple intermediate certificates are all included."""
        peer_key = _generate_key()
        peer_pem, peer_der = _self_signed_cert_bytes(peer_key, "Peer")
        peer_key_pem = peer_key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.TraditionalOpenSSL,
            serialization.NoEncryption(),
        )

        device_key = _generate_key()
        _device_pem, device_der = _self_signed_cert_bytes(device_key, "Device")

        ica1_key = _generate_key()
        _ica1_pem, ica1_der = _self_signed_cert_bytes(ica1_key, "ICA1")

        ica2_key = _generate_key()
        _ica2_pem, ica2_der = _self_signed_cert_bytes(ica2_key, "ICA2")

        sig = device_key.sign(
            peer_der,
            padding.PKCS1v15(),
            hashes.SHA1(),  # noqa: S303
        )

        multi_bundle = CertificateBundle(
            peer_cert_pem=peer_pem,
            peer_key_pem=peer_key_pem,
            device_cert_der=device_der,
            intermediate_certs_der=[ica1_der, ica2_der],
            signature_sha1=sig,
            peer_cert_der=peer_der,
        )

        msg = _parse_auth_response(build_auth_response(multi_bundle))

        assert len(msg.response.intermediate_certificate) == 2
        assert msg.response.intermediate_certificate[0] == ica1_der
        assert msg.response.intermediate_certificate[1] == ica2_der


class TestFetchCrl:
    """Tests for the async fetch_crl() function."""

    async def test_returns_bytes(self) -> None:
        """fetch_crl returns the raw response body as bytes."""
        expected = b"\x0a\xd0\x10"

        def handler(_request: httpx.Request) -> httpx.Response:
            return httpx.Response(200, content=expected)

        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
            result = await fetch_crl(client=client)

        assert result == expected

    async def test_uses_default_url(self) -> None:
        """fetch_crl uses the CRL_URL constant by default."""
        captured_urls: list[str] = []

        def handler(request: httpx.Request) -> httpx.Response:
            captured_urls.append(str(request.url))
            return httpx.Response(200, content=b"data")

        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
            _ = await fetch_crl(client=client)

        assert captured_urls == [CRL_URL]

    async def test_custom_url(self) -> None:
        """fetch_crl accepts a custom URL."""
        captured_urls: list[str] = []
        custom = "https://example.com/custom-crl"

        def handler(request: httpx.Request) -> httpx.Response:
            captured_urls.append(str(request.url))
            return httpx.Response(200, content=b"data")

        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
            _ = await fetch_crl(custom, client=client)

        assert captured_urls == [custom]
