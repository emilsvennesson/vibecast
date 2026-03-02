"""Tests for device authentication response building."""

from __future__ import annotations

import datetime

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


def _generate_key() -> rsa.RSAPrivateKey:
    return rsa.generate_private_key(public_exponent=65537, key_size=2048)


def _self_signed_cert_bytes(
    key: rsa.RSAPrivateKey,
    cn: str,
) -> tuple[bytes, bytes]:
    """Return (pem, der) for a self-signed certificate."""
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
    msg = DeviceAuthMessage()
    _ = msg.ParseFromString(raw)
    return msg


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
    _device_pem, device_der = _self_signed_cert_bytes(device_key, "Device")

    ica_key = _generate_key()
    _ica_pem, ica_der = _self_signed_cert_bytes(ica_key, "ICA")

    sig_sha1 = device_key.sign(
        peer_der,
        padding.PKCS1v15(),
        hashes.SHA1(),  # noqa: S303
    )
    sig_sha256 = device_key.sign(
        peer_der,
        padding.PKCS1v15(),
        hashes.SHA256(),
    )
    now = datetime.datetime.now(datetime.UTC)

    return CertificateBundle(
        peer_cert_pem=peer_pem,
        peer_key_pem=peer_key_pem,
        device_cert_der=device_der,
        intermediate_certs_der=[ica_der],
        signature_sha1=sig_sha1,
        signature_sha256=sig_sha256,
        peer_cert_der=peer_der,
        not_valid_before=now - datetime.timedelta(minutes=1),
        not_valid_after=now + datetime.timedelta(days=1),
    )


class TestBuildAuthResponse:
    def test_returns_valid_protobuf(self, bundle: CertificateBundle) -> None:
        msg = _parse_auth_response(
            build_auth_response(bundle, hash_algorithm=HashAlgorithm.SHA1)
        )

        assert msg.HasField("response")
        assert not msg.HasField("challenge")
        assert not msg.HasField("error")

    def test_signature_matches_bundle_sha1(self, bundle: CertificateBundle) -> None:
        msg = _parse_auth_response(
            build_auth_response(bundle, hash_algorithm=HashAlgorithm.SHA1)
        )

        assert msg.response.signature == bundle.signature_sha1

    def test_signature_matches_bundle_sha256(self, bundle: CertificateBundle) -> None:
        msg = _parse_auth_response(
            build_auth_response(bundle, hash_algorithm=HashAlgorithm.SHA256)
        )

        assert msg.response.signature == bundle.signature_sha256

    def test_client_auth_certificate(self, bundle: CertificateBundle) -> None:
        msg = _parse_auth_response(
            build_auth_response(bundle, hash_algorithm=HashAlgorithm.SHA1)
        )

        assert msg.response.client_auth_certificate == bundle.device_cert_der

    def test_intermediate_certificates(self, bundle: CertificateBundle) -> None:
        msg = _parse_auth_response(
            build_auth_response(bundle, hash_algorithm=HashAlgorithm.SHA1)
        )

        assert (
            list(msg.response.intermediate_certificate) == bundle.intermediate_certs_der
        )

    def test_hash_algorithm_echoed(self, bundle: CertificateBundle) -> None:
        msg = _parse_auth_response(
            build_auth_response(bundle, hash_algorithm=HashAlgorithm.SHA256)
        )

        assert msg.response.hash_algorithm == HashAlgorithm.SHA256

    def test_signature_algorithm_pkcs1v15(self, bundle: CertificateBundle) -> None:
        msg = _parse_auth_response(
            build_auth_response(bundle, hash_algorithm=HashAlgorithm.SHA1)
        )

        assert msg.response.signature_algorithm == SignatureAlgorithm.RSASSA_PKCS1v15

    def test_no_sender_nonce(self, bundle: CertificateBundle) -> None:
        msg = _parse_auth_response(
            build_auth_response(bundle, hash_algorithm=HashAlgorithm.SHA1)
        )

        assert not msg.response.HasField("sender_nonce")

    def test_crl_included_when_provided(self, bundle: CertificateBundle) -> None:
        crl_data = b"\x0a\x0b\x0c\x0d"
        msg = _parse_auth_response(
            build_auth_response(
                bundle,
                hash_algorithm=HashAlgorithm.SHA1,
                crl=crl_data,
            )
        )

        assert msg.response.crl == crl_data

    def test_crl_omitted_when_none(self, bundle: CertificateBundle) -> None:
        msg = _parse_auth_response(
            build_auth_response(
                bundle,
                hash_algorithm=HashAlgorithm.SHA1,
                crl=None,
            )
        )

        assert not msg.response.HasField("crl")

    def test_bundle_crl_used_when_override_missing(
        self, bundle: CertificateBundle
    ) -> None:
        bundle.crl = b"\x9a\x9b"
        msg = _parse_auth_response(
            build_auth_response(bundle, hash_algorithm=HashAlgorithm.SHA1)
        )

        assert msg.response.crl == b"\x9a\x9b"

    def test_multiple_intermediate_certs(self) -> None:
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

        sig_sha1 = device_key.sign(
            peer_der,
            padding.PKCS1v15(),
            hashes.SHA1(),  # noqa: S303
        )
        sig_sha256 = device_key.sign(
            peer_der,
            padding.PKCS1v15(),
            hashes.SHA256(),
        )
        now = datetime.datetime.now(datetime.UTC)

        multi_bundle = CertificateBundle(
            peer_cert_pem=peer_pem,
            peer_key_pem=peer_key_pem,
            device_cert_der=device_der,
            intermediate_certs_der=[ica1_der, ica2_der],
            signature_sha1=sig_sha1,
            signature_sha256=sig_sha256,
            peer_cert_der=peer_der,
            not_valid_before=now - datetime.timedelta(minutes=1),
            not_valid_after=now + datetime.timedelta(days=1),
        )

        msg = _parse_auth_response(
            build_auth_response(multi_bundle, hash_algorithm=HashAlgorithm.SHA1)
        )

        assert len(msg.response.intermediate_certificate) == 2
        assert msg.response.intermediate_certificate[0] == ica1_der
        assert msg.response.intermediate_certificate[1] == ica2_der

    def test_unsupported_hash_algorithm_raises(self, bundle: CertificateBundle) -> None:
        with pytest.raises(ValueError, match="Unsupported Cast auth hash algorithm"):
            _ = build_auth_response(bundle, hash_algorithm=99)


class TestFetchCrl:
    async def test_returns_bytes(self) -> None:
        expected = b"\x0a\xd0\x10"

        def handler(_request: httpx.Request) -> httpx.Response:
            return httpx.Response(200, content=expected)

        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
            result = await fetch_crl(client=client)

        assert result == expected

    async def test_uses_default_url(self) -> None:
        captured_urls: list[str] = []

        def handler(request: httpx.Request) -> httpx.Response:
            captured_urls.append(str(request.url))
            return httpx.Response(200, content=b"data")

        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
            _ = await fetch_crl(client=client)

        assert captured_urls == [CRL_URL]

    async def test_custom_url(self) -> None:
        captured_urls: list[str] = []
        custom = "https://example.com/custom-crl"

        def handler(request: httpx.Request) -> httpx.Response:
            captured_urls.append(str(request.url))
            return httpx.Response(200, content=b"data")

        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
            _ = await fetch_crl(custom, client=client)

        assert captured_urls == [custom]
